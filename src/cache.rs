use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use crate::flag::FeatureFlag;

#[async_trait]
pub trait Cache {
    async fn get(&self, name: &str) -> Result<(bool, bool), Box<dyn std::error::Error + Send + Sync>>;
    async fn get_all(&self) -> Result<Vec<FeatureFlag>, Box<dyn std::error::Error + Send + Sync>>;
    async fn refresh(&mut self, flags: &[FeatureFlag], interval_allowed: i32) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn should_refresh_cache(&self) -> bool;
    async fn init(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

pub struct CacheSystem {
    cache: Box<dyn Cache + Send + Sync>,
}

impl CacheSystem {
    pub fn new(cache: Box<dyn Cache + Send + Sync>) -> Self {
        Self { cache }
    }
}

pub struct MemoryCache {
    flags: RwLock<HashMap<String, FeatureFlag>>,
    cache_ttl: i64,
    next_refresh: RwLock<DateTime<Utc>>,
}

impl MemoryCache {
    pub fn new() -> Self {
        let cache = Self {
            flags: RwLock::new(HashMap::new()),
            cache_ttl: 60,
            next_refresh: RwLock::new(Utc::now() - chrono::Duration::seconds(90)), // Initialize directly
        };

        cache
    }
}


#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, name: &str) -> Result<(bool, bool), Box<dyn std::error::Error + Send + Sync>> {
        let flags = self.flags.read().await;
        if let Some(flag) = flags.get(name) {
            Ok((flag.enabled, true))
        } else {
            Ok((false, false))
        }
    }

    async fn get_all(&self) -> Result<Vec<FeatureFlag>, Box<dyn std::error::Error + Send + Sync>> {
        let flags = self.flags.read().await;
        let all_flags: Vec<FeatureFlag> = flags.values().cloned().collect();
        Ok(all_flags)
    }

    async fn refresh(&mut self, flags: &[FeatureFlag], interval_allowed: i32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut flag_map = self.flags.write().await;
        flag_map.clear();

        for flag in flags {
            flag_map.insert(flag.details.name.clone(), flag.clone());
        }

        self.cache_ttl = interval_allowed as i64;
        let mut next_refresh = self.next_refresh.write().await;
        *next_refresh = Utc::now() + chrono::Duration::seconds(self.cache_ttl);

        Ok(())
    }

    async fn should_refresh_cache(&self) -> bool {
        let next_refresh = self.next_refresh.read().await;
        Utc::now() > *next_refresh
    }

    async fn init(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cache_ttl = 60;
        let mut next_refresh = self.next_refresh.write().await;
        *next_refresh = Utc::now() - chrono::Duration::seconds(90);
        Ok(())
    }
}
