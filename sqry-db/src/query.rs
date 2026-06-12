//! Derived query trait and `TypeId`-based query registry.
//!
//! All analysis queries implement [`DerivedQuery`]. The registry maps each
//! query type's `TypeId` to a shard assignment in the [`ShardedCache`].
//! Registration is explicit — no proc macros.

use std::any::TypeId;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use sqry_core::graph::unified::concurrent::GraphSnapshot;

use crate::QueryDb;

/// A derived query whose result is memoized in the [`ShardedCache`].
///
/// Concrete queries implement this trait. `QueryDb::get::<Q>(key)` checks
/// the cache, validates all applicable dependency tiers, and either returns
/// the cached value or calls `Q::execute`, records dependencies, and caches.
///
/// # Dependency tracking
///
/// During `execute`, the query should call [`record_file_dep`](crate::dependency::record_file_dep)
/// for every `FileId` it reads from. The thread-local recorder collects these
/// and pairs them with revision numbers after execution completes.
///
/// # Tier flags
///
/// - `TRACKS_EDGE_REVISION`: Set to `true` for queries that answer "who points
///   at X" (callers, cycle membership, reachability). Invalidated when ANY
///   file's edges change.
/// - `TRACKS_METADATA_REVISION`: Set to `true` for queries that read node
///   metadata (entry-point classification for `find_unused`). Invalidated when
///   node metadata changes without edge changes.
pub trait DerivedQuery: Send + Sync + 'static {
    /// The input key for this query.
    ///
    /// The `serde::Serialize` bound is required by the PN3 persistence layer:
    /// [`ShardedCache::insert_query`] serialises the key to raw bytes at insert
    /// time so that the key can be round-tripped to disk without re-running the
    /// query. All built-in query keys already implement `Serialize`; downstream
    /// or test keys using primitive types (`u32`, `String`, etc.) also satisfy
    /// this bound automatically.
    type Key: Hash + Eq + Clone + Send + serde::Serialize + 'static;
    /// The output value. Must be `Clone` for cache retrieval.
    ///
    /// The `serde::Serialize` bound is required by the PN3 persistence layer
    /// so that warm-path writes can be saved to disk.
    /// The `serde::de::DeserializeOwned` bound is required by the PN3 cold-load
    /// path: when a rehydrated entry is first found by a typed `get::<Q>`, the
    /// raw result bytes must be decoded back into `Q::Value` before they can
    /// be returned to the caller. Without this bound, "first query after cold
    /// start is free" would degrade into "first query misses, second is free",
    /// which violates spec §2.
    /// Non-serialisable value types must use `PERSISTENT = false` to opt out
    /// of raw-byte retention.
    type Value: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static;

    /// Stable on-disk discriminator for PN3 derived-cache persistence.
    ///
    /// This `u32` is the key written to `.sqry/graph/derived.sqry` to
    /// identify the query type across process restarts. Unlike
    /// `TypeId::of::<Self>()`, which is process-local and not stable
    /// across compiler versions, this ID is fixed at authoring time and
    /// **must never change or be reused** once any derived cache file
    /// containing it has been written.
    ///
    /// # Reserved ranges
    ///
    /// - `0x0000` — invalid / unknown. Must never appear on disk.
    /// - `0x0001..=0x000F` — built-in queries (15 assigned).
    /// - `0x0010..=0x0FFF` — reserved for additional built-ins.
    /// - `0x1000..=0xFFFF` — reserved for downstream / future built-ins.
    ///
    /// See `sqry_db::queries::type_ids` for the canonical constants.
    /// There is no default value: every implementation must set this
    /// explicitly to prevent accidental ID collisions.
    const QUERY_TYPE_ID: u32;

    /// Whether this query's `Value` satisfies the serde bounds required
    /// for PN3 cold-start persistence.
    ///
    /// Set to `false` for queries whose `Value` type cannot (or should
    /// not) be serialized to disk. When `false`, the PN3 persistence
    /// layer skips this query entirely and will recompute it on demand
    /// after a cold start.
    ///
    /// The default is `true`; implementations opt out by overriding.
    const PERSISTENT: bool = true;

    /// Whether this query depends on the global edge topology.
    ///
    /// When `true`, the cached result is invalidated whenever the global edge
    /// revision counter advances (even if the specific files the query read
    /// haven't changed). This handles the "missing negative dependency"
    /// problem: a new file could add an edge that the query never saw.
    const TRACKS_EDGE_REVISION: bool = false;

    /// Whether this query depends on node metadata.
    ///
    /// When `true`, the cached result is invalidated whenever the global
    /// metadata revision counter advances.
    const TRACKS_METADATA_REVISION: bool = false;

    /// Computes the query result.
    ///
    /// Implementations MUST call [`record_file_dep`](crate::dependency::record_file_dep)
    /// for every `FileId` they read from the snapshot.
    fn execute(key: &Self::Key, db: &QueryDb, snapshot: &GraphSnapshot) -> Self::Value;
}

/// A type-erased query cache key combining the query type's stable
/// `QUERY_TYPE_ID` discriminator and the hash of the postcard encoding of the
/// input key.
///
/// Key equality is **build/process-independent**: both components derive from
/// values that are stable across restarts, enabling PN3's `load_derived` to
/// rehydrate entries directly into the same cache slots warm-path
/// `insert_query` and `get` use.
#[derive(Clone, Eq, PartialEq)]
pub struct QueryKey {
    /// `Q::QUERY_TYPE_ID` (the stable u32 discriminator from spec §5.1) widened
    /// to `u64`. Shared between warm-path `QueryKey::new::<Q>` and cold-path
    /// `ShardedCache::insert_validated`.
    type_hash: u64,
    /// Hash of `postcard::to_allocvec(&key)` bytes. Shared between warm-path
    /// and cold-path for the same reason.
    key_hash: u64,
}

impl QueryKey {
    /// Creates a new query key from a concrete query type and input key.
    ///
    /// Both components of the key are computed from **stable, process-independent**
    /// sources so that entries rehydrated by PN3's cold-load path
    /// (`ShardedCache::insert_validated`, which has no typed `Q` and works from
    /// raw bytes alone) live in the SAME key space as warm-path entries. This
    /// is what makes "first query after a cold start is free" actually hold:
    ///
    /// - `type_hash` is `Q::QUERY_TYPE_ID as u64` (the stable on-disk
    ///   discriminator defined in spec §5.1), **not** `TypeId::of::<Q>()` which
    ///   is build/process-local.
    /// - `key_hash` is a hash of the postcard encoding of `key`, matching what
    ///   `ShardedCache::insert_query` already stores as `raw_key_bytes` and what
    ///   `ShardedCache::insert_validated` hashes at cold-load time.
    ///
    /// Deterministic serialization of the key is guaranteed by the
    /// `serde::Serialize` bound on `DerivedQuery::Key`; a panic on serialization
    /// failure would indicate a serde derive bug and is treated as a
    /// non-recoverable programmer error here.
    pub fn new<Q: DerivedQuery>(key: &Q::Key) -> Self {
        let type_hash = u64::from(Q::QUERY_TYPE_ID);
        let key_hash = Self::hash_serialized_key(key);
        Self {
            type_hash,
            key_hash,
        }
    }

    /// Creates a raw query key for testing purposes.
    #[doc(hidden)]
    #[must_use]
    pub fn from_raw(type_hash: u64, key_hash: u64) -> Self {
        Self {
            type_hash,
            key_hash,
        }
    }

    /// Hash the postcard encoding of a key. This matches the key-space used by
    /// `ShardedCache::insert_validated` at cold-load time: both paths hash the
    /// same byte sequence, so a typed `get::<Q>(&key)` issued immediately after
    /// `load_derived` will find the rehydrated entry on the **first** call.
    fn hash_serialized_key<K: serde::Serialize>(key: &K) -> u64 {
        let bytes = postcard::to_allocvec(key).expect(
            "DerivedQuery::Key requires serde::Serialize to be infallible; \
             postcard::to_allocvec must not fail",
        );
        let mut hasher = std::hash::DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }
}

impl Hash for QueryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.type_hash.hash(state);
        self.key_hash.hash(state);
    }
}

impl std::fmt::Debug for QueryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryKey")
            .field("type_hash", &format!("{:#018x}", self.type_hash))
            .field("key_hash", &format!("{:#018x}", self.key_hash))
            .finish()
    }
}

/// Registry mapping query types to shard assignments.
///
/// No macro magic. Queries are registered at startup via
/// `QueryDb::register::<MyQuery>()`. The registry maps `TypeId` to a shard
/// index derived from the `TypeId`'s hash.
pub struct QueryRegistry {
    /// Maps `TypeId` → shard index. Used to route queries to their assigned shard.
    assignments: HashMap<TypeId, usize>,
}

impl QueryRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            assignments: HashMap::new(),
        }
    }

    /// Registers a query type for cache routing.
    ///
    /// Idempotent: re-registering the same query type is a no-op. DB14
    /// relies on this so the built-in queries pre-registered by
    /// [`QueryDb::new`][crate::QueryDb::new] do not conflict with legacy
    /// call-sites (or tests) that also call [`QueryDb::register`].
    pub fn register<Q: DerivedQuery>(&mut self) {
        let tid = TypeId::of::<Q>();
        // Shard assignment is deferred until shard_for() is called,
        // but we record the TypeId so registered_count() stays accurate.
        // The actual shard index is computed from the TypeId hash modulo
        // the shard count.
        self.assignments.entry(tid).or_insert(0);
    }

    /// Returns the shard index for a query type.
    ///
    /// The shard index is deterministic: `u64::from(Q::QUERY_TYPE_ID) &
    /// (shard_count - 1)`. This ensures the same query type always maps to
    /// the same shard **across process boundaries** — critical for PN3
    /// cold-load, where `ShardedCache::insert_validated` places rehydrated
    /// entries into the exact shard a later typed `QueryDb::get::<Q>` will
    /// probe. `shard_count` is required to be a power of two.
    #[must_use]
    pub fn shard_for<Q: DerivedQuery>(&self, shard_count: usize) -> usize {
        Self::shard_for_query_type_id(Q::QUERY_TYPE_ID, shard_count)
    }

    /// Type-erased shard-routing helper. Given a `query_type_id` (equivalent to
    /// `Q::QUERY_TYPE_ID`), returns the shard index. Used by PN3 cold-load
    /// (`ShardedCache::insert_validated`) where the concrete `Q` is not known.
    #[must_use]
    pub fn shard_for_query_type_id(query_type_id: u32, shard_count: usize) -> usize {
        let mask = u64::try_from(shard_count - 1).unwrap_or(u64::MAX);
        let shard = u64::from(query_type_id) & mask;
        usize::try_from(shard).unwrap_or_default()
    }

    /// Returns the number of registered query types.
    #[must_use]
    pub fn registered_count(&self) -> usize {
        self.assignments.len()
    }
}

impl Default for QueryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test query type for unit testing
    struct TestQuery;

    impl DerivedQuery for TestQuery {
        type Key = u32;
        type Value = String;
        const QUERY_TYPE_ID: u32 = 0xF001;

        fn execute(key: &u32, _db: &QueryDb, _snapshot: &GraphSnapshot) -> String {
            format!("result_{key}")
        }
    }

    struct EdgeTrackingQuery;

    impl DerivedQuery for EdgeTrackingQuery {
        type Key = u32;
        type Value = Vec<u32>;
        const QUERY_TYPE_ID: u32 = 0xF002;
        const TRACKS_EDGE_REVISION: bool = true;

        fn execute(_key: &u32, _db: &QueryDb, _snapshot: &GraphSnapshot) -> Vec<u32> {
            vec![]
        }
    }

    #[test]
    fn query_key_equality() {
        let k1 = QueryKey::new::<TestQuery>(&42);
        let k2 = QueryKey::new::<TestQuery>(&42);
        let k3 = QueryKey::new::<TestQuery>(&99);

        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn query_key_different_types_differ() {
        let k1 = QueryKey::new::<TestQuery>(&42);
        let k2 = QueryKey::new::<EdgeTrackingQuery>(&42);
        assert_ne!(
            k1, k2,
            "different query types should produce different keys"
        );
    }

    #[test]
    fn registry_shard_assignment_deterministic() {
        let reg = QueryRegistry::new();
        let s1 = reg.shard_for::<TestQuery>(64);
        let s2 = reg.shard_for::<TestQuery>(64);
        assert_eq!(s1, s2);
        assert!(s1 < 64);
    }

    #[test]
    fn registry_register_and_count() {
        let mut reg = QueryRegistry::new();
        assert_eq!(reg.registered_count(), 0);
        reg.register::<TestQuery>();
        assert_eq!(reg.registered_count(), 1);
        reg.register::<EdgeTrackingQuery>();
        assert_eq!(reg.registered_count(), 2);
    }

    /// DB14 + Codex review (2026-04-15): repeat registration of the same
    /// query type is now a no-op so that legacy / test call sites that
    /// invoke `db.register::<Q>()` after the constructor's built-in
    /// pre-registration do not trip a debug assert. Verify that:
    ///  * count stays stable across repeats,
    ///  * `shard_for` keeps returning the same deterministic index.
    #[test]
    fn registry_register_is_idempotent() {
        let mut reg = QueryRegistry::new();
        reg.register::<TestQuery>();
        let before_shard = reg.shard_for::<TestQuery>(64);
        let before_count = reg.registered_count();

        reg.register::<TestQuery>();
        reg.register::<TestQuery>();
        reg.register::<TestQuery>();

        assert_eq!(
            reg.registered_count(),
            before_count,
            "duplicate register::<Q>() must not bump registered_count"
        );
        assert_eq!(
            reg.shard_for::<TestQuery>(64),
            before_shard,
            "duplicate register::<Q>() must not change shard routing"
        );
    }
}
