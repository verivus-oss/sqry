//! Graph cache for `trace_path` and subgraph operations
//!
//! Provides in-memory LRU caching with TTL expiration for expensive graph traversals and
//! records telemetry snapshots so MCP clients can understand cache behaviour.

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use super::types::{DependencyGraphData, TracePathData};

/// Cache TTL: 5 minutes (graph structure changes infrequently)
pub const CACHE_TTL_SECS: u64 = 300;
const CACHE_TTL: Duration = Duration::from_secs(CACHE_TTL_SECS);

/// Default max cache entries for `trace_path` operations
///
/// **Deprecated:** Use `McpConfig::trace_cache_size` instead.
/// This constant remains for backward compatibility only.
pub const TRACE_PATH_CACHE_CAPACITY: usize = 256;

/// Default max cache entries for subgraph operations
///
/// **Deprecated:** Use `McpConfig::subgraph_cache_size` instead.
/// This constant remains for backward compatibility only.
pub const SUBGRAPH_CACHE_CAPACITY: usize = 128;

// Histogram configuration: track up to 10 minutes to capture overflow buckets
const HISTOGRAM_MAX_MS: u64 = 600_000; // 10 minutes upper bound for cache latency
const HISTOGRAM_SIGFIGS: u8 = 3;
const HISTOGRAM_BUCKET_BOUNDS: &[u64] = &[
    10, 25, 50, 100, 200, 400, 800, 1600, 3200, 6400, 12_800, 25_600, 60_000,
];
// Additional overflow buckets for visibility of extreme outliers
const OVERFLOW_BUCKET_BOUNDS: &[u64] = &[120_000, 300_000, 600_000]; // 2min, 5min, 10min

/// Cache key for `trace_path` operations
///
/// Includes full `GraphIdentity` fields (`workspace_root`, `snapshot_sha256`, `built_at`,
/// `schema_version`, `snapshot_format_version`) to ensure cache isolation per workspace
/// and correct invalidation when graphs are rebuilt.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TracePathCacheKey {
    /// Canonicalized workspace root path (from `GraphIdentity`)
    pub workspace_root: std::path::PathBuf,
    /// SHA-256 hash of the snapshot content (from `GraphIdentity`)
    pub snapshot_sha256: String,
    /// Timestamp when the graph was built, in seconds since epoch (from `GraphIdentity`)
    /// Stored as i64 to avoid `DateTime` hashing complexity
    pub built_at_secs: i64,
    /// Schema version of the graph structure (from `GraphIdentity`)
    pub schema_version: u32,
    /// Binary format version of the snapshot (from `GraphIdentity`)
    pub snapshot_format_version: u32,
    /// Query parameters
    pub from_symbol: String,
    pub to_symbol: String,
    pub max_hops: usize,
    pub max_paths: usize,
    pub cross_language: bool,
    pub min_confidence_millis: u32, // Store as integer millis (0-1000) to avoid f64 hashing issues
}

impl TracePathCacheKey {
    /// Create a new trace path cache key with query parameters.
    ///
    /// `GraphIdentity` fields must be set separately via `with_graph_identity()`.
    pub fn new(
        from_symbol: String,
        to_symbol: String,
        max_hops: usize,
        max_paths: usize,
        cross_language: bool,
        min_confidence: f64,
    ) -> Self {
        let scaled_confidence = (min_confidence.clamp(0.0, 1.0) * 1000.0)
            .round()
            .clamp(0.0, f64::from(u32::MAX));
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        // Clamped to 0..=u32::MAX.
        let min_confidence_millis = scaled_confidence as u32;

        Self {
            // Placeholder GraphIdentity fields - must be set via with_graph_identity()
            workspace_root: std::path::PathBuf::new(),
            snapshot_sha256: String::new(),
            built_at_secs: 0,
            schema_version: 0,
            snapshot_format_version: 0,
            // Query parameters
            from_symbol,
            to_symbol,
            max_hops,
            max_paths,
            cross_language,
            min_confidence_millis,
        }
    }

    /// Set `GraphIdentity` fields for workspace isolation and cache invalidation.
    ///
    /// This method populates the `workspace_root`, `snapshot_sha256`, `built_at`,
    /// `schema_version`, and `snapshot_format_version` fields from a `GraphIdentity`.
    #[must_use]
    pub fn with_graph_identity(mut self, identity: &crate::engine::GraphIdentity) -> Self {
        self.workspace_root.clone_from(&identity.workspace_root);
        self.snapshot_sha256.clone_from(&identity.snapshot_sha256);
        self.built_at_secs = identity.built_at.timestamp();
        self.schema_version = identity.schema_version;
        self.snapshot_format_version = identity.snapshot_format_version;
        self
    }
}

/// Cache key for subgraph operations
///
/// Includes full `GraphIdentity` fields (`workspace_root`, `snapshot_sha256`, `built_at`,
/// `schema_version`, `snapshot_format_version`) to ensure cache isolation per workspace
/// and correct invalidation when graphs are rebuilt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::similar_names)] // Public API mirrors tool arguments (include_callers/include_callees).
#[allow(clippy::struct_excessive_bools)] // Struct mirrors tool arguments for traceability.
pub struct SubgraphCacheKey {
    /// Canonicalized workspace root path (from `GraphIdentity`)
    pub workspace_root: std::path::PathBuf,
    /// SHA-256 hash of the snapshot content (from `GraphIdentity`)
    pub snapshot_sha256: String,
    /// Timestamp when the graph was built, in seconds since epoch (from `GraphIdentity`)
    /// Stored as i64 to avoid `DateTime` hashing complexity
    pub built_at_secs: i64,
    /// Schema version of the graph structure (from `GraphIdentity`)
    pub schema_version: u32,
    /// Binary format version of the snapshot (from `GraphIdentity`)
    pub snapshot_format_version: u32,
    /// Query parameters
    pub symbols: Vec<String>, // Pre-sorted for deterministic hashing
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_callers: bool,
    pub include_callees: bool,
    pub include_imports: bool,
    pub cross_language: bool,
}

impl SubgraphCacheKey {
    /// Create a new subgraph cache key with query parameters.
    ///
    /// `GraphIdentity` fields must be set separately via `with_graph_identity()`.
    #[allow(clippy::similar_names)] // Keep parity with tool args (include_callers/include_callees).
    #[allow(clippy::fn_params_excessive_bools)] // Keeps parity with tool args for clarity.
    pub fn new(
        mut symbols: Vec<String>,
        max_depth: usize,
        max_nodes: usize,
        include_callers: bool,
        include_callees: bool,
        include_imports: bool,
        cross_language: bool,
    ) -> Self {
        // Sort symbols for deterministic cache key
        symbols.sort();
        Self {
            // Placeholder GraphIdentity fields - must be set via with_graph_identity()
            workspace_root: std::path::PathBuf::new(),
            snapshot_sha256: String::new(),
            built_at_secs: 0,
            schema_version: 0,
            snapshot_format_version: 0,
            // Query parameters
            symbols,
            max_depth,
            max_nodes,
            include_callers,
            include_callees,
            include_imports,
            cross_language,
        }
    }

    /// Set `GraphIdentity` fields for workspace isolation and cache invalidation.
    ///
    /// This method populates the `workspace_root`, `snapshot_sha256`, `built_at`,
    /// `schema_version`, and `snapshot_format_version` fields from a `GraphIdentity`.
    #[must_use]
    pub fn with_graph_identity(mut self, identity: &crate::engine::GraphIdentity) -> Self {
        self.workspace_root.clone_from(&identity.workspace_root);
        self.snapshot_sha256.clone_from(&identity.snapshot_sha256);
        self.built_at_secs = identity.built_at.timestamp();
        self.schema_version = identity.schema_version;
        self.snapshot_format_version = identity.snapshot_format_version;
        self
    }
}

impl PartialEq for SubgraphCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.workspace_root == other.workspace_root
            && self.snapshot_sha256 == other.snapshot_sha256
            && self.built_at_secs == other.built_at_secs
            && self.schema_version == other.schema_version
            && self.snapshot_format_version == other.snapshot_format_version
            && self.symbols == other.symbols
            && self.max_depth == other.max_depth
            && self.max_nodes == other.max_nodes
            && self.include_callers == other.include_callers
            && self.include_callees == other.include_callees
            && self.include_imports == other.include_imports
            && self.cross_language == other.cross_language
    }
}

impl Eq for SubgraphCacheKey {}

impl Hash for SubgraphCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.workspace_root.hash(state);
        self.snapshot_sha256.hash(state);
        self.built_at_secs.hash(state);
        self.schema_version.hash(state);
        self.snapshot_format_version.hash(state);
        self.symbols.hash(state);
        self.max_depth.hash(state);
        self.max_nodes.hash(state);
        self.include_callers.hash(state);
        self.include_callees.hash(state);
        self.include_imports.hash(state);
        self.cross_language.hash(state);
    }
}

/// Cached entry with TTL
struct CacheEntry<T> {
    data: T,
    created_at: Instant,
    ttl: Duration,
}

impl<T> CacheEntry<T> {
    fn new(data: T, ttl: Duration) -> Self {
        Self {
            data,
            created_at: Instant::now(),
            ttl,
        }
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.ttl
    }
}

/// Outcome of a cache lookup, including telemetry information
#[derive(Debug, Clone)]
pub struct CacheOutcome<T> {
    pub data: T,
    pub state: CacheState,
    pub latency_ms: u64,
}

/// Cache state for telemetry (warm = hit, cold = miss/recompute)
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CacheState {
    Cold,
    Warm,
}

/// Single cache event captured for telemetry
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEvent {
    pub state: CacheState,
    pub latency_ms: u64,
    pub timestamp_ms: u64,
}

/// Cache statistics (counters only)
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub expired: u64,
}

impl CacheStats {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            let hits = f64::from(u32::try_from(self.hits).unwrap_or(u32::MAX));
            let total = f64::from(u32::try_from(total).unwrap_or(u32::MAX));
            hits / total
        }
    }
}

/// Latency histogram snapshot used in graph metadata
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStatsSnapshot {
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub histogram: Vec<LatencyBucketSnapshot>,
}

impl LatencyStatsSnapshot {
    fn from_histogram(hist: &Histogram<u64>) -> Self {
        let count = hist.len();
        if count == 0 {
            return Self {
                count: 0,
                min_ms: None,
                max_ms: None,
                average_ms: None,
                p50_ms: None,
                p90_ms: None,
                p99_ms: None,
                histogram: Vec::new(),
            };
        }

        let min_ms = hist.min();
        let max_ms = hist.max();
        let average_ms = hist.mean();

        let mut snapshot = Self {
            count,
            min_ms: Some(min_ms),
            max_ms: Some(max_ms),
            average_ms: Some(average_ms),
            p50_ms: Some(hist.value_at_quantile(0.5)),
            p90_ms: Some(hist.value_at_quantile(0.9)),
            p99_ms: Some(hist.value_at_quantile(0.99)),
            histogram: Vec::new(),
        };

        snapshot.histogram = histogram_buckets(hist);
        snapshot
    }
}

/// Histogram bucket snapshot (inclusive upper bound)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyBucketSnapshot {
    pub upper_ms: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSnapshot {
    pub stats: CacheStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_latency: Option<LatencyStatsSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_latency: Option<LatencyStatsSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<CacheEvent>,
}

/// Maximum samples in histogram before rotation to prevent unbounded memory growth
const MAX_HISTOGRAM_SAMPLES: u64 = 10_000;

#[derive(Debug)]
struct CacheTelemetry {
    stats: CacheStats,
    warm_hist: Histogram<u64>,
    cold_hist: Histogram<u64>,
    last_event: Option<CacheEvent>,
}

impl CacheTelemetry {
    fn new() -> Self {
        Self {
            stats: CacheStats::default(),
            warm_hist: Self::create_histogram("warm"),
            cold_hist: Self::create_histogram("cold"),
            last_event: None,
        }
    }

    /// Create histogram with fallback on error (no panic)
    fn create_histogram(name: &str) -> Histogram<u64> {
        Histogram::new_with_bounds(1, HISTOGRAM_MAX_MS, HISTOGRAM_SIGFIGS).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to initialize {} histogram with bounds: {}. Using unbounded fallback.",
                name,
                e
            );
            // Fallback to unbounded histogram if bounded creation fails
            Histogram::new(HISTOGRAM_SIGFIGS).unwrap_or_else(|e2| {
                tracing::error!(
                    "Critical: Failed to create fallback histogram: {}. Using default.",
                    e2
                );
                // Last resort: default histogram (should never fail)
                Histogram::new(2).expect("default histogram creation must succeed")
            })
        })
    }

    fn reset(&mut self) {
        self.stats = CacheStats::default();
        self.warm_hist = Self::create_histogram("warm");
        self.cold_hist = Self::create_histogram("cold");
        self.last_event = None;
    }

    fn record_hit(&mut self, latency_ms: u64) {
        self.stats.hits += 1;
        self.record_event(CacheState::Warm, latency_ms);
    }

    fn record_miss(&mut self, latency_ms: u64) {
        self.stats.misses += 1;
        self.record_event(CacheState::Cold, latency_ms);
    }

    fn record_expired(&mut self) {
        self.stats.expired += 1;
    }

    fn record_eviction(&mut self) {
        self.stats.evictions += 1;
    }

    fn record_event(&mut self, state: CacheState, latency_ms: u64) {
        // Ensure minimum of 1ms for histogram compatibility
        let latency = latency_ms.max(1);

        // Saturate to histogram max for recording, but warn and store actual value
        let histogram_value = latency.min(HISTOGRAM_MAX_MS);

        if latency_ms > HISTOGRAM_MAX_MS {
            tracing::warn!(
                "Cache {:?} latency {}ms exceeds histogram maximum {}ms - saturating to max for histogram",
                state,
                latency_ms,
                HISTOGRAM_MAX_MS
            );
        }

        match state {
            CacheState::Warm => {
                // Rotate histogram if too many samples accumulated
                if self.warm_hist.len() >= MAX_HISTOGRAM_SAMPLES {
                    tracing::debug!(
                        "Warm histogram reached {} samples, rotating to prevent memory growth",
                        MAX_HISTOGRAM_SAMPLES
                    );
                    self.warm_hist.clear();
                }
                // Record saturated value so overflow buckets capture slow operations
                if let Err(e) = self.warm_hist.record(histogram_value) {
                    tracing::warn!(
                        "Failed to record warm cache latency {}ms: {}",
                        histogram_value,
                        e
                    );
                }
            }
            CacheState::Cold => {
                // Rotate histogram if too many samples accumulated
                if self.cold_hist.len() >= MAX_HISTOGRAM_SAMPLES {
                    tracing::debug!(
                        "Cold histogram reached {} samples, rotating to prevent memory growth",
                        MAX_HISTOGRAM_SAMPLES
                    );
                    self.cold_hist.clear();
                }
                // Record saturated value so overflow buckets capture slow operations
                if let Err(e) = self.cold_hist.record(histogram_value) {
                    tracing::warn!(
                        "Failed to record cold cache latency {}ms: {}",
                        histogram_value,
                        e
                    );
                }
            }
        }

        // Store actual (unclamped) latency in last_event for full visibility
        self.last_event = Some(CacheEvent {
            state,
            latency_ms: latency, // Store actual latency, not saturated
            timestamp_ms: epoch_ms_now(),
        });
    }

    fn snapshot(&self) -> CacheSnapshot {
        CacheSnapshot {
            stats: self.stats.clone(),
            warm_latency: if self.warm_hist.is_empty() {
                None
            } else {
                Some(LatencyStatsSnapshot::from_histogram(&self.warm_hist))
            },
            cold_latency: if self.cold_hist.is_empty() {
                None
            } else {
                Some(LatencyStatsSnapshot::from_histogram(&self.cold_hist))
            },
            last_event: self.last_event,
        }
    }
}

/// Global `trace_path` cache using LRU eviction
static TRACE_PATH_CACHE: OnceLock<
    Mutex<lru::LruCache<TracePathCacheKey, CacheEntry<TracePathData>>>,
> = OnceLock::new();

/// Global subgraph cache using LRU eviction
static SUBGRAPH_CACHE: OnceLock<
    Mutex<lru::LruCache<SubgraphCacheKey, CacheEntry<DependencyGraphData>>>,
> = OnceLock::new();

/// Global query cache TTL
static QUERY_CACHE_TTL: OnceLock<Duration> = OnceLock::new();

/// Global cache telemetry snapshots
static TRACE_PATH_TELEMETRY: OnceLock<Mutex<CacheTelemetry>> = OnceLock::new();
static SUBGRAPH_TELEMETRY: OnceLock<Mutex<CacheTelemetry>> = OnceLock::new();

/// Initialize the trace path cache with specified capacity and TTL.
///
/// This function must be called during server initialization before any cache access.
/// Subsequent calls are no-ops (idempotent).
pub fn init_trace_path_cache(capacity: std::num::NonZeroUsize, ttl: Duration) {
    TRACE_PATH_CACHE.get_or_init(|| {
        tracing::info!(
            capacity = capacity.get(),
            ttl_secs = ttl.as_secs(),
            "Initializing trace path cache"
        );
        Mutex::new(lru::LruCache::new(capacity))
    });
    QUERY_CACHE_TTL.get_or_init(|| ttl);
    TRACE_PATH_TELEMETRY.get_or_init(|| Mutex::new(CacheTelemetry::new()));
}

/// Initialize the subgraph cache with specified capacity and TTL.
///
/// This function must be called during server initialization before any cache access.
/// Subsequent calls are no-ops (idempotent).
pub fn init_subgraph_cache(capacity: std::num::NonZeroUsize, ttl: Duration) {
    SUBGRAPH_CACHE.get_or_init(|| {
        tracing::info!(
            capacity = capacity.get(),
            ttl_secs = ttl.as_secs(),
            "Initializing subgraph cache"
        );
        Mutex::new(lru::LruCache::new(capacity))
    });
    QUERY_CACHE_TTL.get_or_init(|| ttl);
    SUBGRAPH_TELEMETRY.get_or_init(|| Mutex::new(CacheTelemetry::new()));
}

/// Get the configured query cache TTL.
///
/// Returns the default TTL if caches haven't been initialized yet.
fn get_cache_ttl() -> Duration {
    QUERY_CACHE_TTL.get().copied().unwrap_or(CACHE_TTL)
}

/// Get or compute `trace_path` result with caching
pub fn get_or_compute_trace_path<F>(
    key: TracePathCacheKey,
    builder: F,
) -> CacheOutcome<TracePathData>
where
    F: FnOnce() -> TracePathData,
{
    let start = Instant::now();

    tracing::debug!(
        "Cache lookup: trace_path from={} to={}",
        key.from_symbol,
        key.to_symbol
    );

    let cache = TRACE_PATH_CACHE
        .get()
        .expect("Trace path cache not initialized - call init_trace_path_cache() first");

    // Try cache first
    let (cache_result, was_expired) = {
        let mut lock = cache.lock();
        if let Some(entry) = lock.get(&key) {
            if entry.is_expired() {
                // Expired - remove from cache
                lock.pop(&key);
                (None, true)
            } else {
                // Fresh hit
                let data = entry.data.clone();
                (Some(data), false)
            }
        } else {
            (None, false)
        }
    };

    // Handle cache hit
    if let Some(data) = cache_result {
        let latency_ms = elapsed_ms(start.elapsed());

        let telemetry = TRACE_PATH_TELEMETRY
            .get()
            .expect("Trace path telemetry not initialized");
        telemetry.lock().record_hit(latency_ms);

        tracing::debug!("Cache HIT: trace_path latency={}ms", latency_ms);

        return CacheOutcome {
            data,
            state: CacheState::Warm,
            latency_ms,
        };
    }

    // Record expired entry if applicable
    if was_expired {
        let telemetry = TRACE_PATH_TELEMETRY
            .get()
            .expect("Trace path telemetry not initialized");
        telemetry.lock().record_expired();
    }

    // Cache miss or expired - compute result
    let result = builder();
    let ttl = get_cache_ttl();

    // Store in cache (LRU handles eviction automatically)
    {
        let mut lock = cache.lock();
        let evicted = lock.push(key, CacheEntry::new(result.clone(), ttl));

        // Record eviction if an entry was pushed out
        if evicted.is_some() {
            let telemetry = TRACE_PATH_TELEMETRY
                .get()
                .expect("Trace path telemetry not initialized");
            telemetry.lock().record_eviction();
        }
    }

    let latency_ms = elapsed_ms(start.elapsed());

    let telemetry = TRACE_PATH_TELEMETRY
        .get()
        .expect("Trace path telemetry not initialized");
    telemetry.lock().record_miss(latency_ms);

    tracing::debug!("Cache MISS: trace_path latency={}ms", latency_ms);

    CacheOutcome {
        data: result,
        state: CacheState::Cold,
        latency_ms,
    }
}

/// Get or compute subgraph result with caching
pub fn get_or_compute_subgraph<F>(
    key: SubgraphCacheKey,
    builder: F,
) -> CacheOutcome<DependencyGraphData>
where
    F: FnOnce() -> DependencyGraphData,
{
    let start = Instant::now();

    tracing::debug!(
        "Cache lookup: subgraph symbols={:?}",
        key.symbols.iter().take(3).collect::<Vec<_>>()
    );

    let cache = SUBGRAPH_CACHE
        .get()
        .expect("Subgraph cache not initialized - call init_subgraph_cache() first");

    // Try cache first
    let (cache_result, was_expired) = {
        let mut lock = cache.lock();
        if let Some(entry) = lock.get(&key) {
            if entry.is_expired() {
                // Expired - remove from cache
                lock.pop(&key);
                (None, true)
            } else {
                // Fresh hit
                let data = entry.data.clone();
                (Some(data), false)
            }
        } else {
            (None, false)
        }
    };

    // Handle cache hit
    if let Some(data) = cache_result {
        let latency_ms = elapsed_ms(start.elapsed());

        let telemetry = SUBGRAPH_TELEMETRY
            .get()
            .expect("Subgraph telemetry not initialized");
        telemetry.lock().record_hit(latency_ms);

        tracing::debug!("Cache HIT: subgraph latency={}ms", latency_ms);

        return CacheOutcome {
            data,
            state: CacheState::Warm,
            latency_ms,
        };
    }

    // Record expired entry if applicable
    if was_expired {
        let telemetry = SUBGRAPH_TELEMETRY
            .get()
            .expect("Subgraph telemetry not initialized");
        telemetry.lock().record_expired();
    }

    // Cache miss or expired - compute result
    let result = builder();
    let ttl = get_cache_ttl();

    // Store in cache (LRU handles eviction automatically)
    {
        let mut lock = cache.lock();
        let evicted = lock.push(key, CacheEntry::new(result.clone(), ttl));

        // Record eviction if an entry was pushed out
        if evicted.is_some() {
            let telemetry = SUBGRAPH_TELEMETRY
                .get()
                .expect("Subgraph telemetry not initialized");
            telemetry.lock().record_eviction();
        }
    }

    let latency_ms = elapsed_ms(start.elapsed());

    let telemetry = SUBGRAPH_TELEMETRY
        .get()
        .expect("Subgraph telemetry not initialized");
    telemetry.lock().record_miss(latency_ms);

    tracing::debug!("Cache MISS: subgraph latency={}ms", latency_ms);

    CacheOutcome {
        data: result,
        state: CacheState::Cold,
        latency_ms,
    }
}

/// Get `trace_path` cache statistics
#[cfg_attr(not(test), allow(dead_code))]
pub fn trace_path_cache_stats() -> CacheStats {
    TRACE_PATH_TELEMETRY
        .get()
        .expect("Trace path telemetry not initialized")
        .lock()
        .stats
        .clone()
}

/// Get subgraph cache statistics
#[cfg_attr(not(test), allow(dead_code))]
pub fn subgraph_cache_stats() -> CacheStats {
    SUBGRAPH_TELEMETRY
        .get()
        .expect("Subgraph telemetry not initialized")
        .lock()
        .stats
        .clone()
}

/// Snapshot telemetry for `trace_path` cache
pub fn trace_path_cache_snapshot() -> CacheSnapshot {
    TRACE_PATH_TELEMETRY
        .get()
        .expect("Trace path telemetry not initialized")
        .lock()
        .snapshot()
}

/// Snapshot telemetry for subgraph cache
pub fn subgraph_cache_snapshot() -> CacheSnapshot {
    SUBGRAPH_TELEMETRY
        .get()
        .expect("Subgraph telemetry not initialized")
        .lock()
        .snapshot()
}

/// Clear all caches (useful for testing and manual cache invalidation)
#[cfg_attr(not(test), allow(dead_code))]
pub fn clear_all_caches() {
    if let Some(cache) = TRACE_PATH_CACHE.get() {
        cache.lock().clear();
    }
    if let Some(cache) = SUBGRAPH_CACHE.get() {
        cache.lock().clear();
    }
    if let Some(telemetry) = TRACE_PATH_TELEMETRY.get() {
        telemetry.lock().reset();
    }
    if let Some(telemetry) = SUBGRAPH_TELEMETRY.get() {
        telemetry.lock().reset();
    }
}

/// Get current cache sizes
#[cfg_attr(not(test), allow(dead_code))]
pub fn cache_sizes() -> (usize, usize) {
    let trace_size = TRACE_PATH_CACHE.get().map_or(0, |c| c.lock().len());
    let subgraph_size = SUBGRAPH_CACHE.get().map_or(0, |c| c.lock().len());
    (trace_size, subgraph_size)
}

fn histogram_buckets(histogram: &Histogram<u64>) -> Vec<LatencyBucketSnapshot> {
    let mut buckets = Vec::new();
    let mut lower_bound = 1;

    for &upper in HISTOGRAM_BUCKET_BOUNDS {
        let count = histogram.count_between(lower_bound, upper);
        if count > 0 {
            buckets.push(LatencyBucketSnapshot {
                upper_ms: upper,
                count,
            });
        }
        lower_bound = upper + 1;
    }

    // Handle overflow beyond max bucket with more granular buckets
    let max_bound = *HISTOGRAM_BUCKET_BOUNDS.last().unwrap_or(&HISTOGRAM_MAX_MS);

    // Add overflow buckets for better visibility of outliers
    for &overflow_bound in OVERFLOW_BUCKET_BOUNDS {
        if overflow_bound > max_bound {
            let count = histogram.count_between(lower_bound, overflow_bound);
            if count > 0 {
                buckets.push(LatencyBucketSnapshot {
                    upper_ms: overflow_bound,
                    count,
                });
                if count > 10 {
                    tracing::warn!(
                        "High number of cache operations ({}) exceeded {}ms threshold",
                        count,
                        overflow_bound
                    );
                }
            }
            lower_bound = overflow_bound + 1;
        }
    }

    // Final overflow bucket for extreme outliers beyond 10 minutes
    let final_overflow = histogram.count_between(lower_bound, u64::MAX);
    if final_overflow > 0 {
        tracing::warn!(
            "Extreme outliers detected: {} cache operations exceeded {}ms (10min)",
            final_overflow,
            OVERFLOW_BUCKET_BOUNDS.last().unwrap_or(&HISTOGRAM_MAX_MS)
        );
        buckets.push(LatencyBucketSnapshot {
            upper_ms: u64::MAX,
            count: final_overflow,
        });
    }

    buckets
}

fn elapsed_ms(duration: Duration) -> u64 {
    let millis = duration.as_secs_f64() * 1000.0;
    if millis < 1.0 {
        1
    } else {
        // Duration beyond u64::MAX ms (~584 million years) is impossible; clamped to max
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation
        )] // Clamped to valid range.
        {
            millis.round().clamp(1.0, u64::MAX as f64) as u64
        }
    }
}

fn epoch_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or_else(
            |e| {
                tracing::error!("Failed to get system time: {e}, using 0");
                0
            },
            |d| {
                // Safely convert u128 to u64 with saturation to prevent overflow
                // This handles timestamps beyond year 2262 gracefully
                // Timestamps beyond u64::MAX ms (~584 million years) are impossible; clamp to max
                d.as_millis().try_into().unwrap_or_else(|_| {
                    tracing::error!(
                        "System time milliseconds exceed u64::MAX, using saturated value"
                    );
                    u64::MAX
                })
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    #[serial]
    fn test_trace_path_cache_key_equality() {
        let key1 = TracePathCacheKey::new("foo".to_string(), "bar".to_string(), 5, 10, true, 0.5);
        let key2 = TracePathCacheKey::new("foo".to_string(), "bar".to_string(), 5, 10, true, 0.5);
        assert_eq!(key1, key2);
    }

    #[test]
    #[serial]
    fn test_subgraph_cache_key_deterministic() {
        // Keys with different order should be equal after sorting
        let key1 = SubgraphCacheKey::new(
            vec!["z".to_string(), "a".to_string(), "m".to_string()],
            2,
            50,
            true,
            true,
            false,
            true,
        );
        let key2 = SubgraphCacheKey::new(
            vec!["a".to_string(), "m".to_string(), "z".to_string()],
            2,
            50,
            true,
            true,
            false,
            true,
        );
        assert_eq!(key1, key2);
    }

    #[test]
    #[serial]
    fn test_trace_path_cache_hit() {
        // Initialize cache before use
        init_trace_path_cache(
            std::num::NonZeroUsize::new(TRACE_PATH_CACHE_CAPACITY).unwrap(),
            Duration::from_secs(CACHE_TTL_SECS),
        );
        clear_all_caches();

        #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
        static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

        let key = TracePathCacheKey::new(
            "test_from".to_string(),
            "test_to".to_string(),
            5,
            10,
            true,
            0.5,
        );

        let builder = || {
            CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            TracePathData {
                paths: vec![],
                from_symbol: "test_from".to_string(),
                to_symbol: "test_to".to_string(),
            }
        };

        // First call - cache miss
        let stats_before = trace_path_cache_stats();
        let outcome1 = get_or_compute_trace_path(key.clone(), builder);
        assert_eq!(outcome1.state, CacheState::Cold);
        let result1 = outcome1.data;
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(result1.from_symbol, "test_from");

        // Second call - cache hit
        let outcome2 = get_or_compute_trace_path(key.clone(), builder);
        assert_eq!(outcome2.state, CacheState::Warm);
        let result2 = outcome2.data;
        assert_eq!(
            CALL_COUNT.load(Ordering::SeqCst),
            1,
            "Builder should not be called on cache hit"
        );
        assert_eq!(result2.from_symbol, "test_from");

        let stats_after = trace_path_cache_stats();
        assert_eq!(stats_after.hits.saturating_sub(stats_before.hits), 1);
        assert_eq!(stats_after.misses.saturating_sub(stats_before.misses), 1);
    }

    #[test]
    #[serial]
    fn test_cache_eviction() {
        // Initialize cache before use
        init_trace_path_cache(
            std::num::NonZeroUsize::new(TRACE_PATH_CACHE_CAPACITY).unwrap(),
            Duration::from_secs(CACHE_TTL_SECS),
        );
        clear_all_caches();

        // Fill cache beyond capacity
        for i in 0..TRACE_PATH_CACHE_CAPACITY + 10 {
            let key =
                TracePathCacheKey::new(format!("from_{i}"), format!("to_{i}"), 5, 10, true, 0.5);
            get_or_compute_trace_path(key, || TracePathData {
                paths: vec![],
                from_symbol: format!("from_{i}"),
                to_symbol: format!("to_{i}"),
            });
        }

        let (trace_size, _) = cache_sizes();
        assert!(
            trace_size <= TRACE_PATH_CACHE_CAPACITY,
            "Cache should not exceed max capacity"
        );

        let stats = trace_path_cache_stats();
        assert!(stats.evictions > 0, "Evictions should have occurred");

        let snapshot = trace_path_cache_snapshot();
        assert!(snapshot.cold_latency.is_some());
    }

    #[test]
    #[serial]
    fn test_subgraph_cache_snapshot_reports_metrics() {
        // Initialize cache before use
        init_subgraph_cache(
            std::num::NonZeroUsize::new(SUBGRAPH_CACHE_CAPACITY).unwrap(),
            Duration::from_secs(CACHE_TTL_SECS),
        );
        clear_all_caches();

        let key = SubgraphCacheKey::new(vec!["root".into()], 2, 10, true, true, false, true);

        let builder = || DependencyGraphData {
            nodes: vec![],
            edges: vec![],
            rendered: None,
        };

        let _ = get_or_compute_subgraph(key.clone(), builder);
        let _ = get_or_compute_subgraph(key, builder);

        let snapshot = subgraph_cache_snapshot();
        assert!(snapshot.stats.hits >= 1);
        assert!(snapshot.stats.misses >= 1);
        assert!(snapshot.last_event.is_some());

        let aggregated = subgraph_cache_stats();
        assert!(aggregated.hits >= 1);
    }
}
