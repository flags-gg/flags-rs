// src/cache.rs
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use log::error;

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
    flags: Mutex<HashMap<String, FeatureFlag>>,
    cache_ttl: i64,
    next_refresh: Mutex<DateTime<Utc>>,
}

impl MemoryCache {
    pub fn new() -> Self {
        let cache = Self {
            flags: Mutex::new(HashMap::new()),
            cache_ttl: 60,
            next_refresh: Mutex::new(Utc::now() - chrono::Duration::seconds(90)), // Initialize directly
        };

        cache
    }
}


#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, name: &str) -> Result<(bool, bool), Box<dyn std::error::Error + Send + Sync>> {
        let flags = self.flags.lock().unwrap();
        if let Some(flag) = flags.get(name) {
            Ok((flag.enabled, true))
        } else {
            Ok((false, false))
        }
    }

    async fn get_all(&self) -> Result<Vec<FeatureFlag>, Box<dyn std::error::Error + Send + Sync>> {
        let flags = self.flags.lock().unwrap();
        let all_flags: Vec<FeatureFlag> = flags.values().cloned().collect();
        Ok(all_flags)
    }

    async fn refresh(&mut self, flags: &[FeatureFlag], interval_allowed: i32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut flag_map = self.flags.lock().unwrap();
        flag_map.clear();

        for flag in flags {
            flag_map.insert(flag.details.name.clone(), flag.clone());
        }

        self.cache_ttl = interval_allowed as i64;
        let mut next_refresh = self.next_refresh.lock().unwrap();
        *next_refresh = Utc::now() + chrono::Duration::seconds(self.cache_ttl);

        Ok(())
    }

    async fn should_refresh_cache(&self) -> bool {
        let next_refresh = self.next_refresh.lock().unwrap();
        Utc::now() > *next_refresh
    }

    async fn init(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cache_ttl = 60;
        let mut next_refresh = self.next_refresh.lock().unwrap();
        *next_refresh = Utc::now() - chrono::Duration::seconds(90);
        Ok(())
    }
}

#[cfg(feature = "rusqlite")]
pub struct SqliteCache {
    conn: Mutex<rusqlite::Connection>,
    cache_ttl: i64,
    next_refresh: Mutex<DateTime<Utc>>,
}

#[cfg(feature = "rusqlite")]
impl SqliteCache {
    pub fn new(file_name: &str) -> Self {
        let conn = match rusqlite::Connection::open(file_name) {
            Ok(conn) => conn,
            Err(e) => {
                error!("Failed to open SQLite database: {}", e);
                panic!("Failed to open SQLite database");
            }
        };

        let cache = Self {
            conn: Mutex::new(conn),
            cache_ttl: 60,
            next_refresh: Mutex::new(Utc::now()),
        };

        // Initialize with a past refresh time to force initial refresh
        let mut next_refresh = cache.next_refresh.lock().unwrap();
        *next_refresh = Utc::now() - chrono::Duration::seconds(90);

        cache
    }
}

#[cfg(feature = "rusqlite")]
#[async_trait]
impl Cache for SqliteCache {
    async fn get(&self, name: &str) -> Result<(bool, bool), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare("SELECT enabled FROM flags WHERE name = ?")?;
        let mut rows = stmt.query(&[name])?;

        if let Some(row) = rows.next()? {
            let enabled: bool = row.get(0)?;
            Ok((enabled, true))
        } else {
            Ok((false, false))
        }
    }

    async fn get_all(&self) -> Result<Vec<FeatureFlag>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare("SELECT name, id, enabled FROM flags")?;
        let rows = stmt.query_map([], |row| {
            Ok(FeatureFlag {
                enabled: row.get(2)?,
                details: crate::flag::Details {
                    name: row.get(0)?,
                    id: row.get(1)?,
                },
            })
        })?;

        let mut flags = Vec::new();
        for flag in rows {
            flags.push(flag?);
        }

        Ok(flags)
    }

    async fn refresh(&mut self, flags: &[FeatureFlag], interval_allowed: i32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.lock().unwrap();

        let tx = conn.transaction()?;
        tx.execute("DELETE FROM flags", [])?;

        for flag in flags {
            tx.execute(
                "INSERT INTO flags (name, id, enabled) VALUES (?, ?, ?)",
                &[
                    &flag.details.name,
                    &flag.details.id,
                    &flag.enabled,
                ],
            )?;
        }

        tx.commit()?;

        self.cache_ttl = interval_allowed as i64;
        let mut next_refresh = self.next_refresh.lock().unwrap();
        *next_refresh = Utc::now() + chrono::Duration::seconds(self.cache_ttl);

        Ok(())
    }

    async fn should_refresh_cache(&self) -> bool {
        let next_refresh = self.next_refresh.lock().unwrap();
        Utc::now() > *next_refresh
    }

    async fn init(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "CREATE TABLE IF NOT EXISTS flags (
                name TEXT PRIMARY KEY,
                id TEXT NOT NULL,
                enabled BOOLEAN NOT NULL
            )",
            [],
        )?;

        self.cache_ttl = 60;
        let mut next_refresh = self.next_refresh.lock().unwrap();
        *next_refresh = Utc::now() - chrono::Duration::seconds(90);

        Ok(())
    }
}
