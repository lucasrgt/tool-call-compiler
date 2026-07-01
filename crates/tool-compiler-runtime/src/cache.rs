//! Tool output caching.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_compiler_ir::Resource;

/// Cache key for one tool call: adapter, tool, optional tool version, and
/// the canonical JSON of the resolved input.
///
/// The version comes from `ToolSpec::version` (possibly hydrated from
/// registry capabilities); bumping it invalidates previously cached outputs
/// of the tool without touching other entries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CacheKey {
    /// Adapter executing the call.
    pub adapter: String,
    /// Tool name.
    pub tool: String,
    /// Tool behavior version, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Canonical JSON of the resolved input.
    pub input: String,
}

/// Pluggable cache for `pure`/`cacheable` tool outputs.
///
/// Contract for implementations:
///
/// - `get`/`insert` follow read-through semantics; the runtime re-executes on
///   a miss and inserts afterwards, so lossy caches (TTL, eviction, external
///   stores that drop writes) are always safe.
/// - `insert` receives the tool's resolved **read resources**; the
///   implementation must remember them so `invalidate_reads` can drop every
///   entry whose reads intersect a written resource. Ignoring `reads` and
///   clearing everything on `invalidate_reads` is a correct (if blunt)
///   implementation.
/// - The runtime calls `invalidate_reads` whenever a node that writes
///   resources completes; this is what keeps cached reads coherent with
///   writes both inside one run and across runs sharing the cache.
/// - Implementations must be safe for concurrent use.
#[async_trait]
pub trait ToolCache: Send + Sync {
    /// Looks up a cached output.
    async fn get(&self, key: &CacheKey) -> Option<Arc<Value>>;
    /// Stores an output together with the resources the call read.
    async fn insert(&self, key: CacheKey, output: Arc<Value>, reads: BTreeSet<Resource>);
    /// Drops every entry whose recorded reads intersect `resources`.
    async fn invalidate_reads(&self, resources: &BTreeSet<Resource>);
    /// Drops everything.
    async fn clear(&self);
}

/// In-memory [`ToolCache`] with LRU eviction and optional TTL.
///
/// Uses a plain `std::sync::Mutex` (operations are short and never await)
/// and shares outputs as `Arc<Value>` so large results are not cloned per
/// hit.
#[derive(Clone)]
pub struct MemoryCache {
    inner: Arc<Mutex<Inner>>,
    max_entries: usize,
    ttl: Option<Duration>,
}

struct Inner {
    entries: BTreeMap<CacheKey, Entry>,
    by_resource: BTreeMap<Resource, BTreeSet<CacheKey>>,
    clock: u64,
}

struct Entry {
    output: Arc<Value>,
    reads: BTreeSet<Resource>,
    inserted: Instant,
    last_used: u64,
}

impl MemoryCache {
    /// Default maximum number of entries.
    pub const DEFAULT_MAX_ENTRIES: usize = 1024;

    /// Creates a cache with [`Self::DEFAULT_MAX_ENTRIES`] and no TTL.
    pub fn new() -> Self {
        Self::with_limits(Self::DEFAULT_MAX_ENTRIES, None)
    }

    /// Creates a cache with an entry cap and an optional time-to-live.
    pub fn with_limits(max_entries: usize, ttl: Option<Duration>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: BTreeMap::new(),
                by_resource: BTreeMap::new(),
                clock: 0,
            })),
            max_entries: max_entries.max(1),
            ttl,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Default for MemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    fn remove(&mut self, key: &CacheKey) {
        if let Some(entry) = self.entries.remove(key) {
            for resource in entry.reads {
                if let Some(keys) = self.by_resource.get_mut(&resource) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.by_resource.remove(&resource);
                    }
                }
            }
        }
    }

    fn evict_least_recently_used(&mut self) {
        let Some(victim) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        self.remove(&victim);
    }
}

#[async_trait]
impl ToolCache for MemoryCache {
    async fn get(&self, key: &CacheKey) -> Option<Arc<Value>> {
        let mut inner = self.lock();
        inner.clock += 1;
        let clock = inner.clock;

        let expired = match inner.entries.get(key) {
            Some(entry) => self.ttl.is_some_and(|ttl| entry.inserted.elapsed() > ttl),
            None => return None,
        };
        if expired {
            inner.remove(key);
            return None;
        }

        let entry = inner.entries.get_mut(key)?;
        entry.last_used = clock;
        Some(entry.output.clone())
    }

    async fn insert(&self, key: CacheKey, output: Arc<Value>, reads: BTreeSet<Resource>) {
        let mut inner = self.lock();
        inner.clock += 1;
        let clock = inner.clock;

        inner.remove(&key);
        while inner.entries.len() >= self.max_entries {
            inner.evict_least_recently_used();
        }
        for resource in &reads {
            inner
                .by_resource
                .entry(resource.clone())
                .or_default()
                .insert(key.clone());
        }
        inner.entries.insert(
            key,
            Entry {
                output,
                reads,
                inserted: Instant::now(),
                last_used: clock,
            },
        );
    }

    async fn invalidate_reads(&self, resources: &BTreeSet<Resource>) {
        let mut inner = self.lock();
        let mut victims = BTreeSet::new();
        for resource in resources {
            if let Some(keys) = inner.by_resource.get(resource) {
                victims.extend(keys.iter().cloned());
            }
        }
        for key in victims {
            inner.remove(&key);
        }
    }

    async fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.by_resource.clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn key(name: &str) -> CacheKey {
        CacheKey {
            adapter: "test".into(),
            tool: name.into(),
            version: None,
            input: "{}".into(),
        }
    }

    #[tokio::test]
    async fn stores_and_returns_shared_outputs() {
        let cache = MemoryCache::new();
        cache
            .insert(key("a"), Arc::new(json!(1)), BTreeSet::new())
            .await;

        assert_eq!(cache.get(&key("a")).await.as_deref(), Some(&json!(1)));
        assert_eq!(cache.get(&key("b")).await, None);
    }

    #[tokio::test]
    async fn evicts_least_recently_used_beyond_capacity() {
        let cache = MemoryCache::with_limits(2, None);
        cache
            .insert(key("a"), Arc::new(json!(1)), BTreeSet::new())
            .await;
        cache
            .insert(key("b"), Arc::new(json!(2)), BTreeSet::new())
            .await;
        cache.get(&key("a")).await; // refresh a
        cache
            .insert(key("c"), Arc::new(json!(3)), BTreeSet::new())
            .await;

        assert!(cache.get(&key("a")).await.is_some());
        assert!(cache.get(&key("b")).await.is_none());
        assert!(cache.get(&key("c")).await.is_some());
    }

    #[tokio::test]
    async fn ttl_expires_entries() {
        let cache = MemoryCache::with_limits(10, Some(Duration::from_millis(0)));
        cache
            .insert(key("a"), Arc::new(json!(1)), BTreeSet::new())
            .await;
        tokio::time::sleep(Duration::from_millis(2)).await;

        assert!(cache.get(&key("a")).await.is_none());
    }

    #[tokio::test]
    async fn invalidates_entries_by_read_resource() {
        let cache = MemoryCache::new();
        cache
            .insert(
                key("file_read"),
                Arc::new(json!("old")),
                ["file:a.txt".to_owned()].into_iter().collect(),
            )
            .await;
        cache
            .insert(key("other"), Arc::new(json!("keep")), BTreeSet::new())
            .await;

        cache
            .invalidate_reads(&["file:a.txt".to_owned()].into_iter().collect())
            .await;

        assert!(cache.get(&key("file_read")).await.is_none());
        assert!(cache.get(&key("other")).await.is_some());
    }

    #[tokio::test]
    async fn versioned_keys_are_distinct() {
        let cache = MemoryCache::new();
        let mut v1 = key("tool");
        v1.version = Some("1".into());
        let mut v2 = key("tool");
        v2.version = Some("2".into());
        cache
            .insert(v1.clone(), Arc::new(json!(1)), BTreeSet::new())
            .await;

        assert!(cache.get(&v2).await.is_none());
        assert!(cache.get(&v1).await.is_some());
    }
}
