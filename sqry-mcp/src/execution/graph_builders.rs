//! Graph construction and metadata utilities for MCP tool execution.
//!
//! This module provides functions for computing graph metadata from the
//! unified `GraphSnapshot` and handling cache metrics.
//!
//! #
//!
//! ## ✅ Fully Migrated to Unified Graph
//!
//! All MCP tools now use the unified graph architecture exclusively.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use super::graph_cache;
use super::types::{
    CacheMetricsSummary, CacheRequestContext, CacheRequestEvent, GraphCacheMetadata,
    GraphCacheStrategyMetadata, GraphMetadata,
};
use super::utils::current_epoch_ms;
use crate::mcp_config::McpConfig;

/// Cached snapshot-derived metrics to avoid repeated expensive edge scans.
///
/// Keyed by `(node_count, csr_edge_count)` as a proxy for snapshot identity.
/// When the graph is rebuilt, these counts change and the cache is invalidated.
struct SnapshotMetricsCache {
    node_count: u64,
    csr_edge_count: u64,
    languages: Vec<String>,
    cross_language_edges: u64,
}

static SNAPSHOT_METRICS_CACHE: Mutex<Option<SnapshotMetricsCache>> = Mutex::new(None);

/// Default maximum edges to scan for cross-language computation (performance guard)
///
/// **Deprecated:** Use `McpConfig::max_cross_lang_edges` instead.
/// This constant remains for backward compatibility only.
const MAX_EDGES_FOR_CROSS_LANG_SCAN: usize = 50_000;

/// Get effective max cross-language edges limit from config or default
fn effective_max_cross_lang_edges() -> usize {
    McpConfig::load_or_default()
        .ok()
        .and_then(|c| c.effective_max_cross_lang_edges().ok())
        .unwrap_or(MAX_EDGES_FOR_CROSS_LANG_SCAN)
}

/// Build metadata about the graph including cache statistics.
///
/// Uses the unified `GraphSnapshot` for computing graph metadata.
///
/// # Arguments
///
/// * `workspace_root` - Optional workspace root path to load manifest for confidence data
/// * `snapshot` - Optional unified graph snapshot (source for graph metadata)
/// * `request` - Optional cache request context for tracking cache hits/misses
///
/// # Metadata Sources
///
/// - **Unified graph** (snapshot): node/edge counts, languages from `FileRegistry`
/// - **Manifest** (via `workspace_root`): confidence metadata per language
pub(crate) fn build_graph_metadata(
    workspace_root: Option<&Path>,
    snapshot: Option<&sqry_core::graph::unified::concurrent::GraphSnapshot>,
    request: Option<CacheRequestContext>,
) -> GraphMetadata {
    let (total_nodes, total_edges, languages, cross_language_edges) = if let Some(snap) = snapshot {
        let nodes = snap.nodes().len() as u64;
        let edge_stats = snap.edges().stats();
        let csr_edges = edge_stats.forward.csr_edge_count as u64;
        let edges = csr_edges + edge_stats.forward.delta_edge_count as u64;

        // Check cache: reuse languages + cross_language_edges if snapshot hasn't changed
        let cached = SNAPSHOT_METRICS_CACHE.lock().ok().and_then(|guard| {
            guard.as_ref().and_then(|c| {
                if c.node_count == nodes && c.csr_edge_count == csr_edges {
                    Some((c.languages.clone(), c.cross_language_edges))
                } else {
                    None
                }
            })
        });

        let (langs, cross_lang) = if let Some((cached_langs, cached_cross)) = cached {
            (cached_langs, cached_cross)
        } else {
            let langs = collect_languages_from_snapshot(snap);
            let cross_lang = compute_cross_language_from_snapshot(snap) as u64;

            // Update cache
            if let Ok(mut guard) = SNAPSHOT_METRICS_CACHE.lock() {
                *guard = Some(SnapshotMetricsCache {
                    node_count: nodes,
                    csr_edge_count: csr_edges,
                    languages: langs.clone(),
                    cross_language_edges: cross_lang,
                });
            }
            (langs, cross_lang)
        };

        (nodes, edges, langs, cross_lang)
    } else {
        // No data source - return empty metadata
        (0, 0, vec![], 0)
    };

    let graph_version = "unified".to_string();
    let rebuild_epoch_ms = current_epoch_ms();

    let trace_snapshot = graph_cache::trace_path_cache_snapshot();
    let subgraph_snapshot = graph_cache::subgraph_cache_snapshot();

    // Load effective cache capacities from config or use defaults
    let trace_capacity = McpConfig::load_or_default()
        .ok()
        .and_then(|c| c.effective_trace_cache_size().ok())
        .unwrap_or(graph_cache::TRACE_PATH_CACHE_CAPACITY);
    let subgraph_capacity = McpConfig::load_or_default()
        .ok()
        .and_then(|c| c.effective_subgraph_cache_size().ok())
        .unwrap_or(graph_cache::SUBGRAPH_CACHE_CAPACITY);

    let cache_metadata = GraphCacheMetadata {
        strategy: GraphCacheStrategyMetadata {
            policy: "lru",
            ttl_seconds: graph_cache::CACHE_TTL_SECS,
            trace_path_capacity: trace_capacity,
            subgraph_capacity,
        },
        trace_path: compute_cache_summary(trace_snapshot),
        subgraph: compute_cache_summary(subgraph_snapshot),
        current_request: request.map(|ctx| CacheRequestEvent {
            tool: ctx.tool,
            state: ctx.state,
            latency_ms: ctx.latency_ms,
            timestamp_ms: current_epoch_ms(),
        }),
    };

    // Load confidence from manifest if workspace root is provided
    let confidence = workspace_root
        .map(|root| {
            let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
            storage
                .load_manifest()
                .ok()
                .map(|manifest| manifest.confidence)
                .unwrap_or_default()
        })
        .unwrap_or_default();

    GraphMetadata {
        total_nodes,
        total_edges,
        languages,
        cross_language_edges,
        graph_version,
        rebuild_epoch_ms,
        cache: Some(cache_metadata),
        confidence,
    }
}

/// Collect unique languages from a unified graph snapshot.
fn collect_languages_from_snapshot(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> Vec<String> {
    let files = snapshot.files();
    let mut languages = HashSet::new();

    for (file_id, _path) in files.iter() {
        if let Some(lang) = files.language_for_file(file_id) {
            languages.insert(lang.to_string());
        }
    }

    let mut langs: Vec<String> = languages.into_iter().collect();
    langs.sort();
    langs
}

/// Compute count of cross-language edges from a unified graph snapshot.
///
/// Uses a performance guard to limit scanning to `MAX_EDGES_FOR_CROSS_LANG_SCAN` edges,
/// preventing expensive full-graph scans on large codebases.
fn compute_cross_language_from_snapshot(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> usize {
    let files = snapshot.files();
    let mut count = 0;
    let mut scanned = 0;

    let max_edges = effective_max_cross_lang_edges();
    for (source_id, target_id, _kind) in snapshot.iter_edges() {
        // Performance guard: cap total edges scanned, not just cross-language count
        scanned += 1;
        if scanned > max_edges {
            break;
        }

        let Some(source_node) = snapshot.get_node(source_id) else {
            continue;
        };
        let Some(target_node) = snapshot.get_node(target_id) else {
            continue;
        };

        let source_lang = files.language_for_file(source_node.file);
        let target_lang = files.language_for_file(target_node.file);

        if let (Some(sl), Some(tl)) = (source_lang, target_lang)
            && sl != tl
        {
            count += 1;
        }
    }

    count
}

/// Compute cache metrics summary from a snapshot.
pub(crate) fn compute_cache_summary(snapshot: graph_cache::CacheSnapshot) -> CacheMetricsSummary {
    let graph_cache::CacheSnapshot {
        stats,
        warm_latency,
        cold_latency,
        last_event,
    } = snapshot;

    CacheMetricsSummary {
        hits: stats.hits,
        misses: stats.misses,
        evictions: stats.evictions,
        expired: stats.expired,
        hit_rate: stats.hit_rate(),
        warm_latency,
        cold_latency,
        last_event,
    }
}
