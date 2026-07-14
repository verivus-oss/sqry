//! `daemon/search` JSON-RPC handler — tier-2 of issue-238.
//!
//! Receives a [`SearchRequest`], acquires the workspace graph through the
//! shared [`crate::workspace::acquirer::DaemonGraphProvider`] boundary so
//! post-eviction bounded reload works correctly per CLAUDE.md "Shared graph
//! acquisition (read-only parity, 2026-05-08+)", then runs the same
//! `find_by_exact_name` / regex / fuzzy logic the in-process CLI search
//! uses (`sqry-cli/src/commands/search.rs::run_regular_search` and
//! `::run_fuzzy_search`). Parity with the in-process path is a hard contract
//! verified by the `DAEMON_SEARCH_TESTS` unit.
//!
//! Wire contract:
//! - Input: [`SearchRequest`] under JSON-RPC `params`.
//! - Output: [`ResponseEnvelope<SearchResult>`] under the JSON-RPC `result`
//!   field. The stale path uses [`ResponseMeta::stale_from`] to surface
//!   staleness — `_stale_warning` is NOT spliced into the result body
//!   because [`SearchResult`] carries `#[serde(deny_unknown_fields)]` and
//!   the client would reject an extra field.
//!
//! Error mapping piggybacks on the established daemon error taxonomy:
//! - `WorkspaceEvicted` → JSON-RPC `-32004` via [`DaemonError::jsonrpc_code`]
//! - `WorkspaceIncompatibleGraph` → `-32005`
//! - `ToolTimeout` → `-32000`
//! - Invalid `params` → `-32602`

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::search::fuzzy::{CandidateGenerator, FuzzyConfig};
use sqry_core::search::matcher::{FuzzyMatcher, MatchConfig};
use sqry_core::search::trigram::TrigramIndex;
use sqry_daemon_protocol::{
    RevisionQueryTarget, SearchItem, SearchMode, SearchRequest, SearchResult,
};

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::super::tool_core;
use super::{HandlerContext, MethodError, daemon_load_revision};

/// Default per-mode result cap, mirroring the CLI defaults in
/// `sqry-cli/src/commands/search.rs::run_search` (search=100, fuzzy=50).
const DEFAULT_LIMIT_REGEX_EXACT: usize = 100;
const DEFAULT_LIMIT_FUZZY: usize = 50;

/// Default minimum-score thresholds, mirroring the CLI fuzzy defaults
/// from `sqry-cli/src/args/mod.rs` (`fuzzy_threshold = 0.6`,
/// `fuzzy_max_candidates = 1000`, `min_similarity = 0.1`).
const FUZZY_MIN_SCORE: f64 = 0.6;
const FUZZY_MAX_CANDIDATES: usize = 1000;
const FUZZY_MIN_SIMILARITY: f64 = 0.1;

/// Handle one `daemon/search` request.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: SearchRequest = match params {
        Value::Null => {
            return Err(MethodError::InvalidParams(serde::de::Error::custom(
                "daemon/search requires params",
            )));
        }
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };

    // Pre-acquisition validation: every check that depends only on the
    // request (and not on the graph) runs here so a bad input maps to
    // -32602 InvalidParams rather than -32603 Internal. The latter
    // would happen if we surfaced the error from inside the closure,
    // since `acquire_and_execute` wraps closure errors as
    // `DaemonError::Internal` (-32603 per `DaemonError::jsonrpc_code`).
    let regex = validate_request(&req)?;

    if let Some(target) = req.revision.clone() {
        return handle_revision_search(ctx, req, target, regex).await;
    }

    // Capture path + tool timeout up front; the `req` is moved into the
    // closure below so this avoids a clone of every owned field on req.
    let path = req.search_path.clone();
    let tool_timeout = Duration::from_secs(ctx.config.tool_timeout_secs);

    let verdict = tool_core::acquire_and_execute(
        Arc::clone(&ctx.manager),
        Arc::clone(&ctx.workspace_builder),
        Arc::clone(&ctx.tool_executor),
        &ctx.cpu_executor,
        tool_timeout,
        &path,
        Some("daemon/search"),
        move |wctx, cancel| -> anyhow::Result<Value> {
            let result =
                run_search_on_graph_cancellable(&wctx.graph, &req, regex.as_ref(), cancel)?;
            serde_json::to_value(&result).context("serialise SearchResult")
        },
    )
    .await
    .map_err(MethodError::Daemon)?;

    match verdict {
        tool_core::ExecuteVerdict::Fresh { inner, state } => {
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::fresh_from(state, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|e| MethodError::Internal(anyhow::anyhow!("envelope serialise: {e}")))
        }
        tool_core::ExecuteVerdict::Stale {
            inner,
            stale_warning: _,
            last_good_at,
            last_error,
        } => {
            // SearchResult is `deny_unknown_fields`, so the per-tool
            // `_stale_warning` splice that the 14-tool `tool_dispatch`
            // path uses cannot be applied here without breaking the
            // client. Staleness reaches the client via `meta.stale`
            // + `meta.last_good_at` + `meta.last_error`, which the CLI
            // shim reads to decide whether to emit a stale banner.
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::stale_from(last_good_at, last_error, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|e| MethodError::Internal(anyhow::anyhow!("envelope serialise: {e}")))
        }
    }
}

async fn handle_revision_search(
    ctx: &HandlerContext,
    req: SearchRequest,
    target: RevisionQueryTarget,
    regex: Option<Regex>,
) -> Result<Value, MethodError> {
    // #566: guard the direct-canonicalize revision branch (bypasses the
    // `resolve_path` funnel) so a relative path is not resolved against the
    // daemon's own CWD.
    crate::ipc::path_policy::ensure_absolute_workspace_path(std::path::Path::new(&req.search_path))
        .map_err(|reason| {
            MethodError::Daemon(crate::error::DaemonError::InvalidArgument { reason })
        })?;
    let root = std::path::PathBuf::from(&req.search_path)
        .canonicalize()
        .map_err(|err| {
            MethodError::Daemon(crate::error::DaemonError::InvalidArgument {
                reason: format!(
                    "daemon/search revision target path {} could not be canonicalized: {err}",
                    req.search_path
                ),
            })
        })?;
    let (revision_id, metadata) = daemon_load_revision::resolve_query_target(ctx, &root, &target)?;
    let guard = ctx.manager.acquire_resident_query(&revision_id)?;
    let graph = guard.graph().ok_or_else(|| {
        MethodError::Daemon(crate::error::DaemonError::RevisionSourceUnavailable {
            reason: format!("resident revision {} had no loaded graph", revision_id.0),
            path: Some(root.clone()),
        })
    })?;
    let tool_timeout = Duration::from_secs(ctx.config.tool_timeout_secs);

    // issue #503 Phase 2: submit the scan on the shared dedicated CPU pool.
    // `CpuExecutor::run` owns and flips the token on deadline (fire-and-forget)
    // and maps the result through the shared ladder, so a timed-out scan stops
    // at its next CANCEL_POLL_STRIDE poll without pinning a Tokio blocking
    // thread. The wire envelope is unchanged.
    let result = ctx
        .cpu_executor
        .run(
            tool_timeout,
            &root,
            move |cancel| -> anyhow::Result<SearchResult> {
                let mut result =
                    run_search_on_graph_cancellable(&graph, &req, regex.as_ref(), cancel)?;
                result.revision = Some(metadata);
                Ok(result)
            },
        )
        .await
        .map_err(MethodError::Daemon)?;
    drop(guard);

    let envelope = ResponseEnvelope {
        result: serde_json::to_value(result)
            .map_err(|err| MethodError::Internal(anyhow::Error::new(err)))?,
        meta: ResponseMeta::fresh_from(
            crate::workspace::WorkspaceState::Loaded,
            ctx.daemon_version,
        ),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

/// Validate a `SearchRequest` against its declared mode.
///
/// Every check here depends only on the request — never on the graph —
/// so a failure can map cleanly to JSON-RPC `-32602 InvalidParams`
/// before any graph acquisition work. Surfacing the same failure from
/// inside the `acquire_and_execute` closure would yield `-32603 Internal`
/// (closure errors are wrapped as `DaemonError::Internal`), which is
/// wrong for input-shape problems and surfaced as Codex review finding 2.
///
/// For `SearchMode::Regex`, returns the compiled `Regex` so the dispatch
/// closure can reuse it without recompiling.
fn validate_request(req: &SearchRequest) -> Result<Option<Regex>, MethodError> {
    if req.pattern.is_empty() {
        return Err(MethodError::InvalidParams(serde::de::Error::custom(
            "daemon/search: pattern must not be empty",
        )));
    }
    let regex = match req.mode {
        SearchMode::Regex => {
            // `RegexBuilder` rather than `Regex::new` so we share the same
            // construction path as the CLI (which configures case sensitivity
            // through this builder).
            let compiled = RegexBuilder::new(&req.pattern).build().map_err(|e| {
                MethodError::InvalidParams(serde::de::Error::custom(format!(
                    "daemon/search: invalid regex pattern: {e}"
                )))
            })?;
            Some(compiled)
        }
        SearchMode::Exact | SearchMode::Fuzzy => None,
    };
    Ok(regex)
}

/// One per-hit intermediate carrying the node id and, for fuzzy hits,
/// the match score from `FuzzyMatcher::match_many`. Exact and regex
/// hits leave `score = None`.
///
/// Threading the score through this intermediate is required so:
/// - the fuzzy path preserves the score-descending order
///   `FuzzyMatcher::match_many` produces (Codex review finding 1);
/// - `SearchItem::score` is populated for fuzzy hits as the protocol
///   crate's wire shape allows (`SearchItem.score: Option<f32>` at
///   `sqry-daemon-protocol/src/protocol.rs:733`).
#[derive(Debug, Clone, Copy)]
struct ScoredHit {
    node_id: NodeId,
    score: Option<f32>,
}

/// Run the actual search against a captured graph. Pure function: no
/// async, no IO — invoked inside `spawn_blocking` by `acquire_and_execute`.
///
/// Per-mode behaviour:
/// - Exact / regex: sort + dedup by `NodeId`; `score = None` everywhere.
/// - Fuzzy: order-preserving dedup by `NodeId` so the score-descending
///   ranking from `FuzzyMatcher::match_many` survives all the way through
///   `limit` truncation.
///
/// `total` reports the pre-truncation match count. When `truncated == false`
/// it equals `items.len()`; when `truncated == true` it is `> limit`
/// (matching the protocol crate's lower-bound-sentinel docstring at
/// `sqry-daemon-protocol/src/protocol.rs:758` while in practice carrying
/// the exact count, since the daemon knows it).
/// Poll the cancellation token once per this many strings / candidates in
/// the search scan loops. Keeps the atomic load off the hot path while
/// bounding how long a timed-out search runs before it observes the
/// deadline-flipped token (issue #503 Phase 1).
const CANCEL_POLL_STRIDE: usize = 1024;

/// Never-cancelled convenience wrapper over [`run_search_on_graph_cancellable`].
///
/// Test-only (`#[cfg(test)]`): production code (the central and revision
/// handlers) calls the cancellable variant directly, so the sole callers of
/// this simple form are the 11 in-crate unit tests, which it keeps untouched.
/// It constructs a token it never flips, so the inner scan can never yield
/// [`sqry_core::query::QueryError::Cancelled`], hence the `expect`.
#[cfg(test)]
fn run_search_on_graph(
    graph: &CodeGraph,
    req: &SearchRequest,
    precompiled_regex: Option<&Regex>,
) -> SearchResult {
    run_search_on_graph_cancellable(
        graph,
        req,
        precompiled_regex,
        &sqry_core::query::cancellation::CancellationToken::new(),
    )
    .expect("never-cancelled token cannot yield QueryError::Cancelled")
}

/// Cancellable search scan. Polls `cancel` in the regex / fuzzy candidate
/// loops (exact is an index lookup and needs no poll), so a deadline-flipped
/// token stops the scan promptly and frees the blocking-pool slot.
fn run_search_on_graph_cancellable(
    graph: &CodeGraph,
    req: &SearchRequest,
    precompiled_regex: Option<&Regex>,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<SearchResult, sqry_core::query::QueryError> {
    // Step 1: per-mode candidate generation. Each branch returns hits in its
    // mode-appropriate order (exact/regex are NodeId-keyed and sorted below,
    // fuzzy is score-descending).
    let hits: Vec<ScoredHit> = match req.mode {
        SearchMode::Exact => exact_hits(graph, &req.pattern),
        SearchMode::Regex => regex_hits(
            graph,
            precompiled_regex.expect("validate_request must precompile regex"),
            cancel,
        )?,
        SearchMode::Fuzzy => fuzzy_hits(graph, &req.pattern, cancel)?,
    };

    // Steps 2..5 run on the already-bounded hit vector and are cheap; one
    // poll here covers them.
    cancel.query_check()?;

    // Step 2: dedup. Fuzzy must preserve order (first occurrence has
    // the highest score) — sorting by NodeId would destroy ranking.
    // Exact/regex can use the cheaper sort+dedup-by-key because order
    // is not semantically meaningful in those modes.
    let hits = match req.mode {
        SearchMode::Fuzzy => dedup_preserve_order(hits),
        SearchMode::Exact | SearchMode::Regex => {
            let mut h = hits;
            h.sort_by_key(|hit| hit.node_id);
            h.dedup_by_key(|hit| hit.node_id);
            h
        }
    };

    // Step 2.5: macro-boundary filter parity. Mirrors the CLI's
    // `sqry-cli/src/commands/search.rs::filter_nodes_by_macro_boundary`
    // for the `include_generated == false` arm — drops candidates whose
    // graph metadata reports `macro_generated == Some(true)`. The CLI's
    // default invocation has `include_generated == false`, so without
    // this step the daemon route returns extra macro-generated hits the
    // in-process path would have dropped (Codex round-1 CLI shim review
    // High finding). The `cfg_filter` / `macro_boundaries` arms of the
    // CLI filter are not threaded on the wire today and remain SHIM-
    // gated; if the CLI engages those flags the shim falls through to
    // in-process so the daemon never observes them.
    let hits = if req.include_generated {
        hits
    } else {
        filter_macro_generated_hits(graph, hits)
    };

    // Step 3: convert to wire-form SearchItem (score threaded through).
    let mut items: Vec<SearchItem> = hits
        .into_iter()
        .filter_map(|hit| node_to_search_item(graph, hit.node_id, hit.score))
        .collect();

    // Step 4: kind + lang filters. `retain` preserves order so fuzzy
    // ranking is still honoured.
    apply_filters(&mut items, req.kind.as_deref(), req.lang.as_deref());

    // Step 5: limit + total + truncated sentinel.
    let limit = req.limit.map_or(
        match req.mode {
            SearchMode::Fuzzy => DEFAULT_LIMIT_FUZZY,
            _ => DEFAULT_LIMIT_REGEX_EXACT,
        },
        |l| l as usize,
    );
    let pre_truncate_count = items.len();
    let truncated = pre_truncate_count > limit;
    if truncated {
        items.truncate(limit);
    }
    let total = pre_truncate_count as u64;

    Ok(SearchResult {
        items,
        total,
        truncated,
        cursor: None,
        revision: None,
    })
}

/// Drop candidates whose `NodeMetadataStore` entry reports
/// `macro_generated == Some(true)`. Mirrors the CLI's
/// `macro_boundary_keeps_node` predicate for the `include_generated == false`
/// arm with `cfg_filter == None` — the only macro-boundary mode the wire
/// currently carries.
///
/// Order-preserving so the fuzzy-score ordering survives the filter pass.
fn filter_macro_generated_hits(graph: &CodeGraph, hits: Vec<ScoredHit>) -> Vec<ScoredHit> {
    let store = graph.macro_metadata();
    hits.into_iter()
        .filter(|hit| {
            store
                .get_macro(hit.node_id)
                .is_none_or(|m| m.macro_generated != Some(true))
        })
        .collect()
}

/// Order-preserving dedup by `NodeId`. The first occurrence wins, so a
/// fuzzy hit's highest-score entry is the one that survives. `HashSet`-
/// backed retain avoids quadratic behaviour on large hit sets.
fn dedup_preserve_order(hits: Vec<ScoredHit>) -> Vec<ScoredHit> {
    let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut out: Vec<ScoredHit> = Vec::with_capacity(hits.len());
    for hit in hits {
        if seen.insert(hit.node_id) {
            out.push(hit);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Per-mode candidate generation. Mirrors the corresponding CLI branches
// in `sqry-cli/src/commands/search.rs::run_regular_search` (exact, regex)
// and `::run_fuzzy_search` (fuzzy).
// ---------------------------------------------------------------------------

fn exact_hits(graph: &CodeGraph, pattern: &str) -> Vec<ScoredHit> {
    // Contract-bound to the planner's `name:<literal>` predicate — see
    // CLI comments at `run_regular_search`.
    graph
        .snapshot()
        .find_by_exact_name(pattern)
        .into_iter()
        .map(|node_id| ScoredHit {
            node_id,
            score: None,
        })
        .collect()
}

fn regex_hits(
    graph: &CodeGraph,
    regex: &Regex,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<Vec<ScoredHit>, sqry_core::query::QueryError> {
    let strings = graph.strings();
    let indices = graph.indices();
    let mut matches: Vec<ScoredHit> = Vec::new();
    for (i, (str_id, s)) in strings.iter().enumerate() {
        if i % CANCEL_POLL_STRIDE == 0 {
            cancel.query_check()?;
        }
        if regex.is_match(s) {
            matches.extend(
                indices
                    .by_qualified_name(str_id)
                    .iter()
                    .map(|&n| ScoredHit {
                        node_id: n,
                        score: None,
                    }),
            );
            matches.extend(indices.by_name(str_id).iter().map(|&n| ScoredHit {
                node_id: n,
                score: None,
            }));
        }
    }
    Ok(matches)
}

fn fuzzy_hits(
    graph: &CodeGraph,
    pattern: &str,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<Vec<ScoredHit>, sqry_core::query::QueryError> {
    // Build trigram index on the fly from the graph's interned strings.
    // This mirrors `build_trigram_index_from_graph` in
    // `sqry-cli/src/commands/search.rs:903` so daemon and in-process
    // paths score identical candidate sets.
    let mut trigram = TrigramIndex::new();
    for (i, (str_id, s)) in graph.strings().iter().enumerate() {
        if i % CANCEL_POLL_STRIDE == 0 {
            cancel.query_check()?;
        }
        trigram.add_symbol(str_id.index() as usize, s);
    }
    let trigram_arc = Arc::new(trigram);

    let fuzzy_config = FuzzyConfig {
        max_candidates: FUZZY_MAX_CANDIDATES,
        min_similarity: FUZZY_MIN_SIMILARITY,
    };
    let generator = CandidateGenerator::with_config(trigram_arc, fuzzy_config);
    let candidate_ids = generator.generate(pattern);
    // Candidate generation walks the query trigrams' posting lists, which are
    // corpus-sized (bounded by the same interned-string set the build loop
    // above scans). Poll once here so a token flipped during the build or the
    // generate step is observed before the bounded (<= FUZZY_MAX_CANDIDATES)
    // resolve / score / sort work below, rather than only after it (issue #503
    // Phase 1, Gate B codex finding).
    cancel.query_check()?;
    if candidate_ids.is_empty() {
        return Ok(Vec::new());
    }

    let match_config = MatchConfig {
        algorithm: sqry_core::search::matcher::MatchAlgorithm::JaroWinkler,
        min_score: FUZZY_MIN_SCORE,
        case_sensitive: false,
    };
    let matcher = FuzzyMatcher::with_config(match_config);

    let strings = graph.strings();
    let resolved: Vec<(usize, Arc<str>)> = candidate_ids
        .iter()
        .filter_map(|&id| {
            let raw = u32::try_from(id).ok()?;
            let sid = sqry_core::graph::unified::string::StringId::new(raw);
            strings.resolve(sid).map(|s| (id, s))
        })
        .collect();
    let targets = resolved.iter().map(|(id, s)| (*id, s.as_ref()));
    let mut scored = matcher.match_many(pattern, targets);
    // `match_many` does not document an ordering guarantee, so sort
    // descending here. Mirrors the CLI at `sqry-cli/src/commands/search.rs:1017`.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let indices = graph.indices();
    let mut hits: Vec<ScoredHit> = Vec::new();
    for (i, result) in scored.into_iter().enumerate() {
        if i % CANCEL_POLL_STRIDE == 0 {
            cancel.query_check()?;
        }
        let Ok(raw) = u32::try_from(result.entry_id) else {
            continue;
        };
        let sid = sqry_core::graph::unified::string::StringId::new(raw);
        // Both name + qualified-name index hits for this scored string
        // inherit the same score — they represent the same fuzzy match
        // surface emerging through two index facets.
        let score = result.score.to_string().parse::<f32>().ok();
        hits.extend(
            indices
                .by_qualified_name(sid)
                .iter()
                .map(|&n| ScoredHit { node_id: n, score }),
        );
        hits.extend(
            indices
                .by_name(sid)
                .iter()
                .map(|&n| ScoredHit { node_id: n, score }),
        );
    }
    Ok(hits)
}

// ---------------------------------------------------------------------------
// NodeId -> SearchItem conversion. Pure function over the captured
// graph snapshot. Mirrors `convert_node_to_display_symbol` from
// `sqry-cli/src/commands/search.rs:755` minus the macro-metadata
// enrichment, since the wire form intentionally carries the flat shape.
// ---------------------------------------------------------------------------

fn node_to_search_item(
    graph: &CodeGraph,
    node_id: NodeId,
    score: Option<f32>,
) -> Option<SearchItem> {
    let entry = graph.nodes().get(node_id)?;
    let strings = graph.strings();
    let files = graph.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());
    let file_path = files
        .resolve(entry.file)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let language = language_from_extension(Path::new(&file_path));

    Some(SearchItem {
        name,
        qualified_name,
        kind: node_kind_to_string(entry.kind).to_owned(),
        language,
        file_path,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        score,
    })
}

// ---------------------------------------------------------------------------
// Post-conversion kind + lang filters. Mirrors `apply_search_filters` in
// `sqry-cli/src/commands/search.rs:216` so daemon and in-process paths
// agree on retention semantics.
// ---------------------------------------------------------------------------

fn apply_filters(items: &mut Vec<SearchItem>, kind: Option<&str>, lang: Option<&str>) {
    if let Some(k) = kind {
        let target = k.to_lowercase();
        items.retain(|s| s.kind.to_lowercase() == target);
    }
    if let Some(l) = lang {
        items.retain(|s| {
            Path::new(&s.file_path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| matches_language(ext, l))
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers: language + kind string mapping. The CLI keeps these in
// `sqry-cli/src/commands/search.rs` and so do `sqry-core/src/graph/diff.rs`
// + several sqry-mcp modules; the repo-wide convention is to inline the
// mapping at each call site, since no single crate currently owns a
// canonical helper. Following the same convention here keeps the
// daemon's NodeKind set wire-aligned with the existing surfaces.
// ---------------------------------------------------------------------------

fn node_kind_to_string(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::CallSite => "call_site",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Channel => "channel",
        NodeKind::Other => "other",
    }
}

fn language_from_extension(path: &Path) -> String {
    path.extension().and_then(|ext| ext.to_str()).map_or_else(
        || "unknown".to_string(),
        |ext| match ext.to_lowercase().as_str() {
            "rs" => "rust".to_string(),
            "js" | "mjs" | "cjs" => "javascript".to_string(),
            "ts" | "mts" | "cts" => "typescript".to_string(),
            "jsx" => "javascriptreact".to_string(),
            "tsx" => "typescriptreact".to_string(),
            "py" | "pyw" => "python".to_string(),
            "rb" => "ruby".to_string(),
            "go" => "go".to_string(),
            "java" => "java".to_string(),
            "kt" | "kts" => "kotlin".to_string(),
            "scala" | "sc" => "scala".to_string(),
            "c" | "h" => "c".to_string(),
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp".to_string(),
            "cs" => "csharp".to_string(),
            "php" => "php".to_string(),
            "swift" => "swift".to_string(),
            "sql" => "sql".to_string(),
            "dart" => "dart".to_string(),
            "lua" => "lua".to_string(),
            "sh" | "bash" | "zsh" => "shell".to_string(),
            "pl" | "pm" => "perl".to_string(),
            "groovy" | "gvy" => "groovy".to_string(),
            "ex" | "exs" => "elixir".to_string(),
            "r" => "r".to_string(),
            "hs" | "lhs" => "haskell".to_string(),
            "svelte" => "svelte".to_string(),
            "vue" => "vue".to_string(),
            "zig" => "zig".to_string(),
            "css" | "scss" | "sass" | "less" => "css".to_string(),
            "html" | "htm" => "html".to_string(),
            "tf" | "tfvars" => "terraform".to_string(),
            "pp" => "puppet".to_string(),
            "pls" | "plb" | "pck" => "plsql".to_string(),
            "cls" | "trigger" => "apex".to_string(),
            "abap" => "abap".to_string(),
            _ => "unknown".to_string(),
        },
    )
}

fn matches_language(ext: &str, lang: &str) -> bool {
    let ext = ext.to_lowercase();
    let lang = lang.to_lowercase();
    match lang.as_str() {
        "rust" | "rs" => ext == "rs",
        "javascript" | "js" => matches!(ext.as_str(), "js" | "jsx" | "mjs" | "cjs"),
        "typescript" | "ts" => matches!(ext.as_str(), "ts" | "tsx"),
        "python" | "py" => matches!(ext.as_str(), "py" | "pyi" | "pyw"),
        "go" => ext == "go",
        "java" => ext == "java",
        "swift" => ext == "swift",
        "c" => matches!(ext.as_str(), "c" | "h"),
        "cpp" | "c++" | "cxx" => {
            matches!(
                ext.as_str(),
                "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h"
            )
        }
        "csharp" | "c#" | "cs" => matches!(ext.as_str(), "cs" | "csx"),
        "dart" => ext == "dart",
        "kotlin" | "kt" => matches!(ext.as_str(), "kt" | "kts"),
        "ruby" | "rb" => matches!(ext.as_str(), "rb" | "rake" | "gemspec"),
        "scala" => matches!(ext.as_str(), "scala" | "sc"),
        "php" => ext == "php",
        "lua" => ext == "lua",
        "elixir" | "ex" => matches!(ext.as_str(), "ex" | "exs"),
        "haskell" | "hs" => matches!(ext.as_str(), "hs" | "lhs"),
        "perl" | "pl" => matches!(ext.as_str(), "pl" | "pm"),
        "r" => ext == "r",
        "shell" | "sh" | "bash" => matches!(ext.as_str(), "sh" | "bash" | "zsh"),
        "zig" => ext == "zig",
        "groovy" => matches!(ext.as_str(), "groovy" | "gvy" | "gy" | "gsh"),
        "vue" => ext == "vue",
        "svelte" => ext == "svelte",
        "html" => matches!(ext.as_str(), "html" | "htm"),
        "css" => matches!(ext.as_str(), "css" | "scss" | "sass" | "less"),
        "terraform" | "tf" | "hcl" => matches!(ext.as_str(), "tf" | "tfvars" | "hcl"),
        "puppet" | "pp" => ext == "pp",
        "sql" => ext == "sql",
        "servicenow" | "servicenow-xanadu" | "servicenow-xanadu-js" | "snjs" => ext == "snjs",
        "apex" | "salesforce" => matches!(ext.as_str(), "cls" | "trigger"),
        "abap" => ext == "abap",
        "plsql" | "oracle-plsql" => matches!(ext.as_str(), "pks" | "pkb" | "pls"),
        other => ext == other,
    }
}

// ---------------------------------------------------------------------------
// Tests — unit-level coverage of the pure helpers + the search pipeline
// against a hand-crafted graph. Integration coverage against a live
// daemon (parity + latency) lives in the DAEMON_SEARCH_TESTS unit.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_to_string_covers_known_variants() {
        assert_eq!(node_kind_to_string(NodeKind::Function), "function");
        assert_eq!(node_kind_to_string(NodeKind::Method), "method");
        assert_eq!(node_kind_to_string(NodeKind::Class), "class");
        assert_eq!(node_kind_to_string(NodeKind::Struct), "struct");
        assert_eq!(node_kind_to_string(NodeKind::Other), "other");
    }

    #[test]
    fn language_from_extension_maps_common_extensions() {
        assert_eq!(language_from_extension(Path::new("foo.rs")), "rust");
        assert_eq!(language_from_extension(Path::new("foo.py")), "python");
        assert_eq!(
            language_from_extension(Path::new("foo.tsx")),
            "typescriptreact"
        );
        assert_eq!(
            language_from_extension(Path::new("foo.unknownext")),
            "unknown"
        );
        assert_eq!(language_from_extension(Path::new("noext")), "unknown");
    }

    #[test]
    fn matches_language_handles_aliases_and_default_passthrough() {
        assert!(matches_language("rs", "rust"));
        assert!(matches_language("py", "python"));
        assert!(matches_language("tsx", "typescript"));
        assert!(matches_language("hpp", "cpp"));
        assert!(!matches_language("rs", "python"));
        // The catchall lets an exact-match lang/ext through (mirroring
        // the CLI's `_ => ext_lower == lang_lower`).
        assert!(matches_language("custom", "custom"));
    }

    #[test]
    fn apply_filters_drops_non_matching_kind_and_lang() {
        let mut items = vec![
            SearchItem {
                name: "a".into(),
                qualified_name: "a".into(),
                kind: "function".into(),
                language: "rust".into(),
                file_path: "a.rs".into(),
                start_line: 1,
                start_column: 0,
                end_line: 1,
                end_column: 1,
                score: None,
            },
            SearchItem {
                name: "b".into(),
                qualified_name: "b".into(),
                kind: "class".into(),
                language: "python".into(),
                file_path: "b.py".into(),
                start_line: 1,
                start_column: 0,
                end_line: 1,
                end_column: 1,
                score: None,
            },
        ];
        apply_filters(&mut items, Some("function"), None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "function");

        // lang filter on the surviving item: Rust-only.
        apply_filters(&mut items, None, Some("rust"));
        assert_eq!(items.len(), 1);

        // lang filter dropping the surviving item: Python.
        apply_filters(&mut items, None, Some("python"));
        assert!(items.is_empty());
    }

    #[test]
    fn run_search_on_graph_empty_graph_returns_empty_result() {
        let graph = CodeGraph::new();
        let req = req_for(SearchMode::Exact, "anything", None);
        let result = run_search_on_graph(&graph, &req, None);
        assert!(result.items.is_empty());
        assert_eq!(result.total, 0);
        assert!(!result.truncated);
        assert!(result.cursor.is_none());
    }

    // -------------------------------------------------------------------
    // Codex finding 2: invalid regex must surface as -32602 InvalidParams,
    // not -32603 Internal. Pre-acquisition validation owns this contract.
    // -------------------------------------------------------------------

    #[test]
    fn validate_request_rejects_empty_pattern_as_invalid_params() {
        let req = req_for(SearchMode::Exact, "", None);
        let err = validate_request(&req).expect_err("empty pattern must reject");
        assert!(
            matches!(err, MethodError::InvalidParams(_)),
            "empty pattern must map to InvalidParams (-32602), got: {err:?}"
        );
    }

    #[test]
    fn validate_request_rejects_malformed_regex_as_invalid_params() {
        // Unclosed character class is unconditionally invalid regex.
        let req = req_for(SearchMode::Regex, "[", None);
        let err = validate_request(&req).expect_err("invalid regex must reject");
        let MethodError::InvalidParams(inner) = &err else {
            panic!("invalid regex must map to InvalidParams (-32602), got: {err:?}");
        };
        let msg = format!("{inner}");
        assert!(
            msg.contains("invalid regex pattern"),
            "expected invalid-regex context, got: {msg}"
        );
        // Verify the JSON-RPC wire mapping: -32602 with the message
        // verbatim. This is what the dispatcher would send back to the
        // client, matching CLAUDE.md's daemon error taxonomy.
        let resp = err.into_jsonrpc_response(None);
        let body = serde_json::to_value(&resp).expect("response to_value");
        let code = body
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64())
            .expect("error.code present");
        assert_eq!(
            code, -32602,
            "wire code for malformed regex must be -32602, got: {code}"
        );
    }

    #[test]
    fn validate_request_compiles_regex_for_regex_mode() {
        let req = req_for(SearchMode::Regex, "fo+", None);
        let regex = validate_request(&req).expect("valid regex compiles");
        let regex = regex.expect("regex mode must yield Some(Regex)");
        assert!(regex.is_match("foo"));
        assert!(!regex.is_match("bar"));
    }

    #[test]
    fn validate_request_returns_none_for_exact_and_fuzzy_modes() {
        assert!(
            validate_request(&req_for(SearchMode::Exact, "x", None))
                .expect("exact ok")
                .is_none()
        );
        assert!(
            validate_request(&req_for(SearchMode::Fuzzy, "x", None))
                .expect("fuzzy ok")
                .is_none()
        );
    }

    // -------------------------------------------------------------------
    // Populated-graph tests: exact, regex, fuzzy. Cover the parity
    // contract DAEMON_SEARCH_HANDLER acceptance #3
    // (`find_by_exact_name` returns matching SearchItem) and Codex
    // finding 1 (fuzzy ranking + score preservation).
    // -------------------------------------------------------------------

    /// Build a small `CodeGraph` with four named function nodes — two
    /// rust files, one python file — so the tests can exercise exact /
    /// regex / fuzzy / kind / lang / limit semantics deterministically.
    fn build_populated_graph() -> CodeGraph {
        use sqry_core::graph::unified::storage::arena::NodeEntry;

        let mut graph = CodeGraph::new();
        let rs_file = graph
            .files_mut()
            .register(Path::new("foo.rs"))
            .expect("register foo.rs");
        let py_file = graph
            .files_mut()
            .register(Path::new("bar.py"))
            .expect("register bar.py");

        for (name, file) in [
            ("alpha", rs_file),
            ("beta", rs_file),
            ("alphabet", rs_file),
            ("delta", py_file),
        ] {
            let name_id = graph.strings_mut().intern(name).expect("intern name");
            let entry = NodeEntry::new(NodeKind::Function, name_id, file).with_location(1, 0, 1, 4);
            graph.nodes_mut().alloc(entry).expect("alloc node");
        }
        graph.rebuild_indices();
        graph
    }

    fn req_for(mode: SearchMode, pattern: &str, limit: Option<u32>) -> SearchRequest {
        SearchRequest {
            envelope_version: 1,
            pattern: pattern.into(),
            search_path: "/tmp".into(),
            mode,
            kind: None,
            lang: None,
            limit,
            // Match the wire default (`true`): keep macro-generated symbols.
            // Tests that exercise the `false` arm set this explicitly.
            include_generated: true,
            revision: None,
        }
    }

    // -------------------------------------------------------------------
    // issue #503 Phase 1: cancellation is delivered on the search read
    // paths, and the deadline envelope is unchanged.
    // -------------------------------------------------------------------

    /// AC2 (scan): the regex candidate scan observes a flipped token and
    /// returns `QueryError::Cancelled` instead of scanning to completion.
    #[test]
    fn search_regex_scan_observes_flipped_token() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Regex, "alpha", None);
        let regex = Regex::new("alpha").expect("valid regex");
        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        cancel.cancel();
        let res = run_search_on_graph_cancellable(&graph, &req, Some(&regex), &cancel);
        assert!(
            matches!(res, Err(sqry_core::query::QueryError::Cancelled)),
            "flipped token must yield Cancelled, got: {res:?}"
        );
    }

    /// AC2 (fuzzy): the fuzzy path (trigram build loop, then the poll after
    /// candidate generation) observes a flipped token and returns Cancelled
    /// rather than building/scoring to completion.
    #[test]
    fn search_fuzzy_scan_observes_flipped_token() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Fuzzy, "alpha", None);
        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        cancel.cancel();
        let res = run_search_on_graph_cancellable(&graph, &req, None, &cancel);
        assert!(
            matches!(res, Err(sqry_core::query::QueryError::Cancelled)),
            "flipped token must yield Cancelled for fuzzy mode, got: {res:?}"
        );
    }

    /// AC2 (pre-step poll): exact mode has no per-string poll, but the
    /// poll before the post-processing steps still observes the flip.
    #[test]
    fn search_exact_pre_step_poll_observes_flipped_token() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Exact, "alpha", None);
        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        cancel.cancel();
        let res = run_search_on_graph_cancellable(&graph, &req, None, &cancel);
        assert!(
            matches!(res, Err(sqry_core::query::QueryError::Cancelled)),
            "flipped token must yield Cancelled even for exact mode, got: {res:?}"
        );
    }

    /// Negative control: a live (never-flipped) token does not false-trigger;
    /// the scan completes and returns the same items the wrapper would.
    #[test]
    fn search_live_token_completes_and_matches_wrapper() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Regex, "alpha", None);
        let regex = Regex::new("alpha").expect("valid regex");
        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        let cancellable = run_search_on_graph_cancellable(&graph, &req, Some(&regex), &cancel)
            .expect("live token must not cancel");
        let wrapper = run_search_on_graph(&graph, &req, Some(&regex));
        assert_eq!(cancellable.total, wrapper.total);
        assert_eq!(cancellable.items.len(), wrapper.items.len());
        assert!(cancellable.total > 0, "regex 'alpha' should match symbols");
    }

    /// AC3: the shared `classify_closure_error` ladder (used by both
    /// `CpuExecutor::run` and the central `execute_with_timeout`) routes a
    /// `QueryError::Cancelled` closure error to the `-32000` `ToolTimeout`
    /// envelope and every other error to `Internal` (-32603). This is the
    /// envelope contract the revision handlers now rely on via
    /// `ctx.cpu_executor.run(...)`.
    #[test]
    fn classify_closure_error_routes_cancelled_and_other() {
        let cancelled = crate::ipc::tool_core::classify_closure_error(
            anyhow::Error::new(sqry_core::query::QueryError::Cancelled),
            std::path::Path::new("/ws"),
            0,
            50,
        );
        let resp = MethodError::Daemon(cancelled).into_jsonrpc_response(None);
        let code = serde_json::to_value(&resp)
            .expect("to_value")
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64())
            .expect("error.code");
        assert_eq!(
            code, -32000,
            "Cancelled must map to ToolTimeout wire -32000"
        );

        let other = crate::ipc::tool_core::classify_closure_error(
            anyhow::anyhow!("some other failure"),
            std::path::Path::new("/ws"),
            0,
            50,
        );
        assert!(
            matches!(other, crate::error::DaemonError::Internal(_)),
            "non-cancellation error must stay Internal, got: {other:?}"
        );
    }

    /// AC2 (mechanism): the revision-handler wrapper shape (spawn_blocking +
    /// timeout + fire-and-forget flip) makes a cooperative closure observe
    /// the deadline-flipped token and stop, rather than running to natural
    /// completion. This is the exact mechanism the timed-out revision search
    /// / query now relies on to free its blocking-pool slot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revision_deadline_flip_makes_closure_observe_and_stop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        let cancel_for_closure = cancel.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let observed_c = Arc::clone(&observed);

        let join = tokio::task::spawn_blocking(move || {
            // Cooperative spin, bounded so a broken test cannot hang.
            for _ in 0..5_000 {
                if cancel_for_closure.is_cancelled() {
                    observed_c.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let timed = tokio::time::timeout(Duration::from_millis(30), join).await;
        assert!(timed.is_err(), "the short deadline must fire first");
        // Handler contract: flip on deadline, do NOT await the JoinHandle.
        cancel.cancel();

        // Confirm the detached closure observed the flip and stopped.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !observed.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("closure must observe the flipped token and stop");
        assert!(observed.load(Ordering::SeqCst));
    }

    // -------------------------------------------------------------------
    // include_generated parity (Codex round-1 CLI shim review blocker).
    //
    // The CLI's default `--exact` invocation has `include_generated == false`
    // and drops `macro_generated == Some(true)` nodes via
    // `filter_nodes_by_macro_boundary`. Without the matching daemon-side
    // filter, the daemon path silently returned a superset of what the
    // in-process path returns. These tests pin the parity contract at the
    // handler level so a regression in `filter_macro_generated_hits` /
    // `run_search_on_graph` is caught even before the cross-binary parity
    // test in `sqry-daemon/tests/search_handler.rs` runs.
    // -------------------------------------------------------------------

    /// Build a populated graph plus a macro-generated sibling node. The
    /// sibling shares the candidate's exact name string so an exact-mode
    /// lookup pulls both NodeIds; the filter is what distinguishes them.
    fn build_graph_with_macro_generated() -> (CodeGraph, NodeKind) {
        use sqry_core::graph::unified::storage::arena::NodeEntry;
        use sqry_core::graph::unified::storage::metadata::MacroNodeMetadata;

        let mut graph = CodeGraph::new();
        let rs_file = graph
            .files_mut()
            .register(Path::new("foo.rs"))
            .expect("register foo.rs");
        // User-authored "alpha"
        let user_name = graph.strings_mut().intern("alpha").expect("intern alpha");
        let user_entry =
            NodeEntry::new(NodeKind::Function, user_name, rs_file).with_location(1, 0, 1, 5);
        graph
            .nodes_mut()
            .alloc(user_entry)
            .expect("alloc user alpha");
        // Macro-generated "alpha" (same name string is fine — exact lookup
        // pulls every NodeId that resolves through `find_by_exact_name`).
        let macro_name = graph
            .strings_mut()
            .intern("alpha")
            .expect("intern macro alpha");
        let macro_entry =
            NodeEntry::new(NodeKind::Function, macro_name, rs_file).with_location(2, 0, 2, 5);
        let macro_id = graph
            .nodes_mut()
            .alloc(macro_entry)
            .expect("alloc macro alpha");
        graph.macro_metadata_mut().insert(
            macro_id,
            MacroNodeMetadata {
                macro_generated: Some(true),
                macro_source: Some("derive_Debug".to_string()),
                cfg_condition: None,
                cfg_active: None,
                proc_macro_kind: None,
                expansion_cached: None,
                unresolved_attributes: Vec::new(),
            },
        );
        graph.rebuild_indices();
        (graph, NodeKind::Function)
    }

    #[test]
    fn run_search_on_graph_drops_macro_generated_when_include_generated_false() {
        let (graph, _) = build_graph_with_macro_generated();
        let mut req = req_for(SearchMode::Exact, "alpha", None);
        req.include_generated = false;
        let result = run_search_on_graph(&graph, &req, None);
        assert_eq!(
            result.total, 1,
            "include_generated=false must drop the macro_generated alpha sibling, got: {result:?}",
        );
        assert_eq!(result.items.len(), 1);
        // The remaining hit must be the user-authored node (lines 1..=1).
        assert_eq!(result.items[0].start_line, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn run_search_on_graph_keeps_macro_generated_when_include_generated_true() {
        let (graph, _) = build_graph_with_macro_generated();
        let req = req_for(SearchMode::Exact, "alpha", None); // default: include_generated=true
        let result = run_search_on_graph(&graph, &req, None);
        assert_eq!(
            result.total, 2,
            "include_generated=true must surface both alphas, got: {result:?}",
        );
        assert_eq!(result.items.len(), 2);
    }

    #[test]
    fn run_search_on_graph_default_request_keeps_macro_generated() {
        // Defensive: a request body produced via the wire default
        // (deserialised with the field missing) must keep macro-generated
        // hits, preserving the pre-Codex-fix HANDLER unit's approved
        // shape — i.e., the new field is back-compat-safe.
        let (graph, _) = build_graph_with_macro_generated();
        let wire = serde_json::json!({
            "envelope_version": sqry_daemon_protocol::ENVELOPE_VERSION,
            "pattern": "alpha",
            "search_path": "/tmp",
            "mode": "exact"
        });
        let req: SearchRequest = serde_json::from_value(wire).expect("default-deserialise");
        assert!(
            req.include_generated,
            "wire default must keep include_generated=true"
        );
        let result = run_search_on_graph(&graph, &req, None);
        assert_eq!(result.total, 2);
    }

    #[test]
    fn run_search_on_graph_exact_finds_by_exact_name() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Exact, "alpha", None);
        let result = run_search_on_graph(&graph, &req, None);
        assert_eq!(
            result.total, 1,
            "expected exactly one exact hit for 'alpha'"
        );
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, "alpha");
        assert_eq!(result.items[0].kind, "function");
        assert_eq!(result.items[0].language, "rust");
        assert!(result.items[0].score.is_none(), "exact hits carry no score");
        assert!(!result.truncated);
    }

    #[test]
    fn run_search_on_graph_regex_uses_precompiled_pattern() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Regex, "^alpha", None);
        let regex = validate_request(&req)
            .expect("regex compile")
            .expect("regex mode yields regex");
        let result = run_search_on_graph(&graph, &req, Some(&regex));
        let names: Vec<&str> = result.items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            names.contains(&"alpha"),
            "regex 'alpha' must match: {names:?}"
        );
        assert!(
            names.contains(&"alphabet"),
            "regex '^alpha' must match alphabet too: {names:?}"
        );
        assert!(
            !names.contains(&"beta"),
            "regex '^alpha' must NOT match beta: {names:?}"
        );
        for item in &result.items {
            assert!(item.score.is_none(), "regex hits carry no score");
        }
    }

    #[test]
    fn run_search_on_graph_fuzzy_preserves_score_descending_order() {
        // Codex finding 1: ordering by NodeId destroyed fuzzy ranking.
        // With the refactor, fuzzy results must remain score-descending
        // and SearchItem.score must be populated.
        let graph = build_populated_graph();
        // 'alph' is a typo-style query that scores highest on 'alpha'
        // and progressively lower on 'alphabet' / 'beta'. Threshold +
        // jaro-winkler defaults in `fuzzy_hits` decide what survives.
        let req = req_for(SearchMode::Fuzzy, "alph", None);
        let result = run_search_on_graph(&graph, &req, None);
        assert!(
            !result.items.is_empty(),
            "fuzzy search must produce at least one hit for 'alph'"
        );
        for item in &result.items {
            assert!(
                item.score.is_some(),
                "every fuzzy hit must carry a score; got: {item:?}"
            );
        }
        // Score-descending ordering invariant.
        for pair in result.items.windows(2) {
            let a = pair[0].score.unwrap();
            let b = pair[1].score.unwrap();
            assert!(
                a >= b,
                "fuzzy items must be sorted score-descending; {a} >= {b} failed at {:?}",
                pair
            );
        }
        // 'alpha' is the closest jaro-winkler match for 'alph' so it
        // should be ranked first when present in the result set.
        if result.items.iter().any(|i| i.name == "alpha") {
            assert_eq!(
                result.items[0].name, "alpha",
                "fuzzy top hit for 'alph' must be 'alpha' when present: {:?}",
                result.items
            );
        }
    }

    // -------------------------------------------------------------------
    // Codex finding 3 follow-up: limit / total / truncated semantics
    // must be exercised by unit tests (not just helper coverage).
    // -------------------------------------------------------------------

    #[test]
    fn run_search_on_graph_limit_truncates_and_reports_pre_truncate_total() {
        let graph = build_populated_graph();
        // `.*` matches everything via the regex path → all four nodes
        // qualify, then a limit of 2 must:
        //   - retain exactly two items
        //   - report total = 4 (pre-truncate count)
        //   - flip truncated = true
        let req = req_for(SearchMode::Regex, ".*", Some(2));
        let regex = validate_request(&req)
            .expect("regex compile")
            .expect("regex mode yields regex");
        let result = run_search_on_graph(&graph, &req, Some(&regex));
        assert_eq!(result.items.len(), 2, "limit must cap items");
        assert_eq!(
            result.total, 4,
            "total must report the pre-truncate count, got: {result:?}"
        );
        assert!(
            result.truncated,
            "truncated must be true when items were capped"
        );
    }

    // -------------------------------------------------------------------
    // Wire-shape parity test. DAG acceptance criterion (HANDLER, line
    // 493 of the DAG TOML) requires "wire shape parity verified by a
    // parity test: in-process search and daemon search produce identical
    // SqrySearchItem sets for the same fixture". The cross-transport
    // (live-daemon vs CLI) parity test owned by the DAEMON_SEARCH_TESTS
    // unit (separate DAG unit, separate integration file) needs a live
    // sqryd and cannot run as a unit test; this unit-level companion
    // pins the wire-shape contract at the data-model boundary so the
    // SearchItem fields emitted by the daemon are bit-identical to the
    // NodeEntry data they're derived from.
    //
    // Rationale: any future refactor that changes `node_to_search_item`
    // — kind mapping, language detection, span fields, qualified-name
    // fallback — would silently diverge the daemon path from the CLI's
    // `convert_node_to_display_symbol` (sqry-cli/src/commands/search.rs).
    // This test fails closed on any such drift by re-deriving the
    // expected wire shape directly from the same NodeEntry/interner the
    // CLI would consume.
    // -------------------------------------------------------------------

    // -------------------------------------------------------------------
    // In-process vs daemon parity test. DAG line 493 names the literal
    // contract: "in-process search and daemon search produce identical
    // SqrySearchItem sets for the same fixture". The cross-transport
    // (live sqryd vs CLI binary) version lives in DAEMON_SEARCH_TESTS
    // per DAG line 627-629; this is the unit-level realization.
    //
    // The "in-process" projection below is intentionally a fork of the
    // CLI's projection logic — `node_kind_to_string` and the language
    // mapping are duplicated here rather than reused from the daemon's
    // helpers, so any silent drift between daemon's `node_to_search_item`
    // and the CLI's `convert_node_to_display_symbol` would fail this
    // test (one of the issues Codex round-4 flagged: re-using the
    // daemon's own helpers for the expected values lets drift hide).
    // -------------------------------------------------------------------

    /// Mirror of `sqry-cli/src/commands/search.rs::node_kind_to_string`.
    /// Kept lexically independent of the daemon's `node_kind_to_string`
    /// so the parity comparison fails closed if the daemon's mapping
    /// drifts away from the CLI's.
    fn cli_equivalent_node_kind_to_string(kind: NodeKind) -> &'static str {
        match kind {
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Class => "class",
            NodeKind::Interface => "interface",
            NodeKind::Trait => "trait",
            NodeKind::Module => "module",
            NodeKind::Variable => "variable",
            NodeKind::Constant => "constant",
            NodeKind::Type => "type",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            NodeKind::EnumVariant => "enum_variant",
            NodeKind::Macro => "macro",
            NodeKind::Parameter => "parameter",
            NodeKind::Property => "property",
            NodeKind::Import => "import",
            NodeKind::Export => "export",
            NodeKind::Component => "component",
            NodeKind::Service => "service",
            NodeKind::Resource => "resource",
            NodeKind::Endpoint => "endpoint",
            NodeKind::Test => "test",
            NodeKind::CallSite => "call_site",
            NodeKind::StyleRule => "style_rule",
            NodeKind::StyleAtRule => "style_at_rule",
            NodeKind::StyleVariable => "style_variable",
            NodeKind::Lifetime => "lifetime",
            NodeKind::TypeParameter => "type_parameter",
            NodeKind::Annotation => "annotation",
            NodeKind::AnnotationValue => "annotation_value",
            NodeKind::LambdaTarget => "lambda_target",
            NodeKind::JavaModule => "java_module",
            NodeKind::EnumConstant => "enum_constant",
            NodeKind::Channel => "channel",
            NodeKind::Other => "other",
        }
    }

    /// Mirror of `sqry-cli/src/commands/search.rs::language_from_path`.
    /// Lexically independent of the daemon's `language_from_extension`.
    fn cli_equivalent_language_from_path(path: &Path) -> String {
        path.extension().and_then(|ext| ext.to_str()).map_or_else(
            || "unknown".to_string(),
            |ext| match ext.to_lowercase().as_str() {
                "rs" => "rust".to_string(),
                "js" | "mjs" | "cjs" => "javascript".to_string(),
                "ts" | "mts" | "cts" => "typescript".to_string(),
                "jsx" => "javascriptreact".to_string(),
                "tsx" => "typescriptreact".to_string(),
                "py" | "pyw" => "python".to_string(),
                "rb" => "ruby".to_string(),
                "go" => "go".to_string(),
                "java" => "java".to_string(),
                "kt" | "kts" => "kotlin".to_string(),
                "scala" | "sc" => "scala".to_string(),
                "c" | "h" => "c".to_string(),
                "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp".to_string(),
                "cs" => "csharp".to_string(),
                "php" => "php".to_string(),
                "swift" => "swift".to_string(),
                "sql" => "sql".to_string(),
                "dart" => "dart".to_string(),
                "lua" => "lua".to_string(),
                "sh" | "bash" | "zsh" => "shell".to_string(),
                "pl" | "pm" => "perl".to_string(),
                "groovy" | "gvy" => "groovy".to_string(),
                "ex" | "exs" => "elixir".to_string(),
                "r" => "r".to_string(),
                "hs" | "lhs" => "haskell".to_string(),
                "svelte" => "svelte".to_string(),
                "vue" => "vue".to_string(),
                "zig" => "zig".to_string(),
                "css" | "scss" | "sass" | "less" => "css".to_string(),
                "html" | "htm" => "html".to_string(),
                "tf" | "tfvars" => "terraform".to_string(),
                "pp" => "puppet".to_string(),
                "pls" | "plb" | "pck" => "plsql".to_string(),
                "cls" | "trigger" => "apex".to_string(),
                "abap" => "abap".to_string(),
                _ => "unknown".to_string(),
            },
        )
    }

    /// Test-local "in-process search projection" mirroring
    /// `sqry-cli/src/commands/search.rs::run_regular_search` (exact mode)
    /// + `convert_node_to_display_symbol`. Produces `SearchItem` values
    ///   (the daemon wire form) so they can be compared directly against
    ///   `run_search_on_graph` output.
    ///
    /// Independent of the daemon's `node_to_search_item` so any
    /// projection drift between daemon and CLI would surface in the
    /// parity assertion below.
    fn in_process_search_projection_exact(graph: &CodeGraph, pattern: &str) -> Vec<SearchItem> {
        let snapshot = graph.snapshot();
        let mut node_ids = snapshot.find_by_exact_name(pattern);
        // Match daemon's exact-mode normalization (sort + dedup by id).
        node_ids.sort_unstable();
        node_ids.dedup();

        node_ids
            .into_iter()
            .filter_map(|nid| {
                let entry = graph.nodes().get(nid)?;
                let strings = graph.strings();
                let files = graph.files();
                let name = strings.resolve(entry.name).map(|s| s.to_string())?;
                let qualified_name = entry
                    .qualified_name
                    .and_then(|id| strings.resolve(id))
                    .map_or_else(|| name.clone(), |s| s.to_string());
                let file_path = files
                    .resolve(entry.file)
                    .map(|p| p.to_string_lossy().into_owned())?;
                let language = cli_equivalent_language_from_path(Path::new(&file_path));
                let kind = cli_equivalent_node_kind_to_string(entry.kind).to_owned();
                Some(SearchItem {
                    name,
                    qualified_name,
                    kind,
                    language,
                    file_path,
                    start_line: entry.start_line,
                    start_column: entry.start_column,
                    end_line: entry.end_line,
                    end_column: entry.end_column,
                    score: None,
                })
            })
            .collect()
    }

    #[test]
    fn daemon_and_in_process_exact_search_produce_identical_search_item_sets() {
        // DAG line 493 unit-level realization. The in-process projection
        // above is a fork of the CLI's `run_regular_search` exact-mode
        // pipeline + `convert_node_to_display_symbol`. Both paths operate
        // on the same fixture graph; the resulting SearchItem vectors
        // must be byte-equal.
        let graph = build_populated_graph();
        let patterns = ["alpha", "beta", "alphabet", "delta", "nonexistent"];
        for pat in patterns {
            let req = req_for(SearchMode::Exact, pat, None);
            let daemon_items = run_search_on_graph(&graph, &req, None).items;
            let in_process_items = in_process_search_projection_exact(&graph, pat);
            assert_eq!(
                daemon_items, in_process_items,
                "in-process vs daemon parity FAILED for pattern '{pat}':\n  \
                 daemon = {daemon_items:#?}\n  in_process = {in_process_items:#?}"
            );
        }
    }

    #[test]
    fn daemon_search_item_wire_shape_matches_node_entry_canonical_fields() {
        let graph = build_populated_graph();
        let req = req_for(SearchMode::Exact, "alpha", None);
        let result = run_search_on_graph(&graph, &req, None);
        assert_eq!(result.items.len(), 1, "fixture has one 'alpha' node");
        let item = &result.items[0];

        // Re-derive every wire field directly from the graph so any
        // refactor in `node_to_search_item` that drops or rewrites a
        // field fails this test loudly. This is the unit-level
        // companion to the integration parity test that
        // `sqry-daemon/tests/search_handler.rs` (DAEMON_SEARCH_TESTS)
        // will own against a live daemon.
        let snapshot = graph.snapshot();
        let candidates = snapshot.find_by_exact_name("alpha");
        assert_eq!(candidates.len(), 1, "expected one NodeId for 'alpha'");
        let nid = candidates[0];
        let entry = graph.nodes().get(nid).expect("alpha NodeEntry");
        let expected_name = graph
            .strings()
            .resolve(entry.name)
            .expect("alpha name interned")
            .to_string();
        let expected_qn = entry
            .qualified_name
            .and_then(|id| graph.strings().resolve(id))
            .map_or_else(|| expected_name.clone(), |s| s.to_string());
        let expected_file = graph
            .files()
            .resolve(entry.file)
            .expect("alpha file registered")
            .to_string_lossy()
            .into_owned();
        let expected_lang = language_from_extension(Path::new(&expected_file));
        let expected_kind = node_kind_to_string(entry.kind).to_owned();

        assert_eq!(item.name, expected_name, "name wire field");
        assert_eq!(
            item.qualified_name, expected_qn,
            "qualified_name wire field (with fallback)"
        );
        assert_eq!(item.file_path, expected_file, "file_path wire field");
        assert_eq!(item.language, expected_lang, "language wire field");
        assert_eq!(item.kind, expected_kind, "kind wire field");
        assert_eq!(item.start_line, entry.start_line, "start_line wire field");
        assert_eq!(
            item.start_column, entry.start_column,
            "start_column wire field"
        );
        assert_eq!(item.end_line, entry.end_line, "end_line wire field");
        assert_eq!(item.end_column, entry.end_column, "end_column wire field");
        assert!(
            item.score.is_none(),
            "exact-mode hits must carry None score"
        );
    }

    #[test]
    fn run_search_on_graph_kind_and_lang_filters_preserve_order() {
        let graph = build_populated_graph();
        // Regex `.*` then lang=rust must keep only the rs entries.
        let mut req = req_for(SearchMode::Regex, ".*", None);
        req.lang = Some("rust".into());
        let regex = validate_request(&req)
            .expect("regex compile")
            .expect("regex mode yields regex");
        let result = run_search_on_graph(&graph, &req, Some(&regex));
        assert!(!result.items.is_empty(), "rust filter must keep some hits");
        for item in &result.items {
            assert_eq!(item.language, "rust", "lang filter must drop non-rust");
        }
    }
}
