// src/lib.rs
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use log::{error, warn};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod cache;
pub mod flag;
mod tests;

use crate::cache::{Cache, CacheSystem, MemoryCache};

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
    circuit_state: RwLock<CircuitState>,
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
        let cache = self.cache.read().unwrap();
        let flags = cache.get_all().await
            .map_err(|e| FlagError::CacheError(e.to_string()))?;
        Ok(flags)
    }

    async fn is_enabled(&self, name: &str) -> bool {
        let name = name.to_lowercase();

        // Check if cache needs refresh
        {
            let cache = self.cache.read().unwrap();
            if cache.should_refresh_cache().await {
                drop(cache); // Release the read lock before acquiring write lock
                if let Err(e) = self.refetch().await {
                    error!("Failed to refetch flags: {}", e);
                    return false;
                }
            }
        }

        // Check local environment variables first
        let local_flags = build_local();
        if let Some(&enabled) = local_flags.get(&name) {
            return enabled;
        }

        // Check cache
        let cache = self.cache.read().unwrap();
        match cache.get(&name).await {
            Ok((enabled, exists)) => {
                if exists {
                    enabled
                } else {
                    false
                }
            }
            Err(_) => false,
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
        let mut circuit_state = self.circuit_state.write().unwrap();

        if circuit_state.is_open {
            if let Some(last_failure) = circuit_state.last_failure {
                let now = Utc::now();
                if (now - last_failure).num_seconds() < 10 {
                    return Ok(());
                }
            }
            circuit_state.is_open = false;
            circuit_state.failure_count = 0;
        }
        drop(circuit_state);

        let mut api_resp = None;
        let mut last_error = None;

        for retry in 0..self.max_retries {
            match self.fetch_flags().await {
                Ok(resp) => {
                    api_resp = Some(resp);
                    let mut circuit_state = self.circuit_state.write().unwrap();
                    circuit_state.failure_count = 0;
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    let mut circuit_state = self.circuit_state.write().unwrap();
                    circuit_state.failure_count += 1;

                    if circuit_state.failure_count >= self.max_retries {
                        circuit_state.is_open = true;
                        circuit_state.last_failure = Some(Utc::now());
                        return Ok(());
                    }
                    drop(circuit_state);

                    tokio::time::sleep(Duration::from_secs((retry + 1) as u64)).await;
                }
            }
        }

        let api_resp = match api_resp {
            Some(resp) => resp,
            None => return Err(last_error.unwrap()),
        };

        let flags: Vec<flag::FeatureFlag> = api_resp.flags
            .into_iter()
            .map(|f| flag::FeatureFlag {
                enabled: f.enabled,
                details: flag::Details {
                    name: f.details.name.to_lowercase(),
                    id: f.details.id,
                },
            })
            .collect();

        let mut cache = self.cache.write().unwrap();
        cache.refresh(&flags, api_resp.interval_allowed).await
            .map_err(|e| FlagError::CacheError(e.to_string()))?;

        Ok(())
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
        let cache: Box<dyn Cache + Send + Sync> = if self.use_memory_cache {
            Box::new(MemoryCache::new())
        } else {
            #[cfg(feature = "rusqlite")]
            {
                if let Some(file_name) = self.file_name {
                    Box::new(cache::SqliteCache::new(&file_name))
                } else {
                    Box::new(MemoryCache::new())
                }
            }
            #[cfg(not(feature = "rusqlite"))]
            {
                Box::new(MemoryCache::new())
            }
        };

        Client {
            base_url: self.base_url,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            cache: Arc::new(RwLock::new(cache)),
            max_retries: self.max_retries,
            circuit_state: RwLock::new(CircuitState {
                is_open: false,
                failure_count: 0,
                last_failure: None,
            }),
            auth: self.auth,
        }
    }
}

fn build_local() -> HashMap<String, bool> {
    let mut result = HashMap::new();

    for (key, value) in env::vars() {
        if !key.starts_with("FLAGS_") {
            continue;
        }

        let enabled = value == "true";
        let key_lower = key.trim_start_matches("FLAGS_").to_lowercase();

        result.insert(key_lower.clone(), enabled);
        result.insert(key_lower.replace('_', "-"), enabled);
        result.insert(key_lower.replace('_', " "), enabled);
    }

    result
}
