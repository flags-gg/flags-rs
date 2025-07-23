use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use log::{error, warn};
use log::__private_api::loc;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod cache;
pub mod flag;
mod tests;

#[cfg(feature = "tower-middleware")]
pub mod middleware;

#[cfg(all(test, feature = "tower-middleware"))]
mod middleware_tests;

use crate::cache::{Cache, CacheSystem, MemoryCache};
use crate::flag::{Details, FeatureFlag};

const BASE_URL: &str = "https://api.flags.gg";
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone)]
pub struct Auth {
    pub project_id: String,
    pub agent_id: String,
    pub environment_id: String,
}

pub struct Flag<'a> {
    name: String,
    client: &'a Client,
}

#[derive(Debug, Error)]
pub enum FlagError {
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Missing authentication: {0}")]
    AuthError(String),

    #[error("API error: {0}")]
    ApiError(String),
}

#[derive(Debug)]
struct CircuitState {
    is_open: bool,
    failure_count: u32,
    last_failure: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(rename = "intervalAllowed")]
    interval_allowed: i32,
    flags: Vec<flag::FeatureFlag>,
}

pub struct Client {
    base_url: String,
    http_client: reqwest::Client,
    cache: Arc<RwLock<Box<dyn Cache + Send + Sync>>>,
    max_retries: u32,
    circuit_state: Arc<RwLock<CircuitState>>,
    auth: Option<Auth>,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    pub fn debug_info(&self) -> String {
        format!(
            "Client {{ base_url: {}, max_retries: {}, auth: {:?} }}",
            self.base_url, self.max_retries, self.auth
        )
    }

    pub fn is(&self, name: &str) -> Flag {
        Flag {
            name: name.to_string(),
            client: self,
        }
    }

    pub async fn list(&self) -> Result<Vec<flag::FeatureFlag>, FlagError> {
        // Check if cache needs refresh before listing
        {
            let cache = self.cache.read().await;
            if cache.should_refresh_cache().await {
                drop(cache); // Release the read lock before acquiring write lock
                if let Err(e) = self.refetch().await {
                    error!("Failed to refetch flags for list: {}", e);
                    // Continue with potentially stale cache data if refetch fails
                }
            }
        }

        let cache = self.cache.read().await;
        cache.get_all().await
            .map_err(|e| FlagError::CacheError(e.to_string()))
    }

    async fn is_enabled(&self, name: &str) -> bool {
        let name = name.to_lowercase();

        // Check if cache needs refresh
        {
            let cache = self.cache.read().await;
            if cache.should_refresh_cache().await {
                drop(cache); // Release the read lock before acquiring write lock
                if let Err(e) = self.refetch().await {
                    error!("Failed to refetch flags: {}", e);
                    // If refetch fails, continue to check the potentially stale cache.
                }
            }
        }

        // Check cache (which now contains combined API and local flags with overrides)
        let cache = self.cache.read().await;
        match cache.get(&name).await {
            Ok((enabled, exists)) => {
                if exists {
                    enabled
                } else {
                    false
                }
            }
            Err(_) => false, // Treat cache errors as flag not found
        }
    }

    async fn fetch_flags(&self) -> Result<ApiResponse, FlagError> {
        let auth = match &self.auth {
            Some(auth) => auth,
            None => return Err(FlagError::AuthError("Authentication is required".to_string())),
        };

        if auth.project_id.is_empty() {
            return Err(FlagError::AuthError("Project ID is required".to_string()));
        }
        if auth.agent_id.is_empty() {
            return Err(FlagError::AuthError("Agent ID is required".to_string()));
        }
        if auth.environment_id.is_empty() {
            return Err(FlagError::AuthError("Environment ID is required".to_string()));
        }

        let mut headers = HeaderMap::new();
        headers.insert("User-Agent", HeaderValue::from_static("Flags-Rust"));
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert("X-Project-ID", HeaderValue::from_str(&auth.project_id).unwrap());
        headers.insert("X-Agent-ID", HeaderValue::from_str(&auth.agent_id).unwrap());
        headers.insert("X-Environment-ID", HeaderValue::from_str(&auth.environment_id).unwrap());

        let url = format!("{}/flags", self.base_url);
        let response = self.http_client
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(FlagError::ApiError(format!(
                "Unexpected status code: {}",
                response.status()
            )));
        }

        let api_resp = response.json::<ApiResponse>().await?;
        Ok(api_resp)
    }

    async fn refetch(&self) -> Result<(), FlagError> {
        let mut circuit_state = self.circuit_state.write().await;

        if circuit_state.is_open {
            if let Some(last_failure) = circuit_state.last_failure {
                let now = Utc::now();
                // Keep the circuit open for a bit after failure
                if (now - last_failure).num_seconds() < 10 { // You can adjust this duration
                    warn!("Circuit breaker is open, skipping refetch.");
                    return Ok(());
                }
            }
            // If enough time has passed, attempt to close the circuit
            warn!("Attempting to close circuit breaker.");
            circuit_state.is_open = false;
            circuit_state.failure_count = 0;
        }
        drop(circuit_state); // Release the write lock

        let api_resp = match self.fetch_flags().await {
            Ok(resp) => {
                let mut circuit_state = self.circuit_state.write().await;
                circuit_state.failure_count = 0; // Reset failure count on success
                resp
            }
            Err(e) => {
                let mut circuit_state = self.circuit_state.write().await;
                circuit_state.failure_count += 1;
                circuit_state.last_failure = Some(Utc::now());
                if circuit_state.failure_count >= self.max_retries {
                    circuit_state.is_open = true;
                    error!("Refetch failed after {} retries, opening circuit breaker: {}", self.max_retries, e);
                } else {
                    warn!("Refetch failed (attempt {}/{}), retrying: {}", circuit_state.failure_count, self.max_retries, e);
                }
                drop(circuit_state); // Release the write lock
                // If fetching fails, we should still attempt to use local flags and potentially old cache data
                let local_flags = build_local(); // Build local flags even on API failure
                let mut cache = self.cache.write().await;
                // Attempt to refresh cache with only local flags if API failed
                cache.refresh(&local_flags, 60).await // Use a default interval if API interval is not available
                    .map_err(|e| FlagError::CacheError(e.to_string()))?;
                return Err(e); // Propagate the error
            }
        };

        let mut api_flags: Vec<flag::FeatureFlag> = api_resp.flags
            .into_iter()
            .map(|f| flag::FeatureFlag {
                enabled: f.enabled,
                details: flag::Details {
                    name: f.details.name.to_lowercase(),
                    id: f.details.id,
                },
            })
            .collect();

        let local_flags = build_local();

        // Combine API flags and local flags, with local overriding API
        let mut combined_flags = Vec::new();
        let mut local_flags_map: HashMap<String, FeatureFlag> = local_flags.into_iter().map(|f| (f.details.name.clone(), f)).collect();

        for api_flag in api_flags.drain(..) {
            if let Some(local_flag) = local_flags_map.remove(&api_flag.details.name) {
                // Local flag with the same name exists, use the local one
                combined_flags.push(local_flag);
            } else {
                // No local flag with the same name, use the API one
                combined_flags.push(api_flag);
            }
        }

        // Add any remaining local flags that didn't have a corresponding API flag
        combined_flags.extend(local_flags_map.into_values());


        let mut cache = self.cache.write().await;
        cache.refresh(&combined_flags, api_resp.interval_allowed).await
            .map_err(|e| FlagError::CacheError(e.to_string()))?;

        Ok(())
    }
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Client {
            base_url: self.base_url.clone(),
            http_client: self.http_client.clone(),
            cache: Arc::clone(&self.cache),
            max_retries: self.max_retries,
            circuit_state: Arc::clone(&self.circuit_state),
            auth: self.auth.clone(),
        }
    }
}

impl<'a> Flag<'a> {
    pub async fn enabled(&self) -> bool {
        self.client.is_enabled(&self.name).await
    }
}

pub struct ClientBuilder {
    base_url: String,
    max_retries: u32,
    auth: Option<Auth>,
    use_memory_cache: bool,
    file_name: Option<String>,
}

impl ClientBuilder {
    fn new() -> Self {
        Self {
            base_url: BASE_URL.to_string(),
            max_retries: MAX_RETRIES,
            auth: None,
            use_memory_cache: false,
            file_name: None,
        }
    }

    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.to_string();
        self
    }

    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn with_file_name(mut self, file_name: &str) -> Self {
        self.file_name = Some(file_name.to_string());
        self
    }

    pub fn with_memory_cache(mut self) -> Self {
        self.use_memory_cache = true;
        self
    }

    pub fn build(self) -> Client {
        let cache: Box<dyn Cache + Send + Sync> = Box::new(MemoryCache::new());

        Client {
            base_url: self.base_url,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            cache: Arc::new(RwLock::new(cache)),
            max_retries: self.max_retries,
            circuit_state: Arc::new(RwLock::new(CircuitState {
                is_open: false,
                failure_count: 0,
                last_failure: None,
            })),
            auth: self.auth,
        }
    }
}

fn build_local() -> Vec<FeatureFlag> {
    let mut result = Vec::new();

    for (key, value) in env::vars() {
        if !key.starts_with("FLAGS_") {
            continue;
        }

        let enabled = value == "true";
        let flag_name_env = key.trim_start_matches("FLAGS_").to_string();
        let flag_name_lower = flag_name_env.to_lowercase();

        // Create a FeatureFlag for the flag name as it appears in the environment variable (lowercase)
        result.push(FeatureFlag {
            enabled,
            details: Details {
                name: flag_name_lower.clone(),
                id: format!("local_{}", flag_name_lower), // Using a simple identifier for local flags
            },
        });

        // Optionally, create FeatureFlags for common variations (hyphens and spaces)
        if flag_name_lower.contains('_') {
            let flag_name_hyphenated = flag_name_lower.replace('_', "-");
            result.push(FeatureFlag {
                enabled,
                details: Details {
                    name: flag_name_hyphenated.clone(),
                    id: format!("local_{}", flag_name_hyphenated),
                },
            });
        }

        if flag_name_lower.contains('_') || flag_name_lower.contains('-') {
            let flag_name_spaced = flag_name_lower.replace('_', " ").replace('-', " ");
            result.push(FeatureFlag {
                enabled,
                details: Details {
                    name: flag_name_spaced.clone(),
                    id: format!("local_{}", flag_name_spaced),
                },
            });
        }

    }

    result
}

