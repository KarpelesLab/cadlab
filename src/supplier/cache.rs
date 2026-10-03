//! Response cache for network providers: `<cache dir>/<provider>/<hash>.json`, with a TTL.
//!
//! The cache lives in the user cache directory (`$XDG_CACHE_HOME/cadlab/suppliers` or
//! `~/.cache/cadlab/suppliers`), never in projects. With `CADLAB_OFFLINE=1`, providers answer from
//! the cache only, however old.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Default time-to-live of cached responses.
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    fetched_at: u64,
    body: String,
}

/// A response cache.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
    ttl: Duration,
}

/// Stable 64-bit FNV-1a hash (std's hasher is not stable across releases).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Whether offline mode is on (`CADLAB_OFFLINE=1`).
pub fn offline() -> bool {
    std::env::var("CADLAB_OFFLINE").is_ok_and(|v| v == "1")
}

impl Cache {
    /// A cache in `dir`.
    pub fn new(dir: PathBuf, ttl: Duration) -> Self {
        Cache { dir, ttl }
    }

    /// The user cache directory, if one can be determined.
    pub fn user_default() -> Option<Self> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        Some(Cache::new(base.join("cadlab").join("suppliers"), DEFAULT_TTL))
    }

    fn path(&self, provider: &str, key: &str) -> PathBuf {
        self.dir.join(provider).join(format!("{:016x}.json", fnv1a(key)))
    }

    fn read(&self, provider: &str, key: &str) -> Option<Entry> {
        let text = std::fs::read_to_string(self.path(provider, key)).ok()?;
        let e: Entry = serde_json::from_str(&text).ok()?;
        (e.key == key).then_some(e)
    }

    /// A cached body younger than the TTL (any age in offline mode).
    pub fn get(&self, provider: &str, key: &str) -> Option<String> {
        let e = self.read(provider, key)?;
        (offline() || now().saturating_sub(e.fetched_at) < self.ttl.as_secs()).then_some(e.body)
    }

    /// A cached body of any age.
    pub fn get_stale(&self, provider: &str, key: &str) -> Option<String> {
        self.read(provider, key).map(|e| e.body)
    }

    /// Stores a body. Failures are ignored: the cache is an optimization.
    pub fn put(&self, provider: &str, key: &str, body: &str) {
        self.put_at(provider, key, body, now());
    }

    fn put_at(&self, provider: &str, key: &str, body: &str, fetched_at: u64) {
        let path = self.path(provider, key);
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let e = Entry {
            key: key.to_string(),
            fetched_at,
            body: body.to_string(),
        };
        if let Ok(text) = serde_json::to_string(&e) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let c = Cache::new(dir.path().to_path_buf(), Duration::from_secs(60));
        assert_eq!(c.get("p", "q"), None);
        c.put("p", "q", "body");
        assert_eq!(c.get("p", "q").as_deref(), Some("body"));
        c.put_at("p", "old", "stale", now() - 3600);
        assert_eq!(c.get("p", "old"), None);
        assert_eq!(c.get_stale("p", "old").as_deref(), Some("stale"));
        assert_eq!(fnv1a("abc"), 0xe71f_a219_0541_574b);
    }
}
