//! 64-shard query cache with three-tier invalidation.
//!
//! Each shard is a `parking_lot::RwLock<HashMap<QueryKey, CachedResult>>`.
//! Sharding reduces lock contention: concurrent reads to different query types
//! never contend. The shard count is configurable (must be a power of two).
//!
//! # PN3 raw-byte retention
//!
//! For queries with `PERSISTENT = true`, [`ShardedCache::insert_query`]
//! serialises both the input key and the output value via `postcard` at insert
//! time and stores the raw bytes alongside the typed value. This makes
//! streaming the cache to disk in [`iter_persistent`] allocation-free after
//! the fact — no re-serialisation is needed during save.
//!
//! Entries whose serialised size exceeds [`QueryDbConfig::max_entry_size_bytes`]
//! are **not** stored (soft skip — insert returns `Ok(())`). The caller's
//! computed value is unaffected because `QueryDb::get` returns the value
//! directly without going through the cache for that invocation.
//!
//! For `PERSISTENT = false` queries the raw bytes are set to empty slices and
//! [`iter_persistent`] skips them.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;
use smallvec::SmallVec;

use crate::config::QueryDbConfig;
use crate::dependency::FileDep;
use crate::input::FileInputStore;
use crate::persistence::QueryDeps;
use crate::query::{DerivedQuery, QueryKey};

/// A cache entry yielded by [`ShardedCache::iter_persistent`].
///
/// Each entry carries everything the SAVE_PATH unit needs to write the entry to
/// disk: the stable on-disk discriminator, the serialised key + value, and the
/// dependency metadata needed to validate the entry on reload.
///
/// `Arc<[u8]>` is used instead of `Vec<u8>` so that collecting entries from a
/// shard (while holding the shard lock) does only cheap reference-count bumps,
/// not byte copies. The save loop can then release all shard locks before
/// performing any I/O.
// SAVE_PATH (the next DAG unit) constructs and consumes this type.
// Allow dead-code lint until that unit is implemented.
#[allow(dead_code)]
pub(crate) struct PersistableEntry {
    /// Stable on-disk discriminator from [`DerivedQuery::QUERY_TYPE_ID`].
    pub query_type_id: u32,
    /// Postcard-serialised form of the query's input key.
    pub raw_key_bytes: Arc<[u8]>,
    /// Postcard-serialised form of the query's output value.
    pub raw_result_bytes: Arc<[u8]>,
    /// Dependency metadata for three-tier cache validation on reload.
    pub deps: QueryDeps,
}

/// A cached query result with dependency metadata for three-tier validation.
///
/// # Raw byte fields
///
/// `raw_key_bytes` and `raw_result_bytes` are populated at insert time by
/// [`ShardedCache::insert_query`] for queries with `PERSISTENT = true`. They
/// hold the postcard-serialised key and value respectively, enabling
/// [`ShardedCache::iter_persistent`] to stream entries to disk without
/// acquiring shard locks during I/O.
///
/// For `PERSISTENT = false` queries (or entries inserted via the bare
/// [`ShardedCache::insert`] method) both byte slices are empty.
///
/// The typed `value` field is always populated for both persistent and
/// non-persistent queries; the raw bytes are a read-side convenience only.
pub struct CachedResult {
    /// Type-erased query result value.
    value: Box<dyn Any + Send + Sync>,
    /// Tier 1: File-level dependencies recorded during execution.
    ///
    /// Each entry is `(FileId, revision_at_read_time)`. SmallVec with inline
    /// capacity 8 covers the common case of queries touching ≤8 files without
    /// heap allocation.
    file_deps: SmallVec<[FileDep; 8]>,
    /// Tier 2: Global edge revision at cache time (None if query doesn't track).
    edge_revision: Option<u64>,
    /// Tier 3: Global metadata revision at cache time (None if query doesn't track).
    metadata_revision: Option<u64>,
    /// Postcard-serialised input key (empty for non-persistent queries).
    raw_key_bytes: Arc<[u8]>,
    /// Postcard-serialised output value (empty for non-persistent queries).
    raw_result_bytes: Arc<[u8]>,
    /// Stable on-disk discriminator for the query type.
    ///
    /// Zero for entries inserted via the bare [`ShardedCache::insert`] path
    /// (non-typed, no serialisation). Set to [`DerivedQuery::QUERY_TYPE_ID`]
    /// by [`ShardedCache::insert_query`].
    query_type_id: u32,
    /// Whether this entry is eligible for persistence.
    ///
    /// `true` only when inserted via [`ShardedCache::insert_query`] for a
    /// query whose `PERSISTENT = true` and whose serialised size is within
    /// [`QueryDbConfig::max_entry_size_bytes`].
    persistent: bool,
}

impl CachedResult {
    /// Creates a new cached result with dependency metadata.
    ///
    /// Raw-byte fields are left empty; `persistent` is `false`. Use
    /// [`ShardedCache::insert_query`] when raw-byte retention is required.
    pub fn new<V: Clone + Send + Sync + 'static>(
        value: V,
        file_deps: SmallVec<[FileDep; 8]>,
        edge_revision: Option<u64>,
        metadata_revision: Option<u64>,
    ) -> Self {
        let empty: Arc<[u8]> = Arc::from(Vec::<u8>::new().into_boxed_slice());
        Self {
            value: Box::new(value),
            file_deps,
            edge_revision,
            metadata_revision,
            raw_key_bytes: Arc::clone(&empty),
            raw_result_bytes: empty,
            query_type_id: 0,
            persistent: false,
        }
    }

    /// Creates a fully-populated cached result for a persistent query.
    ///
    /// This is called by [`ShardedCache::insert_query`] after serialisation.
    fn new_persistent<V: Clone + Send + Sync + 'static>(
        value: V,
        file_deps: SmallVec<[FileDep; 8]>,
        edge_revision: Option<u64>,
        metadata_revision: Option<u64>,
        raw_key_bytes: Arc<[u8]>,
        raw_result_bytes: Arc<[u8]>,
        query_type_id: u32,
    ) -> Self {
        Self {
            value: Box::new(value),
            file_deps,
            edge_revision,
            metadata_revision,
            raw_key_bytes,
            raw_result_bytes,
            query_type_id,
            persistent: true,
        }
    }

    /// Attempts to downcast the value to the expected type.
    #[must_use]
    pub fn downcast_value<V: Clone + 'static>(&self) -> Option<&V> {
        self.value.downcast_ref::<V>()
    }

    /// Returns the cached edge revision, if tracked.
    #[inline]
    #[must_use]
    pub fn edge_revision(&self) -> Option<u64> {
        self.edge_revision
    }

    /// Returns the cached metadata revision, if tracked.
    #[inline]
    #[must_use]
    pub fn metadata_revision(&self) -> Option<u64> {
        self.metadata_revision
    }

    /// Returns the file deps for external validation.
    #[inline]
    #[must_use]
    pub fn file_deps(&self) -> &SmallVec<[FileDep; 8]> {
        &self.file_deps
    }

    /// Returns the raw postcard-serialised key bytes.
    ///
    /// Empty (`is_empty() == true`) for non-persistent entries.
    #[inline]
    #[must_use]
    pub fn raw_key_bytes(&self) -> &Arc<[u8]> {
        &self.raw_key_bytes
    }

    /// Returns the raw postcard-serialised result bytes.
    ///
    /// Empty (`is_empty() == true`) for non-persistent entries.
    #[inline]
    #[must_use]
    pub fn raw_result_bytes(&self) -> &Arc<[u8]> {
        &self.raw_result_bytes
    }

    /// Returns the stable on-disk query type discriminator.
    ///
    /// Zero for non-typed entries (those inserted without [`ShardedCache::insert_query`]).
    #[inline]
    #[must_use]
    pub fn query_type_id(&self) -> u32 {
        self.query_type_id
    }

    /// Returns whether this entry is eligible for persistence.
    #[inline]
    #[must_use]
    pub fn persistent(&self) -> bool {
        self.persistent
    }

    /// Validates Tier 1 file-level dependencies against the current input store.
    ///
    /// Returns `true` if ALL recorded `(FileId, revision)` pairs match the
    /// current revision in the store. Returns `false` if any file's revision
    /// has advanced or if a file has been removed from the store.
    #[must_use]
    pub fn validate_file_deps(&self, inputs: &FileInputStore) -> bool {
        self.file_deps
            .iter()
            .all(|&(fid, rev)| inputs.revision(fid) == Some(rev))
    }
}

// CachedResult cannot derive Debug due to Box<dyn Any>, so manual impl.
impl std::fmt::Debug for CachedResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedResult")
            .field("file_deps", &self.file_deps)
            .field("edge_revision", &self.edge_revision)
            .field("metadata_revision", &self.metadata_revision)
            .field("raw_key_bytes_len", &self.raw_key_bytes.len())
            .field("raw_result_bytes_len", &self.raw_result_bytes.len())
            .field("query_type_id", &self.query_type_id)
            .field("persistent", &self.persistent)
            .finish_non_exhaustive()
    }
}

/// 64-shard query cache.
///
/// Each shard protects a `HashMap<QueryKey, CachedResult>` behind a
/// `parking_lot::RwLock`. The query registry assigns each query type to a
/// specific shard via `TypeId` hashing, so reads for different query types
/// never contend on the same lock.
///
/// # Raw-byte retention and persistence
///
/// Use [`insert_query`] (generic over `Q: DerivedQuery`) to insert entries with
/// raw-byte retention. The method serialises the key and value at insert time
/// and enforces the `max_entry_size_bytes` cap from [`QueryDbConfig`].
///
/// Use [`iter_persistent`] to stream all persistent entries for the SAVE_PATH unit.
/// It collects cheap `Arc` clones under each shard lock, then releases the lock
/// before yielding, so shard locks are never held during I/O.
pub struct ShardedCache {
    shards: Vec<RwLock<HashMap<QueryKey, CachedResult>>>,
}

impl ShardedCache {
    /// Creates a new cache with the given number of shards.
    ///
    /// # Panics
    ///
    /// Panics if `shard_count` is zero or not a power of two.
    #[must_use]
    pub fn new(shard_count: usize) -> Self {
        assert!(shard_count > 0 && shard_count.is_power_of_two());
        let shards = (0..shard_count)
            .map(|_| RwLock::new(HashMap::new()))
            .collect();
        Self { shards }
    }

    /// Returns the number of shards.
    #[inline]
    #[must_use]
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Attempts to retrieve a cached value, validating and downcasting within
    /// the read lock scope.
    ///
    /// The `validate` closure receives the cached result and should return
    /// `true` if the cache entry is still valid. If valid, the value is
    /// downcast and cloned. Returns `None` on miss, failed validation, or
    /// downcast failure.
    ///
    /// This design avoids lifetime issues with read guards by performing all
    /// work within the lock scope.
    pub fn get_if_valid<V: Clone + 'static>(
        &self,
        shard_idx: usize,
        key: &QueryKey,
        validate: impl FnOnce(&CachedResult) -> bool,
    ) -> Option<V> {
        let shard = self.shards[shard_idx].read();
        let cached = shard.get(key)?;
        if !validate(cached) {
            return None;
        }
        cached.downcast_value::<V>().cloned()
    }

    /// Cold-load rehydration lookup.
    ///
    /// Companion to [`get_if_valid`] for entries placed by
    /// [`ShardedCache::insert_validated`] during PN3's `load_derived`. Those
    /// entries carry raw `postcard` bytes for the value but only a unit
    /// placeholder in the typed `Box<dyn Any>` slot, so [`get_if_valid`]'s
    /// downcast returns `None` on them. This method:
    ///
    /// 1. Reads the cached entry; returns `None` on miss.
    /// 2. Runs `validate` (three-tier revision check). Returns `None` on fail.
    /// 3. If the entry's raw bytes decode into `V` via `postcard::from_bytes`,
    ///    **promotes** the entry in place **only if** no other thread has
    ///    already written a typed value into the slot. Subsequent lookups
    ///    hit the fast `get_if_valid` path. Returns the decoded value.
    ///
    /// The read lock is dropped before the decode so the (moderately-sized)
    /// postcard work does not block the shard. The write lock for promotion
    /// is re-acquired briefly.
    ///
    /// # Concurrent-update safety
    ///
    /// Between this method's read and write phases another thread may have
    /// recomputed the entry, producing a fresher typed value, OR loaded a
    /// more recent revision from disk. Blindly overwriting `cached.value`
    /// in that gap would clobber the newer result with the stale cold-
    /// loaded one (Codex review finding on commit `a41787179`). The promote
    /// step therefore:
    ///
    /// 1. Re-reads the entry under the write lock.
    /// 2. Verifies it is still the unit placeholder left by
    ///    [`insert_validated`] — i.e. `value.downcast_ref::<()>()` succeeds.
    ///    If anything else lives there (a typed `V`, a different revision's
    ///    value, or a newly-recomputed result), skip the overwrite.
    /// 3. Verifies the raw bytes have not changed (e.g., a concurrent
    ///    `insert_query` with a different value).
    /// 4. Only then writes the decoded value.
    ///
    /// The caller still receives the decoded value it returned up, because
    /// correctness for this individual call does not depend on the promote
    /// succeeding — a lost promotion just means the next reader pays the
    /// decode cost too. The race window is bounded to at most one decode
    /// per concurrent reader per entry per cold-start session.
    ///
    /// # Errors
    ///
    /// Returns `None` on miss, validation failure, or decode failure. The
    /// caller falls back to recomputation on `None` just as it would for a
    /// cold cache miss.
    pub fn get_cold_if_valid<V: Clone + Send + Sync + serde::de::DeserializeOwned + 'static>(
        &self,
        shard_idx: usize,
        key: &QueryKey,
        validate: impl FnOnce(&CachedResult) -> bool,
    ) -> Option<V> {
        // Read phase: load the entry, validate tiers, clone out the raw bytes.
        let raw_bytes_snapshot: Arc<[u8]> = {
            let shard = self.shards[shard_idx].read();
            let cached = shard.get(key)?;
            if !validate(cached) {
                return None;
            }
            // Require the placeholder shape for the cold path. If the entry
            // already carries a typed V, the warm `get_if_valid` in the
            // caller path would have taken it; finding a non-placeholder here
            // implies a concurrent recompute landed between the caller's
            // warm-probe and this cold-probe. Defer to that result by
            // returning None so the caller falls through to recomputation
            // (which will find and reuse the concurrent result on its own
            // warm probe).
            cached.value.downcast_ref::<()>()?;
            Arc::clone(&cached.raw_result_bytes)
        };

        // Decode outside any lock.
        let decoded: V = postcard::from_bytes(&raw_bytes_snapshot).ok()?;

        // Write phase: promote the entry only if it is still the same
        // placeholder we decoded from. Skip the write if anything changed.
        {
            let mut shard = self.shards[shard_idx].write();
            if let Some(cached) = shard.get_mut(key) {
                let still_placeholder = cached.value.downcast_ref::<()>().is_some();
                let bytes_unchanged = Arc::ptr_eq(&cached.raw_result_bytes, &raw_bytes_snapshot);
                if still_placeholder && bytes_unchanged {
                    cached.value = Box::new(decoded.clone());
                }
                // else: a concurrent thread promoted, recomputed, or
                // replaced the entry — leave their work intact.
            }
        }

        Some(decoded)
    }

    /// Inserts a pre-built [`CachedResult`] into the specified shard.
    ///
    /// This is the low-level, non-generic insert used internally and by tests
    /// that construct [`CachedResult`] directly (without needing raw-byte
    /// retention). For production call-sites that require serialisation and the
    /// `max_entry_size_bytes` cap, use [`insert_query`] instead.
    pub fn insert(&self, shard_idx: usize, key: QueryKey, result: CachedResult) {
        let mut shard = self.shards[shard_idx].write();
        shard.insert(key, result);
    }

    /// Type-aware insert for queries that require raw-byte retention.
    ///
    /// Serialises `key` and `value` via `postcard` at insert time. If
    /// `Q::PERSISTENT = true` and `raw_result_bytes.len() <=
    /// config.max_entry_size_bytes`, the entry is stored with full persistence
    /// metadata. If the serialised value exceeds the cap, the entry is **not**
    /// stored (soft skip — returns `Ok(())`); the caller's computed value is
    /// unaffected.
    ///
    /// For `Q::PERSISTENT = false`, the typed value is stored but raw bytes are
    /// left empty and `persistent = false` — the entry is invisible to
    /// [`iter_persistent`].
    ///
    /// # Errors
    ///
    /// Returns an error only if `postcard` serialisation of the key or value
    /// fails (should not occur for well-formed types that implement `Serialize`).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_query<Q: DerivedQuery>(
        &self,
        shard_idx: usize,
        query_key: QueryKey,
        key: &Q::Key,
        value: Q::Value,
        file_deps: SmallVec<[FileDep; 8]>,
        edge_revision: Option<u64>,
        metadata_revision: Option<u64>,
        config: &QueryDbConfig,
    ) -> Result<(), postcard::Error>
    where
        Q::Key: Serialize,
        Q::Value: Serialize,
    {
        if !Q::PERSISTENT {
            // Non-persistent query: store the typed value without raw bytes.
            let result = CachedResult::new(value, file_deps, edge_revision, metadata_revision);
            let mut shard = self.shards[shard_idx].write();
            shard.insert(query_key, result);
            return Ok(());
        }

        // Serialise key and value.
        let raw_key = postcard::to_allocvec(key)?;
        let raw_value = postcard::to_allocvec(&value)?;

        // Enforce the per-entry size cap on the value bytes.
        if raw_value.len() > config.max_entry_size_bytes {
            log::debug!(
                "sqry-db: skipping oversized cache entry (query_type_id={:#06x}, \
                 raw_result_bytes={} bytes, max={})",
                Q::QUERY_TYPE_ID,
                raw_value.len(),
                config.max_entry_size_bytes,
            );
            // Soft skip: do NOT store the entry. The caller's computed value is
            // still returned by QueryDb::get directly.
            return Ok(());
        }

        let raw_key_bytes: Arc<[u8]> = Arc::from(raw_key.into_boxed_slice());
        let raw_result_bytes: Arc<[u8]> = Arc::from(raw_value.into_boxed_slice());

        let result = CachedResult::new_persistent(
            value,
            file_deps,
            edge_revision,
            metadata_revision,
            raw_key_bytes,
            raw_result_bytes,
            Q::QUERY_TYPE_ID,
        );

        let mut shard = self.shards[shard_idx].write();
        shard.insert(query_key, result);
        Ok(())
    }

    /// Removes a specific key from a shard.
    pub fn remove(&self, shard_idx: usize, key: &QueryKey) -> bool {
        let mut shard = self.shards[shard_idx].write();
        shard.remove(key).is_some()
    }

    /// Clears all entries from all shards.
    pub fn clear_all(&self) {
        for shard in &self.shards {
            shard.write().clear();
        }
    }

    /// Returns the total number of cached entries across all shards.
    #[must_use]
    pub fn total_entries(&self) -> usize {
        self.shards.iter().map(|s| s.read().len()).sum()
    }

    /// Returns per-shard entry counts for diagnostics.
    #[must_use]
    pub fn shard_entry_counts(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.read().len()).collect()
    }

    /// Inserts a pre-validated cold-load entry using raw bytes only.
    ///
    /// Bypasses the typed deserialise path because at cold-load time only the
    /// raw postcard bytes from disk are available — no typed value has been
    /// decoded. The shard is selected by hashing `raw_key_bytes`.
    ///
    /// **Infallible by construction**: uses only `HashMap::insert`. Called
    /// exclusively from [`QueryDb::commit_staged_load`] — the single
    /// infallible commit boundary in LOAD_PATH.
    ///
    /// # Typed vs raw-only entries
    ///
    /// Entries inserted here have `value = Box::new(())` (a unit placeholder)
    /// because the typed value is not available at cold-load time. The cache
    /// entry is still valid for persistence re-export (raw bytes are
    /// populated and `persistent = true`) but `get_if_valid::<V>` will
    /// return `None` for any typed `V` on first access — the cache will
    /// transparently recompute and replace the entry with a fully-typed
    /// version at that point.
    pub(crate) fn insert_validated(
        &self,
        query_type_id: u32,
        raw_key_bytes: Arc<[u8]>,
        raw_result_bytes: Arc<[u8]>,
        deps: crate::persistence::QueryDeps,
    ) {
        // INVARIANT: all calls below are infallible — see spec §5.7
        //
        // Shard selection MUST match warm-path `QueryRegistry::shard_for::<Q>`
        // so that a later typed `QueryDb::get::<Q>(&key)` probes the same
        // shard this cold-load insert is writing to. Both paths route by
        // `u64::from(Q::QUERY_TYPE_ID) & (shard_count - 1)`.
        use std::hash::{Hash, Hasher};
        let shard_idx =
            crate::query::QueryRegistry::shard_for_query_type_id(query_type_id, self.shards.len());

        // Key hash MUST also match warm-path `QueryKey::new::<Q>(&key)` so
        // that `get::<Q>` finds the rehydrated entry on the FIRST call. Warm
        // path hashes `postcard::to_allocvec(&key)`; cold-load hashes
        // `raw_key_bytes`, which IS that same postcard encoding (set by
        // `insert_query`).
        let mut hasher = std::hash::DefaultHasher::new();
        raw_key_bytes.hash(&mut hasher);
        let hash = hasher.finish();

        let file_deps: SmallVec<[crate::dependency::FileDep; 8]> =
            deps.file_deps.iter().copied().collect();

        // Store a placeholder unit value — the typed value will be populated
        // on the first typed cache hit via QueryDb::get.
        let result = CachedResult {
            value: Box::new(()),
            file_deps,
            edge_revision: deps.edge_revision,
            metadata_revision: deps.metadata_revision,
            raw_key_bytes,
            raw_result_bytes,
            query_type_id,
            persistent: true,
        };

        // QueryKey for the cold-load entry: the same `(type_hash, key_hash)`
        // pair `QueryKey::new::<Q>(&key)` produces on the warm path —
        // `type_hash = u64::from(Q::QUERY_TYPE_ID)`, `key_hash =
        // hash(postcard_encoding(&key))`. This is what fulfils the spec §2
        // promise that "the first query after a cold start is free": a
        // typed `QueryDb::get::<Q>(&key)` issued immediately after
        // `load_derived` finds this entry on its first lookup.
        //
        // Typed-value reconstruction note: `insert_validated` stores a unit
        // placeholder in the `value: Box<dyn Any>` slot because cold-load
        // does not know `Q` and therefore cannot deserialise
        // `raw_result_bytes` into `Q::Value`. On the first typed `get::<Q>`,
        // `get_if_valid` passes the `CachedResult` to the validator — if
        // the revision tiers pass but the typed downcast fails, the caller
        // (`QueryDb::get`) decodes `raw_result_bytes` via
        // `postcard::from_bytes::<Q::Value>` and replaces the placeholder
        // in-place with the properly typed value. That promotion path is
        // implemented in the `get::<Q>` body.
        let shard_key = QueryKey::from_raw(u64::from(query_type_id), hash);

        let mut shard = self.shards[shard_idx].write();
        shard.insert(shard_key, result);
    }

    /// Yields all persistent cache entries as [`PersistableEntry`] values.
    ///
    /// This is the feed for the SAVE_PATH persistence unit.
    ///
    /// # Implementation note
    ///
    /// To avoid holding shard locks during I/O, the method collects cheap
    /// `Arc` clones within each shard lock, then releases the lock before
    /// yielding from the collected `Vec`. No bytes are copied — the `Arc`
    /// reference counts are simply incremented.
    // SAVE_PATH (the next DAG unit) calls this method. Allow dead-code until then.
    #[allow(dead_code)]
    pub(crate) fn iter_persistent(&self) -> impl Iterator<Item = PersistableEntry> + '_ {
        self.shards.iter().flat_map(|shard| {
            // Take the read lock, collect persistent entries as cheap Arc clones,
            // then drop the lock before yielding.
            let guard = shard.read();
            let entries: Vec<PersistableEntry> = guard
                .values()
                .filter(|e| e.persistent)
                .map(|e| PersistableEntry {
                    query_type_id: e.query_type_id,
                    raw_key_bytes: Arc::clone(&e.raw_key_bytes),
                    raw_result_bytes: Arc::clone(&e.raw_result_bytes),
                    // Convert SmallVec to Vec for the serialisable QueryDeps type.
                    deps: QueryDeps {
                        file_deps: e.file_deps.to_vec(),
                        edge_revision: e.edge_revision,
                        metadata_revision: e.metadata_revision,
                    },
                })
                .collect();
            drop(guard);
            entries.into_iter()
        })
    }
}

// SAFETY: All mutation is behind `parking_lot::RwLock`.
unsafe impl Send for ShardedCache {}
unsafe impl Sync for ShardedCache {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use sqry_core::graph::unified::concurrent::CodeGraph;

    use sqry_core::graph::unified::file::id::FileId;

    use crate::query::QueryKey;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    fn empty_snapshot() -> Arc<sqry_core::graph::unified::concurrent::GraphSnapshot> {
        Arc::new(CodeGraph::new().snapshot())
    }

    // Test query: persistent, with serialisable key + value.
    struct PersistentTestQuery;

    #[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone)]
    struct PersistentTestKey(u32);

    impl DerivedQuery for PersistentTestQuery {
        type Key = PersistentTestKey;
        type Value = Vec<u8>;
        const QUERY_TYPE_ID: u32 = 0xF100;
        const PERSISTENT: bool = true;

        fn execute(
            _key: &Self::Key,
            _db: &crate::QueryDb,
            _snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
        ) -> Self::Value {
            vec![]
        }
    }

    // Test query: non-persistent.
    struct NonPersistentTestQuery;

    #[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone)]
    struct NonPersistentTestKey(u32);

    impl DerivedQuery for NonPersistentTestQuery {
        type Key = NonPersistentTestKey;
        type Value = String;
        const QUERY_TYPE_ID: u32 = 0xF101;
        const PERSISTENT: bool = false;

        fn execute(
            key: &Self::Key,
            _db: &crate::QueryDb,
            _snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
        ) -> Self::Value {
            format!("result_{}", key.0)
        }
    }

    // ---------------------------------------------------------------------------
    // Original tests (preserved, using bare CachedResult::new)
    // ---------------------------------------------------------------------------

    #[test]
    fn sharded_cache_basic_ops() {
        let cache = ShardedCache::new(4);
        assert_eq!(cache.shard_count(), 4);
        assert_eq!(cache.total_entries(), 0);

        let key = QueryKey::from_raw(42, 0);
        let result = CachedResult::new(vec![1u32, 2, 3], SmallVec::new(), None, None);

        cache.insert(0, key.clone(), result);
        assert_eq!(cache.total_entries(), 1);

        let val: Option<Vec<u32>> = cache.get_if_valid(0, &key, |_| true);
        assert_eq!(val, Some(vec![1u32, 2, 3]));

        assert!(cache.remove(0, &key));
        assert_eq!(cache.total_entries(), 0);
    }

    #[test]
    fn sharded_cache_validation_rejects() {
        let cache = ShardedCache::new(4);
        let key = QueryKey::from_raw(1, 0);
        cache.insert(
            0,
            key.clone(),
            CachedResult::new(42u32, SmallVec::new(), None, None),
        );

        // Validation fails — should return None
        let val: Option<u32> = cache.get_if_valid(0, &key, |_| false);
        assert!(val.is_none());

        // Validation passes
        let val: Option<u32> = cache.get_if_valid(0, &key, |_| true);
        assert_eq!(val, Some(42));
    }

    #[test]
    fn sharded_cache_clear_all() {
        let cache = ShardedCache::new(4);
        for i in 0..4 {
            let key = QueryKey::from_raw(i as u64, 0);
            cache.insert(i, key, CachedResult::new(i, SmallVec::new(), None, None));
        }
        assert_eq!(cache.total_entries(), 4);
        cache.clear_all();
        assert_eq!(cache.total_entries(), 0);
    }

    #[test]
    fn cached_result_validates_file_deps() {
        let mut store = crate::input::FileInputStore::new();
        store.insert(
            FileId::new(1),
            crate::input::FileInput::new(Default::default()),
        );
        store.insert(
            FileId::new(2),
            crate::input::FileInput::new(Default::default()),
        );

        let mut deps: SmallVec<[FileDep; 8]> = SmallVec::new();
        deps.push((FileId::new(1), 1)); // matches initial revision
        deps.push((FileId::new(2), 1));

        let result = CachedResult::new(42u32, deps, None, None);
        assert!(result.validate_file_deps(&store));

        // Bump file 1's revision
        store
            .get_mut(FileId::new(1))
            .unwrap()
            .update(Default::default());
        assert!(
            !result.validate_file_deps(&store),
            "should invalidate after revision bump"
        );
    }

    #[test]
    #[should_panic(expected = "is_power_of_two")]
    fn sharded_cache_rejects_non_power_of_two() {
        let _ = ShardedCache::new(3);
    }

    #[test]
    fn shard_entry_counts() {
        let cache = ShardedCache::new(4);
        cache.insert(
            0,
            QueryKey::from_raw(1, 0),
            CachedResult::new(1u32, SmallVec::new(), None, None),
        );
        cache.insert(
            0,
            QueryKey::from_raw(2, 0),
            CachedResult::new(2u32, SmallVec::new(), None, None),
        );
        cache.insert(
            2,
            QueryKey::from_raw(3, 0),
            CachedResult::new(3u32, SmallVec::new(), None, None),
        );

        let counts = cache.shard_entry_counts();
        assert_eq!(counts, vec![2, 0, 1, 0]);
    }

    // ---------------------------------------------------------------------------
    // New tests: insert_query (typed, raw-byte retention)
    // ---------------------------------------------------------------------------

    fn default_config() -> QueryDbConfig {
        QueryDbConfig::default()
    }

    /// CachedResult::new leaves raw bytes empty and persistent=false.
    #[test]
    fn cached_result_new_has_empty_raw_bytes() {
        let r = CachedResult::new(42u32, SmallVec::new(), None, None);
        assert!(r.raw_key_bytes().is_empty());
        assert!(r.raw_result_bytes().is_empty());
        assert_eq!(r.query_type_id(), 0);
        assert!(!r.persistent());
    }

    /// A persistent insert stores raw bytes, sets query_type_id, and is
    /// visible to iter_persistent.
    #[test]
    fn insert_query_persistent_stores_raw_bytes() {
        let cache = ShardedCache::new(4);
        let cfg = default_config();

        let key = PersistentTestKey(7);
        let value: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let query_key = QueryKey::new::<PersistentTestQuery>(&key);
        let shard_idx = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<PersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };

        cache
            .insert_query::<PersistentTestQuery>(
                shard_idx,
                query_key.clone(),
                &key,
                value.clone(),
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .expect("insert_query should not fail");

        // Typed value still retrievable.
        let got: Option<Vec<u8>> = cache.get_if_valid(shard_idx, &query_key, |_| true);
        assert_eq!(got, Some(value));

        // iter_persistent yields this entry.
        let persistent: Vec<_> = cache.iter_persistent().collect();
        assert_eq!(persistent.len(), 1);
        assert_eq!(persistent[0].query_type_id, 0xF100);
        assert!(!persistent[0].raw_key_bytes.is_empty());
        assert!(!persistent[0].raw_result_bytes.is_empty());
    }

    /// Oversize entries are silently skipped: get returns None (cache miss)
    /// and iter_persistent yields nothing.
    #[test]
    fn insert_query_oversize_entry_skipped() {
        let cache = ShardedCache::new(4);
        // Max = 1024 bytes; we'll insert a value that serialises to ~2048 bytes.
        let cfg = QueryDbConfig::builder().max_entry_size_bytes(1024).build();

        let key = PersistentTestKey(99);
        // 2048 bytes of payload → postcard adds a varint length header; still > 1024.
        let value: Vec<u8> = vec![0xABu8; 2048];
        let query_key = QueryKey::new::<PersistentTestQuery>(&key);
        let shard_idx = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<PersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };

        cache
            .insert_query::<PersistentTestQuery>(
                shard_idx,
                query_key.clone(),
                &key,
                value,
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .expect("oversize soft-skip must still return Ok");

        // Entry was NOT stored → cache miss.
        let got: Option<Vec<u8>> = cache.get_if_valid(shard_idx, &query_key, |_| true);
        assert!(got.is_none(), "oversize entry must not be present in cache");

        // iter_persistent must not yield the oversized entry.
        let persistent: Vec<_> = cache.iter_persistent().collect();
        assert!(
            persistent.is_empty(),
            "oversize entry must not appear in iter_persistent"
        );
    }

    /// Non-persistent queries: insert succeeds, get returns value, but
    /// iter_persistent skips the entry and raw_key_bytes is empty.
    ///
    /// This is verified indirectly via insert_query then iter_persistent count.
    #[test]
    fn insert_query_non_persistent_invisible_to_iter_persistent() {
        let cache = ShardedCache::new(4);
        let cfg = default_config();

        let key = NonPersistentTestKey(42);
        let value = "hello".to_owned();
        let query_key = QueryKey::new::<NonPersistentTestQuery>(&key);
        let shard_idx = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<NonPersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };

        cache
            .insert_query::<NonPersistentTestQuery>(
                shard_idx,
                query_key.clone(),
                &key,
                value.clone(),
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .expect("non-persistent insert must succeed");

        // Typed value is retrievable.
        let got: Option<String> = cache.get_if_valid(shard_idx, &query_key, |_| true);
        assert_eq!(got, Some(value));

        // Raw bytes on the stored entry are empty.
        {
            let shard = cache.shards[shard_idx].read();
            let entry = shard.get(&query_key).expect("entry must be present");
            assert!(
                entry.raw_key_bytes().is_empty(),
                "non-persistent entry must have empty raw_key_bytes"
            );
            assert!(
                entry.raw_result_bytes().is_empty(),
                "non-persistent entry must have empty raw_result_bytes"
            );
            assert!(
                !entry.persistent(),
                "PERSISTENT=false must set persistent=false"
            );
        }

        // iter_persistent must skip it.
        let persistent: Vec<_> = cache.iter_persistent().collect();
        assert!(
            persistent.is_empty(),
            "non-persistent entry must not appear in iter_persistent"
        );
    }

    /// Edge-revision and metadata-revision propagate correctly to
    /// PersistableEntry.deps.
    #[test]
    fn insert_query_deps_propagated_to_persistable_entry() {
        let cache = ShardedCache::new(4);
        let cfg = default_config();

        let key = PersistentTestKey(1);
        let value: Vec<u8> = vec![1, 2, 3];
        let query_key = QueryKey::new::<PersistentTestQuery>(&key);
        let shard_idx = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<PersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };

        let mut file_deps: SmallVec<[FileDep; 8]> = SmallVec::new();
        file_deps.push((FileId::new(10), 5));

        cache
            .insert_query::<PersistentTestQuery>(
                shard_idx,
                query_key,
                &key,
                value,
                file_deps,
                Some(42),
                Some(7),
                &cfg,
            )
            .expect("insert_query should succeed");

        let entries: Vec<_> = cache.iter_persistent().collect();
        assert_eq!(entries.len(), 1);

        let deps = &entries[0].deps;
        assert_eq!(deps.file_deps.len(), 1);
        assert_eq!(deps.file_deps[0], (FileId::new(10), 5));
        assert_eq!(deps.edge_revision, Some(42));
        assert_eq!(deps.metadata_revision, Some(7));
    }

    /// Insert two persistent and one non-persistent; iter_persistent yields
    /// exactly two entries.
    #[test]
    fn iter_persistent_counts_correctly() {
        let cache = ShardedCache::new(4);
        let cfg = default_config();

        // Persistent 1
        let k1 = PersistentTestKey(1);
        let qk1 = QueryKey::new::<PersistentTestQuery>(&k1);
        let si1 = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<PersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };
        cache
            .insert_query::<PersistentTestQuery>(
                si1,
                qk1,
                &k1,
                vec![1u8],
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .unwrap();

        // Persistent 2
        let k2 = PersistentTestKey(2);
        let qk2 = QueryKey::new::<PersistentTestQuery>(&k2);
        cache
            .insert_query::<PersistentTestQuery>(
                si1,
                qk2,
                &k2,
                vec![2u8],
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .unwrap();

        // Non-persistent
        let nk = NonPersistentTestKey(3);
        let nqk = QueryKey::new::<NonPersistentTestQuery>(&nk);
        let nsi = {
            use std::hash::{Hash, Hasher};
            let tid = std::any::TypeId::of::<NonPersistentTestQuery>();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tid.hash(&mut h);
            (h.finish() & 3) as usize
        };
        cache
            .insert_query::<NonPersistentTestQuery>(
                nsi,
                nqk,
                &nk,
                "skip".to_owned(),
                SmallVec::new(),
                None,
                None,
                &cfg,
            )
            .unwrap();

        let count = cache.iter_persistent().count();
        assert_eq!(count, 2, "only the two persistent entries should appear");
    }

    // Verify that the test helper is used to silence the unused-import warning.
    #[test]
    fn empty_snapshot_compiles() {
        let _ = empty_snapshot();
    }
}
