//! `sqry-db`: Salsa-style incremental computation engine for sqry.
//!
//! This crate provides a demand-driven memoized query engine with file-level
//! dependency tracking, three-tier cache invalidation, and incremental indexing
//! support. All analysis queries (callers, callees, dependency impact, unused
//! detection, etc.) are expressed as [`DerivedQuery`] implementations whose
//! results are cached in a 64-shard [`ShardedCache`] and automatically
//! invalidated when their input dependencies change.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐      ┌──────────────────┐      ┌──────────────���─┐
//! │  MCP / CLI   │─────▶│     QueryDb      │─────▶│  GraphSnapshot │
//! │  handlers    │      │  (ShardedCache)   │      │  (immutable)   │
//! └─────────────┘      └──────────────────┘      └────���───────────┘
//!                             │                          │
//!                       ┌─────┴──────┐           ┌──────┴───────┐
//!                       │ FileInput  │           │  NodeArena   │
//!                       │   Store    │           │  EdgeStore   │
//!                       └────────────┘           └──────────────┘
//! ```
//!
//! # Three-Tier Invalidation
//!
//! - **Tier 1 — File-level deps**: Every query records the `FileId` of every
//!   node it reads. Invalidated when any recorded file's revision bumps.
//! - **Tier 2 — Global edge revision**: A single `AtomicU64` that increments
//!   when *any* file's edges change. Reverse queries (`callers_of`,
//!   `is_node_in_cycle`) set `TRACKS_EDGE_REVISION = true`.
//! - **Tier 3 — Global metadata revision**: A single `AtomicU64` that
//!   increments when node metadata changes. `find_unused` sets
//!   `TRACKS_METADATA_REVISION = true`.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_db::{QueryDb, QueryDbConfig};
//!
//! let config = QueryDbConfig::default();
//! let db = QueryDb::new(snapshot, config);
//! let result = db.get::<CallersQuery>(&node_id);
//! ```

pub mod cache;
pub mod compaction;
pub mod comparative;
pub mod config;
pub mod dependency;
pub mod input;
pub mod persistence;
pub mod planner;
pub mod queries;
pub mod query;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sqry_core::graph::unified::concurrent::GraphSnapshot;

pub use cache::ShardedCache;
pub use comparative::{
    ChangeType, ComparativeQueryDb, DiffOptions, DiffOutput, DiffSummary, NodeChange, NodeLocation,
};
pub use config::QueryDbConfig;
pub use dependency::DependencyRecorderGuard;
pub use input::{FileInput, FileInputStore};
pub use persistence::{
    DERIVED_FORMAT_VERSION, DERIVED_MAGIC, DerivedHeader, DerivedManifest, LoadError, LoadOutcome,
    PersistedEntry, QueryDeps, SkipReason, StagedEntry, load_derived,
};
pub use queries::dispatch::{derived_path, load_derived_opportunistic, make_query_db_cold};
pub use query::{DerivedQuery, QueryKey, QueryRegistry};
// `QueryDb` and `QueryDbMetrics` are defined in this module; re-exported
// at the crate root so callers can `use sqry_db::{QueryDb, QueryDbMetrics}`
// without qualifying the module path.

/// The central incremental computation database.
///
/// `QueryDb` owns the sharded query cache, file-level input store, and global
/// revision counters. All derived queries are evaluated and cached through
/// [`QueryDb::get`].
///
/// # Concurrency
///
/// `QueryDb` is `Send + Sync`. The sharded cache uses `parking_lot::RwLock`
/// per shard (64 shards) for high-concurrency reads. Global revision counters
/// use `AtomicU64` with `Ordering::Acquire` / `Ordering::Release` semantics.
pub struct QueryDb {
    /// Per-file fact sets with revision counters.
    inputs: FileInputStore,
    /// 64-shard query cache.
    cache: ShardedCache,
    /// Global edge revision counter.
    ///
    /// Incremented when *any* file's edges change during `reindex_files`.
    /// Queries with `TRACKS_EDGE_REVISION = true` are invalidated when this
    /// counter advances past their cached `edge_revision` value.
    edge_revision: AtomicU64,
    /// Global node metadata revision counter.
    ///
    /// Incremented when node metadata (visibility, entry-point classification,
    /// node kind) changes. Queries with `TRACKS_METADATA_REVISION = true` are
    /// invalidated when this counter advances.
    metadata_revision: AtomicU64,
    /// Query type registry mapping `TypeId` to shard assignments.
    registry: QueryRegistry,
    /// Reference to the current graph snapshot.
    snapshot: Arc<GraphSnapshot>,
    /// Configuration for cache behavior, compaction thresholds, etc.
    config: QueryDbConfig,
    /// Cumulative cache hits — incremented on every `get()` that returns a
    /// cached value without invoking `Q::execute`.
    ///
    /// Exposed for proof-point tests (Phase 3C DB21 Proof 3) and production
    /// telemetry. Use [`Self::metrics`] to read a coherent snapshot.
    cache_hits: AtomicU64,
    /// Cumulative cache misses — incremented on every `get()` that invokes
    /// `Q::execute` because the entry was missing or its dependency tiers
    /// failed validation.
    cache_misses: AtomicU64,
    /// Guards the cold-load window.
    ///
    /// `true` at construction — exactly one successful `load_derived` call is
    /// permitted per `QueryDb` instance. After a successful load this is set
    /// to `false` (via `Release` ordering). Subsequent calls to `load_derived`
    /// return [`persistence::LoadError::AlreadyLoaded`] immediately, without
    /// reading the file, so double-apply is impossible.
    ///
    /// The field is intentionally `pub(crate)` so that the sibling
    /// `persistence` module can check and flip it atomically.
    pub(crate) cold_load_allowed: AtomicBool,
}

/// Lightweight snapshot of `QueryDb` cache-usage counters.
///
/// Returned by [`QueryDb::metrics`]. Counters are monotonic and never reset
/// during the lifetime of a `QueryDb` — tests should read a baseline before
/// the work under test, run the work, read again, and diff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryDbMetrics {
    /// Total cache hits served since this `QueryDb` was constructed.
    pub cache_hits: u64,
    /// Total cache misses (including dependency-invalidated entries) served
    /// since this `QueryDb` was constructed.
    pub cache_misses: u64,
}

impl QueryDbMetrics {
    /// Total `get()` calls observed (`cache_hits + cache_misses`).
    #[inline]
    #[must_use]
    pub fn total_gets(&self) -> u64 {
        self.cache_hits.saturating_add(self.cache_misses)
    }
}

impl QueryDb {
    /// Creates a new `QueryDb` backed by the given graph snapshot.
    ///
    /// Every built-in [`DerivedQuery`] is pre-registered so callers of the
    /// planner executor and the relation / cycle / unused queries never have
    /// to hand-register them. Extra query types can still be added via
    /// [`Self::register`].
    #[must_use]
    pub fn new(snapshot: Arc<GraphSnapshot>, config: QueryDbConfig) -> Self {
        let inputs = FileInputStore::from_snapshot(&snapshot);
        let mut db = Self {
            inputs,
            cache: ShardedCache::new(config.shard_count),
            edge_revision: AtomicU64::new(0),
            metadata_revision: AtomicU64::new(0),
            registry: QueryRegistry::new(),
            snapshot,
            config,
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            cold_load_allowed: AtomicBool::new(true),
        };
        db.register_builtin_queries();
        db
    }

    /// Registers the queries that ship with `sqry-db`.
    ///
    /// Called from [`Self::new`]; exposed as a helper so tests can reuse it
    /// when they set up bespoke `QueryDb` instances.
    fn register_builtin_queries(&mut self) {
        // Foundational queries
        self.registry.register::<queries::SccQuery>();
        self.registry.register::<queries::CondensationQuery>();
        self.registry.register::<queries::ReachabilityQuery>();
        // DB14 relation queries
        self.registry.register::<queries::CallersQuery>();
        self.registry.register::<queries::CalleesQuery>();
        self.registry.register::<queries::ImportsQuery>();
        self.registry.register::<queries::ExportsQuery>();
        self.registry.register::<queries::ReferencesQuery>();
        self.registry.register::<queries::ImplementsQuery>();
        // DB14 analysis queries
        self.registry.register::<queries::CyclesQuery>();
        self.registry.register::<queries::IsInCycleQuery>();
        self.registry.register::<queries::EntryPointsQuery>();
        self.registry
            .register::<queries::ReachableFromEntryPointsQuery>();
        self.registry.register::<queries::UnusedQuery>();
        self.registry.register::<queries::IsNodeUnusedQuery>();
    }

    /// Registers a derived query type for cache routing.
    ///
    /// Must be called once per query type before the first `get` call.
    /// Panics (in debug builds) if the query type is already registered.
    pub fn register<Q: DerivedQuery>(&mut self) {
        self.registry.register::<Q>();
    }

    /// Evaluates a derived query, returning the cached result if valid or
    /// recomputing and caching on miss/invalidation.
    ///
    /// # Cache validation
    ///
    /// A cached result is returned only if ALL applicable tiers pass:
    /// 1. Every `(FileId, revision)` pair in the cached deps matches the
    ///    current file revision in `FileInputStore`.
    /// 2. If `Q::TRACKS_EDGE_REVISION`, the cached `edge_revision` matches
    ///    the current global counter.
    /// 3. If `Q::TRACKS_METADATA_REVISION`, the cached `metadata_revision`
    ///    matches the current global counter.
    pub fn get<Q: DerivedQuery>(&self, key: &Q::Key) -> Q::Value {
        let query_key = QueryKey::new::<Q>(key);
        let shard_idx = self.registry.shard_for::<Q>(self.cache.shard_count());

        // Fast path: typed cached value with valid tiers (warm path).
        if let Some(value) = self
            .cache
            .get_if_valid::<Q::Value>(shard_idx, &query_key, |cached| {
                self.validate_cached_result::<Q>(cached)
            })
        {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return value;
        }

        // Cold-load rehydration path: entry may exist as raw-bytes-only, placed
        // by `ShardedCache::insert_validated` during `load_derived`. If the
        // tiers still validate, decode the raw bytes into `Q::Value` and
        // promote the entry to the typed fast-path for subsequent calls. This
        // is what makes spec §2's "first query after a cold start is free"
        // actually hold: no recomputation is performed when the rehydrated
        // entry is valid.
        if let Some(value) =
            self.cache
                .get_cold_if_valid::<Q::Value>(shard_idx, &query_key, |cached| {
                    self.validate_cached_result::<Q>(cached)
                })
        {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return value;
        }

        // Cache miss or invalidated — recompute
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        let guard = DependencyRecorderGuard::new();
        let value = Q::execute(key, self, &self.snapshot);
        let file_deps = guard.finish(&self.inputs);

        let edge_rev = if Q::TRACKS_EDGE_REVISION {
            Some(self.edge_revision.load(Ordering::Acquire))
        } else {
            None
        };
        let metadata_rev = if Q::TRACKS_METADATA_REVISION {
            Some(self.metadata_revision.load(Ordering::Acquire))
        } else {
            None
        };

        // insert_query serialises key+value for persistent queries and enforces
        // the max_entry_size_bytes cap. For non-persistent queries it stores
        // the typed value with empty raw bytes. For oversized entries it skips
        // caching entirely (soft skip). The caller's computed value is always
        // returned regardless of whether caching succeeded.
        //
        // Serialisation errors are non-fatal: log a warning and fall back to
        // the bare untyped insert so the entry is at least cache-warm for the
        // current session (just not persistent).
        if let Err(err) = self.cache.insert_query::<Q>(
            shard_idx,
            query_key.clone(),
            key,
            value.clone(),
            file_deps.clone(),
            edge_rev,
            metadata_rev,
            &self.config,
        ) {
            log::warn!(
                "sqry-db: failed to serialise cache entry for query_type_id={:#06x}: {err}; \
                 storing typed value without persistence",
                Q::QUERY_TYPE_ID,
            );
            self.cache.insert(
                shard_idx,
                query_key,
                cache::CachedResult::new(value.clone(), file_deps, edge_rev, metadata_rev),
            );
        }

        value
    }

    /// Returns the current graph snapshot.
    #[inline]
    #[must_use]
    pub fn snapshot(&self) -> &GraphSnapshot {
        &self.snapshot
    }

    /// Returns the current graph snapshot as an `Arc`.
    #[inline]
    #[must_use]
    pub fn snapshot_arc(&self) -> Arc<GraphSnapshot> {
        Arc::clone(&self.snapshot)
    }

    /// Replaces the current snapshot with a new one.
    ///
    /// This does NOT invalidate the cache. Use [`bump_edge_revision`] and/or
    /// [`bump_metadata_revision`] after updating to trigger invalidation on
    /// next access.
    pub fn set_snapshot(&mut self, snapshot: Arc<GraphSnapshot>) {
        self.snapshot = snapshot;
    }

    /// Returns a reference to the file input store.
    #[inline]
    #[must_use]
    pub fn inputs(&self) -> &FileInputStore {
        &self.inputs
    }

    /// Returns a mutable reference to the file input store.
    #[inline]
    pub fn inputs_mut(&mut self) -> &mut FileInputStore {
        &mut self.inputs
    }

    /// Returns the current global edge revision counter.
    #[inline]
    #[must_use]
    pub fn edge_revision(&self) -> u64 {
        self.edge_revision.load(Ordering::Acquire)
    }

    /// Atomically increments the global edge revision counter and returns the
    /// new value.
    pub fn bump_edge_revision(&self) -> u64 {
        self.edge_revision.fetch_add(1, Ordering::Release) + 1
    }

    /// Returns the current global metadata revision counter.
    #[inline]
    #[must_use]
    pub fn metadata_revision(&self) -> u64 {
        self.metadata_revision.load(Ordering::Acquire)
    }

    /// Atomically increments the global metadata revision counter and returns
    /// the new value.
    pub fn bump_metadata_revision(&self) -> u64 {
        self.metadata_revision.fetch_add(1, Ordering::Release) + 1
    }

    /// Returns the database configuration.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &QueryDbConfig {
        &self.config
    }

    /// Returns a snapshot of the cache-usage counters.
    ///
    /// The returned [`QueryDbMetrics`] is a coherent read of the hit and miss
    /// counters (modulo the usual memory-ordering semantics of two separate
    /// atomic reads). The counters are monotonic and never reset — callers
    /// interested in deltas should take a baseline, run the work, and diff.
    ///
    /// Used by Phase 3C DB21 Proof 3 to assert "zero recomputation" after a
    /// cache warm-up, and by production telemetry to emit cache hit rates.
    #[inline]
    #[must_use]
    pub fn metrics(&self) -> QueryDbMetrics {
        QueryDbMetrics {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
        }
    }

    /// Invalidates all cached results unconditionally.
    ///
    /// This is a heavy operation used for full rebuilds.
    pub fn invalidate_all(&self) {
        self.cache.clear_all();
    }

    /// Returns `true` if a cold-load is still permitted for this `QueryDb`.
    ///
    /// `true` at construction. Transitions to `false` after a successful
    /// [`persistence::load_derived`] call. Once `false`, subsequent
    /// `load_derived` calls return
    /// [`persistence::LoadError::AlreadyLoaded`] without reading the file.
    #[inline]
    #[must_use]
    pub fn cold_load_allowed(&self) -> bool {
        self.cold_load_allowed.load(Ordering::Acquire)
    }

    /// Apply a validated cold-load staging result. **Infallible by construction.**
    ///
    /// Pre-conditions enforced by the caller (`load_derived`):
    /// - `header.magic == DERIVED_MAGIC`
    /// - `header.format_version == DERIVED_FORMAT_VERSION`
    /// - `header.snapshot_sha256` matched the caller-supplied SHA
    /// - `staged` is the set of valid entries (unknown-ID entries filtered out
    ///   at validation stage)
    ///
    /// Uses only infallible operations:
    /// - [`AtomicU64::store`] for `edge_revision` / `metadata_revision`
    /// - [`input::FileInputStore::restore_revisions`] (infallible `HashMap::insert`)
    /// - [`cache::ShardedCache::insert_validated`] (infallible `HashMap::insert`)
    pub(crate) fn commit_staged_load(
        &mut self,
        header: persistence::DerivedHeader,
        staged: Vec<persistence::StagedEntry>,
    ) {
        // INVARIANT: all calls below are infallible — see spec §5.7
        // No `?`, no `.map_err(...)`, no `Result`-bearing call is permitted here.
        // If you find yourself wanting to return Err from this function, that is
        // a spec violation — move the fallible work into `load_derived` before
        // the commit boundary.

        // Tier 2: restore global edge revision.
        self.edge_revision
            .store(header.edge_revision, Ordering::Release);

        // Tier 3: restore global metadata revision.
        self.metadata_revision
            .store(header.metadata_revision, Ordering::Release);

        // Tier 1: restore per-file revision counters.
        self.inputs.restore_revisions(&header.file_revisions);

        // Warm the cache with raw-byte entries for each staged result.
        for entry in staged {
            self.cache.insert_validated(
                entry.query_type_id,
                entry.raw_key_bytes.into(),
                entry.raw_result_bytes.into(),
                entry.deps,
            );
        }
    }

    /// Yields all persistent cache entries for the SAVE_PATH persistence unit.
    ///
    /// Delegates to [`ShardedCache::iter_persistent`]. This crate-internal
    /// accessor exists to allow `persistence::save_derived` (a sibling module)
    /// to drive the save loop without exposing the raw `cache` field.
    pub(crate) fn iter_persistent_cache_entries(
        &self,
    ) -> impl Iterator<Item = cache::PersistableEntry> + '_ {
        self.cache.iter_persistent()
    }

    /// Validates a cached result against all applicable dependency tiers.
    fn validate_cached_result<Q: DerivedQuery>(&self, cached: &cache::CachedResult) -> bool {
        // Tier 1: File-level deps
        if !cached.validate_file_deps(&self.inputs) {
            return false;
        }

        // Tier 2: Global edge revision
        if Q::TRACKS_EDGE_REVISION {
            if let Some(cached_rev) = cached.edge_revision() {
                if cached_rev != self.edge_revision.load(Ordering::Acquire) {
                    return false;
                }
            } else {
                // Query tracks edge revision but cache entry doesn't have one
                return false;
            }
        }

        // Tier 3: Global metadata revision
        if Q::TRACKS_METADATA_REVISION {
            if let Some(cached_rev) = cached.metadata_revision() {
                if cached_rev != self.metadata_revision.load(Ordering::Acquire) {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

// SAFETY: All internal state is either atomic or behind parking_lot locks.
// The `Arc<GraphSnapshot>` is immutable.
unsafe impl Send for QueryDb {}
unsafe impl Sync for QueryDb {}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::concurrent::CodeGraph;

    #[test]
    fn query_db_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<QueryDb>();
    }

    /// DB14 + Codex review (2026-04-15): `QueryDb::new` pre-registers every
    /// built-in `DerivedQuery`. Legacy / test call sites that re-register
    /// the same built-in after `QueryDb::new()` must be a no-op — the
    /// registry count must stay stable so duplicate registration does not
    /// silently alter shard routing or count semantics.
    #[test]
    fn query_db_new_built_in_registration_is_stable_under_repeats() {
        let snapshot = std::sync::Arc::new(CodeGraph::new().snapshot());
        let mut db = QueryDb::new(snapshot, QueryDbConfig::default());

        let count_after_new = db.registry.registered_count();
        // Re-register every DB14-shipped built-in. Each call must be a no-op.
        db.register::<queries::SccQuery>();
        db.register::<queries::CondensationQuery>();
        db.register::<queries::ReachabilityQuery>();
        db.register::<queries::CallersQuery>();
        db.register::<queries::CalleesQuery>();
        db.register::<queries::ImportsQuery>();
        db.register::<queries::ExportsQuery>();
        db.register::<queries::ReferencesQuery>();
        db.register::<queries::ImplementsQuery>();
        db.register::<queries::CyclesQuery>();
        db.register::<queries::IsInCycleQuery>();
        db.register::<queries::EntryPointsQuery>();
        db.register::<queries::ReachableFromEntryPointsQuery>();
        db.register::<queries::UnusedQuery>();
        db.register::<queries::IsNodeUnusedQuery>();

        assert_eq!(
            db.registry.registered_count(),
            count_after_new,
            "repeat register of built-ins must not bump count"
        );
        // Ensure the count is non-zero (i.e. built-ins were actually
        // registered by the constructor — a regression where new() forgets
        // to call register_builtin_queries() would surface here).
        assert!(
            count_after_new >= 15,
            "QueryDb::new must pre-register at least the 15 DB14 built-ins, got {count_after_new}"
        );
    }
}
