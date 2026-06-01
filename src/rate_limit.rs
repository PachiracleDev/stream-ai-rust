//! Rate limit por usuario+entrevista: Redis, memoria local o desactivado.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

const RATE_LIMIT_SCRIPT: &str = r#"
local c = redis.call('INCR', KEYS[1])
if c == 1 then
  redis.call('EXPIRE', KEYS[1], tonumber(ARGV[1]))
end
if c > tonumber(ARGV[2]) then
  return 0
end
return 1
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitBackend {
    Redis,
    Memory,
    Disabled,
}

impl RateLimitBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Redis => "redis",
            Self::Memory => "memory",
            Self::Disabled => "disabled",
        }
    }

    pub fn from_env() -> Self {
        std::env::var("RATE_LIMIT_BACKEND")
            .ok()
            .and_then(|s| parse_backend(&s))
            .unwrap_or(Self::Redis)
    }
}

pub fn parse_backend(raw: &str) -> Option<RateLimitBackend> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "redis" => Some(RateLimitBackend::Redis),
        "memory" | "mem" | "inmemory" | "in-memory" => Some(RateLimitBackend::Memory),
        "disabled" | "off" | "none" | "false" | "0" => Some(RateLimitBackend::Disabled),
        _ => None,
    }
}

#[derive(Clone)]
struct RedisRateLimiter {
    conn: ConnectionManager,
    script: Arc<redis::Script>,
    max: u32,
    window_secs: u64,
}

impl RedisRateLimiter {
    async fn connect(
        redis_url: &str,
        max: u32,
        window_secs: u64,
    ) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self {
            conn,
            script: Arc::new(redis::Script::new(RATE_LIMIT_SCRIPT)),
            max,
            window_secs,
        })
    }

    async fn ping(&self) -> Result<(), redis::RedisError> {
        let mut conn = self.conn.clone();
        redis::cmd("PING").query_async(&mut conn).await
    }

    async fn check_allowed(&self, key: &str) -> Result<bool, redis::RedisError> {
        let redis_key = format!("relay:rl:{key}");
        let mut conn = self.conn.clone();
        let allowed: i64 = self
            .script
            .key(redis_key)
            .arg(self.window_secs as i64)
            .arg(self.max as i64)
            .invoke_async(&mut conn)
            .await?;
        Ok(allowed == 1)
    }
}

struct WindowEntry {
    count: u32,
    window_start: Instant,
}

#[derive(Clone)]
struct MemoryRateLimiter {
    max: u32,
    window: Duration,
    counters: Arc<Mutex<HashMap<String, WindowEntry>>>,
}

impl MemoryRateLimiter {
    fn new(max: u32, window_secs: u64) -> Self {
        Self {
            max,
            window: Duration::from_secs(window_secs),
            counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn check_allowed(&self, key: &str) -> bool {
        let mut counters = self.counters.lock().await;
        let now = Instant::now();
        let entry = counters
            .entry(key.to_string())
            .or_insert(WindowEntry {
                count: 0,
                window_start: now,
            });

        if now.duration_since(entry.window_start) >= self.window {
            entry.count = 0;
            entry.window_start = now;
        }

        entry.count += 1;
        entry.count <= self.max
    }
}

#[derive(Clone)]
pub struct RateLimiter(RateLimiterInner);

#[derive(Clone)]
enum RateLimiterInner {
    Redis(RedisRateLimiter),
    Memory(MemoryRateLimiter),
    Disabled,
}

impl RateLimiter {
    pub async fn from_env(max: u32, window_secs: u64) -> Result<Self, redis::RedisError> {
        match RateLimitBackend::from_env() {
            RateLimitBackend::Disabled => Ok(Self(RateLimiterInner::Disabled)),
            RateLimitBackend::Memory => Ok(Self(RateLimiterInner::Memory(
                MemoryRateLimiter::new(max, window_secs),
            ))),
            RateLimitBackend::Redis => {
                let redis_url = std::env::var("REDIS_URL")
                    .expect("REDIS_URL requerida cuando RATE_LIMIT_BACKEND=redis");
                Ok(Self(RateLimiterInner::Redis(
                    RedisRateLimiter::connect(&redis_url, max, window_secs).await?,
                )))
            }
        }
    }

    pub fn backend(&self) -> RateLimitBackend {
        match &self.0 {
            RateLimiterInner::Redis(_) => RateLimitBackend::Redis,
            RateLimiterInner::Memory(_) => RateLimitBackend::Memory,
            RateLimiterInner::Disabled => RateLimitBackend::Disabled,
        }
    }

    pub async fn ping(&self) -> Result<(), redis::RedisError> {
        match &self.0 {
            RateLimiterInner::Redis(l) => l.ping().await,
            RateLimiterInner::Memory(_) | RateLimiterInner::Disabled => Ok(()),
        }
    }

    pub async fn check_allowed(&self, key: &str) -> Result<bool, redis::RedisError> {
        match &self.0 {
            RateLimiterInner::Disabled => Ok(true),
            RateLimiterInner::Memory(l) => Ok(l.check_allowed(key).await),
            RateLimiterInner::Redis(l) => l.check_allowed(key).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backend_values() {
        assert_eq!(parse_backend("redis"), Some(RateLimitBackend::Redis));
        assert_eq!(parse_backend("memory"), Some(RateLimitBackend::Memory));
        assert_eq!(parse_backend("disabled"), Some(RateLimitBackend::Disabled));
        assert_eq!(parse_backend("unknown"), None);
    }

    #[tokio::test]
    async fn memory_limiter_blocks_after_max() {
        let limiter = MemoryRateLimiter::new(2, 60);
        assert!(limiter.check_allowed("u:1").await);
        assert!(limiter.check_allowed("u:1").await);
        assert!(!limiter.check_allowed("u:1").await);
        assert!(limiter.check_allowed("u:2").await);
    }

    #[tokio::test]
    async fn disabled_always_allows() {
        let limiter = RateLimiter(RateLimiterInner::Disabled);
        for _ in 0..100 {
            assert!(limiter.check_allowed("any").await.unwrap());
        }
    }
}
