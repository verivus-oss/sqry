//! Search tool execution.
//!
//! This module implements the semantic search and `find_similar` tools
//! for querying and finding related symbols in the codebase.
//!
//! Uses native graph types (`NodeId`, `NodeEntry`) directly without intermediate
//! Symbol conversion.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};
use sqry_core::search::matcher::{FuzzyMatcher, MatchConfig};

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{SearchSimilarArgs, SemanticSearchArgs, StructuralSimilarArgs};

use crate::execution::location::node_location_for_reporting_snapshot;
use crate::execution::types::{
    FindSimilarData, SemanticSearchData, SimilarSymbolData, StructuralNeighborData,
    StructuralSimilarData, ToolExecution,
};
use crate::execution::utils::{duration_to_ms, paginate};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticSortKey {
    display_name: String,
    relative_path: String,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
}

/// Execute the `semantic_search` tool to find symbols matching a query.
///
/// Uses native graph types (`NodeId`) directly without intermediate Symbol conversion.
/// Resolve workspace path from args.path parameter.
///
/// If path is "." (default), returns None to trigger discovery.
/// Otherwise returns Some(path) for explicit workspace resolution.
fn resolve_workspace_path(path: &str) -> Option<PathBuf> {
    // Issue #394: resolve a subdirectory `path` to its owning workspace instead
    // of failing as if it were a workspace root. Subtree scoping for list/scan
    // tools is applied separately via `resolve_workspace_scope`.
    crate::execution::workspace_scope::resolve_workspace_selector(path)
        .ok()
        .flatten()
}

/// Resolve workspace root for security checking only (no validation required).
///
/// This function determines the workspace root path without requiring a valid
/// sqry workspace to exist. It's used for security checks to ensure paths don't
/// escape the workspace boundary, even if the workspace hasn't been initialized yet.
///
/// Priority order:
/// 1. `SQRY_MCP_WORKSPACE_ROOT` env var (primary security boundary)
/// 2. `SQRY_WORKSPACE_ROOT` env var (backward compatibility)
/// 3. Current working directory
fn resolve_workspace_root_for_security() -> Result<PathBuf> {
    use std::env;

    if let Some(workspace_root) = crate::workspace_session::current_workspace_override() {
        return Ok(workspace_root);
    }

    // Check SQRY_MCP_WORKSPACE_ROOT first (primary)
    if let Ok(root) = env::var("SQRY_MCP_WORKSPACE_ROOT") {
        let path = PathBuf::from(root);
        return std::fs::canonicalize(&path)
            .with_context(|| format!("Failed to canonicalize workspace root: {}", path.display()));
    }

    // Check SQRY_WORKSPACE_ROOT (backward compatibility)
    if let Ok(root) = env::var("SQRY_WORKSPACE_ROOT") {
        let path = PathBuf::from(root);
        return std::fs::canonicalize(&path)
            .with_context(|| format!("Failed to canonicalize workspace root: {}", path.display()));
    }

    // Fallback to current directory
    let cwd = env::current_dir().context("Failed to get current directory")?;
    std::fs::canonicalize(&cwd).with_context(|| {
        format!(
            "Failed to canonicalize current directory: {}",
            cwd.display()
        )
    })
}

fn semantic_sort_key(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> SemanticSortKey {
    let Some(entry) = snapshot.get_node(node_id) else {
        return SemanticSortKey {
            display_name: String::new(),
            relative_path: String::new(),
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
    };

    let strings = snapshot.strings();
    let files = snapshot.files();
    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let display_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);
    let relative_path = files.resolve(entry.file).map_or_else(String::new, |path| {
        crate::execution::symbol_utils::relative_path_forward_slash(
            workspace_root.join(path.as_ref()),
            workspace_root,
        )
    });

    // NOTE: SemanticSortKey uses raw entry spans for deterministic sort ordering,
    // not for user-visible display. Do NOT migrate to node_location_for_reporting.
    SemanticSortKey {
        display_name,
        relative_path,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
    }
}

/// Execute the `semantic_search` tool.
///
/// # Errors
///
/// Returns an error if workspace security validation, graph acquisition, query
/// planning, or search execution fails.
pub fn execute_semantic_search(
    args: &SemanticSearchArgs,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<ToolExecution<SemanticSearchData>> {
    if has_revision_selector(args) {
        bail!("revision selectors require daemon-hosted semantic_search with a loaded revision");
    }
    // SECURITY: Validate path BEFORE loading workspace/engine.
    // SGA03 keeps this preflight in place: the standalone-MCP
    // `canonicalize_in_workspace` enforces invalid-params for symlink-escape
    // and outside-workspace paths *before* `engine_for_workspace`.
    // `Engine::ensure_graph` then routes through `FilesystemGraphProvider`,
    // which performs its own (compatible) path-policy check before any
    // disk graph load. The two layers compose without producing different
    // error variants because the standalone preflight always runs first
    // when the request originated from MCP.
    let workspace_root_for_security = resolve_workspace_root_for_security()?;

    // Check path security before any other operations
    let search_root = canonicalize_in_workspace(&args.path, &workspace_root_for_security)?;

    // Now load the engine (requires valid workspace)
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    // Pre-refactor timing: `start` fires before `ensure_graph`; preserve by
    // taking the instant here and threading it into `inner::`.
    let start = Instant::now();

    // SGA03: `engine.ensure_graph()` is now backed by
    // `FilesystemGraphProvider` with `MissingGraphPolicy::AutoBuildIfEnabled`
    // and an auto-build hook that retains the existing daemon-conflict
    // check + `build_and_persist_graph` flow. Wire shape, response
    // envelope, and `graph_metadata` are unchanged.
    let graph = engine.ensure_graph()?;
    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_semantic_search(&ctx, args, &search_root, start, cancel)
}

pub(crate) fn has_revision_selector(args: &SemanticSearchArgs) -> bool {
    args.revision_id.is_some()
        || args.revision_ref.is_some()
        || args.revision_commit.is_some()
        || args.revision_tree.is_some()
        || args.revision_dirty
        || args.revision_include_untracked
        || args.revision_include_ignored
}

pub(crate) mod inner {
    use super::{NodeId, Result, SemanticSearchArgs, SemanticSearchData, ToolExecution};
    use crate::daemon_adapter::WorkspaceContext;
    use crate::execution::symbol_utils::{build_search_hits_from_nodes, filter_node};
    use crate::execution::utils::{duration_to_ms, paginate};
    use anyhow::bail;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Instant;

    /// Daemon/SqryServer-shared body for `semantic_search`.
    ///
    /// `search_root` is the caller-canonicalized target directory (NOT the
    /// workspace root). The caller is responsible for the
    /// `resolve_workspace_root_for_security` + `canonicalize_in_workspace`
    /// preflight that enforces the workspace boundary on `args.path`. `start`
    /// is supplied by the caller so pre-refactor timing (which enclosed
    /// `ensure_graph`) is preserved exactly.
    pub(crate) fn execute_semantic_search(
        ctx: &WorkspaceContext,
        args: &SemanticSearchArgs,
        search_root: &Path,
        start: Instant,
        cancel: &sqry_core::query::cancellation::CancellationToken,
    ) -> Result<ToolExecution<SemanticSearchData>> {
        let query = args.query.trim();
        if query.is_empty() {
            bail!("query cannot be empty");
        }

        tracing::debug!(
            query = %query,
            path = %search_root.display(),
            max_results = args.max_results,
            context_lines = args.context_lines,
            framework = ?args.framework,
            resolved_via_filter = ?args.resolved_via,
            "Executing semantic_search tool"
        );
        // Phase β joint-stubs: framework / resolved_via filter params are
        // threaded end-to-end (MCP → params → args struct → executor).
        // The canonical predicate-evaluation path for these filters is
        // the planner pipeline reached via
        // `sqry-mcp::execution::tools::planner_query::overlay_phase_beta_filters`
        // (which appends `Predicate::FrameworkEq` / `Predicate::ResolvedViaEq`
        // as a trailing `Filter` step — both predicates evaluate fully via
        // `NodeMetadataStore::framework_route` and outgoing `Calls`
        // `resolved_via` field probes). The legacy semantic-search
        // executor's regex-based hot loop does not re-evaluate the
        // predicates here; the args propagate so daemon-side logging /
        // future plan-fusion can observe them without breaking ABI.
        let _ = (&args.framework, &args.resolved_via);

        // Get the graph for filtering
        let snapshot = ctx.graph.snapshot();

        // `A_cancellation.md` §3 + §A4 + `C_budget.md` §C5: route
        // through the budgeted+cancellable executor so the
        // in-flight rayon scan inside `evaluate_all` polls the
        // per-request token at every CANCELLATION_POLL_BATCH-node
        // boundary AND `tick()`'s every node against the runtime
        // row budget. When either signal trips, `evaluate_all`
        // funnels through `classify_cancel` which chooses the
        // typed error variant from the budget's source-tag.
        //
        // The wrapper-side downcast at
        // `sqry-mcp/src/server.rs::execute_tool_with_timeout`
        // reshapes `BudgetExceeded` into the canonical
        // `RpcError::query_too_broad` envelope with
        // `details.source = "runtime_budget"` (vs the static-gate
        // `details.source = "static_estimate"` from cluster B),
        // and `QueryError::Cancelled` into
        // `RpcError::deadline_exceeded` (cluster A).
        let budget = sqry_core::query::budget::QueryBudget::from_per_call_or_env(
            args.budget_rows,
            cancel.clone(),
        );
        // Preserve the typed root error so the wrapper downcast in
        // `sqry-mcp/src/server.rs::execute_tool_with_timeout` can recover
        // `QueryError::Cancelled` (→ `deadline_exceeded`) and
        // `BudgetExceeded` (→ `query_too_broad`). Using `anyhow!` would
        // rewrap as a string and break both downcasts (cluster-A iter-1
        // BLOCKER + cluster-C iter-1 BLOCKER 2). The executor already
        // returns `anyhow::Error`; propagate it verbatim so the typed
        // root remains the discriminator at the wrapper boundary.
        let query_results = ctx.executor.execute_on_preloaded_graph_with_budget(
            Arc::clone(&ctx.graph),
            query,
            search_root,
            None,
            &budget,
        )?;

        let nodes_searched = query_results.len();
        let elapsed = duration_to_ms(start.elapsed());

        // Filter using NodeId + graph lookups
        let mut filtered: Vec<NodeId> = query_results
            .node_ids()
            .iter()
            .filter(|&&node_id| filter_node(&snapshot, node_id, &args.filters))
            .copied()
            .collect();

        // C_SUPPRESS: drop synthetic placeholder nodes (Go-plugin
        // `<field:...>` shadows + `<ident>@<offset>` per-binding-site
        // Variables) from the user-facing surface. The executor's
        // node-name scan can surface these directly without going
        // through GraphSnapshot::find_by_pattern, so we re-apply the
        // suppression check here at the MCP boundary. There is no
        // user-visible opt-in for synthetic nodes — they are an
        // internal binding-plane implementation detail.
        filtered.retain(|&node_id| !snapshot.is_node_synthetic(node_id));

        // Filter out classpath (external) nodes unless explicitly requested
        if !args.include_classpath {
            filtered.retain(|&node_id| {
                !crate::execution::symbol_utils::is_node_external(&snapshot, node_id)
            });
        }

        // Sort by user-facing display identity for deterministic MCP output.
        let workspace_root: &Path = &ctx.workspace_root;
        filtered
            .sort_by_key(|&node_id| super::semantic_sort_key(&snapshot, node_id, workspace_root));

        // Apply score (all results get 1.0 for now since executor doesn't provide scores)
        let mut scored: Vec<(NodeId, f64)> =
            filtered.into_iter().map(|node_id| (node_id, 1.0)).collect();

        if let Some(min_score) = args.score_min {
            scored.retain(|(_, score)| (*score) >= min_score);
        }

        let total = scored.len();
        let limited_len = total.min(args.max_results);
        let truncated = total > args.max_results;
        scored.truncate(limited_len);

        let (page_slice, next_token) = paginate(&scored, &args.pagination);

        // Build search hits using NodeId + graph lookups
        let hits = build_search_hits_from_nodes(
            &snapshot,
            page_slice,
            args.context_lines,
            workspace_root,
        )?;
        let truncated_flag = truncated || next_token.is_some();

        Ok(ToolExecution {
            data: SemanticSearchData {
                results: hits,
                total: total as u64,
                truncated: truncated_flag,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: elapsed,
            next_page_token: next_token,
            total: Some(total as u64),
            truncated: Some(truncated_flag),
            candidates_scanned: Some(nodes_searched as u64),
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `structural_similar` (U07): acquires the
    /// snapshot from the workspace context and delegates to the shared core, so
    /// the daemon and standalone transports are behaviourally identical.
    ///
    /// # Errors
    /// Returns an error if a supplied `file_path` escapes the workspace or the
    /// probe symbol has no body-shape descriptor.
    pub(crate) fn execute_structural_similar(
        ctx: &WorkspaceContext,
        args: &super::StructuralSimilarArgs,
        start: Instant,
    ) -> Result<ToolExecution<super::StructuralSimilarData>> {
        let snapshot = Arc::new(ctx.graph.snapshot());
        super::structural_similar_from_snapshot(&snapshot, &ctx.workspace_root, args, start)
    }
}

/// Find the reference node for similarity search.
///
/// Returns (`node_id`, `node_entry`) or an error if not found.
fn find_reference_node(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol_name: &str,
    file_path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Result<(
    sqry_core::graph::unified::NodeId,
    sqry_core::graph::unified::NodeEntry,
)> {
    let relative_file = file_path.strip_prefix(workspace_root).unwrap_or(file_path);

    let reference_node_id = match snapshot.resolve_symbol(&SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(relative_file),
        mode: ResolutionMode::Strict,
    }) {
        SymbolResolutionOutcome::Resolved(node_id) => node_id,
        SymbolResolutionOutcome::NotFound | SymbolResolutionOutcome::FileNotIndexed => {
            return Err(anyhow!(
                "Symbol '{}' not found in {}",
                symbol_name,
                file_path.display()
            ));
        }
        SymbolResolutionOutcome::Ambiguous(candidates) => {
            return Err(anyhow!(
                "Symbol '{}' is ambiguous in {} ({} candidates)",
                symbol_name,
                file_path.display(),
                candidates.len()
            ));
        }
    };

    let reference_entry = snapshot
        .get_node(reference_node_id)
        .ok_or_else(|| anyhow!("Reference symbol entry not found"))?;

    Ok((reference_node_id, reference_entry.clone()))
}

/// Execute the `find_similar` tool to find symbols similar to a reference.
///
/// Uses `CodeGraph` to find symbols with similar names using fuzzy matching.
///
/// # Errors
///
/// Returns an error if workspace resolution, graph acquisition, file
/// canonicalization, or reference-symbol resolution fails.
#[allow(clippy::too_many_lines)]
pub fn execute_find_similar(args: &SearchSimilarArgs) -> Result<ToolExecution<FindSimilarData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _scope_root = canonicalize_in_workspace(&args.path, &workspace_root)?;
    let file_path = canonicalize_in_workspace(&args.file_path, &workspace_root)?;

    tracing::debug!(
        file_path = %args.file_path,
        symbol = %args.symbol_name,
        similarity_threshold = args.similarity_threshold,
        max_results = args.max_results,
        "Executing find_similar tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Find the reference symbol using name lookup
    let (reference_node_id, reference_entry) =
        find_reference_node(&snapshot, &args.symbol_name, &file_path, &workspace_root)?;

    // Build reference symbol data
    let strings = snapshot.strings();
    let files = snapshot.files();

    let ref_name = strings
        .resolve(reference_entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let ref_qualified_name = reference_entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| ref_name.clone(), |s| s.to_string());
    let ref_kind = reference_entry.kind;
    let ref_language = files
        .language_for_file(reference_entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    let reference_ref = build_node_ref_from_node(
        &reference_entry,
        reference_node_id,
        &snapshot,
        &workspace_root,
    );

    // Use FuzzyMatcher for similarity
    let matcher = FuzzyMatcher::with_config(MatchConfig {
        min_score: args.similarity_threshold,
        ..MatchConfig::default()
    });

    let mut candidates: Vec<(SimilarSymbolData, f64)> = Vec::new();
    let mut candidates_scanned: u64 = 0;
    let mut seen_names: HashSet<String> = HashSet::new();
    seen_names.insert(ref_qualified_name.clone());

    // Iterate through all nodes and find similar ones
    for (node_id, entry) in snapshot.iter_nodes() {
        // Gate 0d iter-2 fix: skip unified losers from
        // `search_similar`. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        candidates_scanned += 1;

        // Skip the reference symbol itself
        if node_id == reference_node_id {
            continue;
        }

        // Only compare same kind
        if entry.kind != ref_kind {
            continue;
        }

        // Get candidate name
        let candidate_name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let candidate_canonical_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| candidate_name.clone(), |s| s.to_string());

        // Skip already seen
        if seen_names.contains(&candidate_canonical_name) {
            continue;
        }
        seen_names.insert(candidate_canonical_name);

        // Calculate similarity using FuzzyMatcher on names
        let score = matcher.score(&ref_name, &candidate_name);
        if score < args.similarity_threshold {
            continue;
        }

        // Get candidate language
        let candidate_language_enum = files.language_for_file(entry.file);
        let candidate_language =
            candidate_language_enum.map_or_else(|| "unknown".to_string(), |l| l.to_string());

        // Only same language (unless cross-language is desired)
        if candidate_language != ref_language {
            continue;
        }

        // Build candidate file path and URI
        let candidate_file = files
            .resolve(entry.file)
            .map(|p| workspace_root.join(p.as_ref()))
            .unwrap_or_default();
        let file_uri = url::Url::from_file_path(&candidate_file).ok().map_or_else(
            || crate::execution::symbol_utils::path_to_forward_slash(&candidate_file),
            std::convert::Into::into,
        );
        let candidate_display_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry,
            strings,
            files,
            &candidate_name,
        );

        // Build NodeRefData for the candidate
        let loc = node_location_for_reporting_snapshot(&snapshot, node_id, &workspace_root);
        let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));
        let candidate_ref = crate::execution::types::NodeRefData {
            name: candidate_name,
            qualified_name: candidate_display_name,
            kind: format!("{:?}", entry.kind),
            language: candidate_language,
            file_uri,
            range: crate::execution::types::RangeData {
                start: crate::execution::types::PositionData {
                    line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                    character: loc.as_ref().map_or(entry.start_column, |l| l.column),
                },
                end: crate::execution::types::PositionData {
                    line: loc.as_ref().map_or(entry.end_line, |l| l.end_line),
                    character: loc.as_ref().map_or(entry.end_column, |l| l.end_column),
                },
            },
            metadata: None,
            resolution_source,
        };

        let similar_data = SimilarSymbolData {
            symbol: candidate_ref,
            similarity: score,
        };

        candidates.push((similar_data, score));

        // Early exit if we have enough
        if candidates.len() >= args.max_results * 2 {
            break;
        }
    }

    // Sort by similarity descending
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let total = candidates.len();
    let truncated = total > args.max_results;
    candidates.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&candidates, &args.pagination);
    let results: Vec<SimilarSymbolData> = page_slice.iter().map(|(data, _)| data.clone()).collect();
    let truncated_flag = truncated || next_page_token.is_some();

    tracing::debug!(
        candidates_scanned = candidates_scanned,
        total_candidates = total,
        returned = results.len(),
        truncated = truncated_flag,
        "find_similar candidate summary"
    );

    Ok(ToolExecution {
        data: FindSimilarData {
            reference: reference_ref,
            results,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated_flag),
        candidates_scanned: Some(candidates_scanned),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `structural_similar` tool (body-shape descriptor, U07).
///
/// Resolves the probe to a Function/Method carrying a shape descriptor, then
/// routes through the sqry-db `StructuralNeighborsQuery` LSH index (NOT a
/// hand-rolled traversal) via the shared `structural_neighbors` helper. Each
/// match reports the AC-4 two numbers: exact `shape_hash` identity + MinHash
/// Jaccard. Distinct from the name-based `search_similar`.
///
/// # Errors
/// Returns an error if the workspace/graph cannot be resolved or the probe
/// symbol has no body-shape descriptor.
pub fn execute_structural_similar(
    args: &StructuralSimilarArgs,
) -> Result<ToolExecution<StructuralSimilarData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let graph = engine.ensure_graph()?;
    let snapshot = std::sync::Arc::new(graph.snapshot());
    structural_similar_from_snapshot(&snapshot, &workspace_root, args, start)
}

/// Shared core for `structural_similar`, reached by BOTH the standalone rmcp
/// path ([`execute_structural_similar`]) and the daemon-hosted path
/// (`daemon_adapter::execute_structural_similar_for_daemon`). Operating on an
/// already-acquired snapshot keeps the two transports behaviourally identical.
///
/// # Errors
/// Returns an error if a supplied `file_path` escapes the workspace or the probe
/// symbol has no body-shape descriptor.
pub(crate) fn structural_similar_from_snapshot(
    snapshot: &std::sync::Arc<sqry_core::graph::unified::concurrent::GraphSnapshot>,
    workspace_root: &Path,
    args: &StructuralSimilarArgs,
    start: Instant,
) -> Result<ToolExecution<StructuralSimilarData>> {
    let want_file = args
        .file_path
        .as_ref()
        .map(|f| canonicalize_in_workspace(f, workspace_root))
        .transpose()?;

    // Resolve the probe: first Function/Method carrying a descriptor that matches
    // `symbol_name` (simple or qualified), optionally constrained to `file_path`.
    let probe_id = {
        let strings = snapshot.strings();
        let files = snapshot.files();
        let descriptors = snapshot.macro_metadata().shape_descriptors();
        snapshot
            .iter_nodes()
            .find(|(node_id, entry)| {
                if entry.is_unified_loser() || !descriptors.contains_key(node_id) {
                    return false;
                }
                if let Some(wf) = &want_file {
                    let matches = files
                        .resolve(entry.file)
                        .map(|p| workspace_root.join(p.as_ref()))
                        .is_some_and(|p| &p == wf);
                    if !matches {
                        return false;
                    }
                }
                let n = strings.resolve(entry.name);
                let q = entry.qualified_name.and_then(|id| strings.resolve(id));
                n.is_some_and(|n| n.as_ref() == args.symbol_name)
                    || q.is_some_and(|q| q.as_ref() == args.symbol_name)
            })
            .map(|(id, _)| id)
    };

    let Some(probe_id) = probe_id else {
        return Err(anyhow!(
            "No function/method named '{}' with a body-shape descriptor was found{}",
            args.symbol_name,
            args.file_path
                .as_ref()
                .map_or_else(String::new, |f| format!(" in '{f}'"))
        ));
    };

    let reference_ref = {
        let entry = snapshot
            .nodes()
            .get(probe_id)
            .context("probe node vanished from snapshot")?;
        build_node_ref_from_node(entry, probe_id, snapshot.as_ref(), workspace_root)
    };

    let db = sqry_db::queries::dispatch::make_query_db(std::sync::Arc::clone(snapshot));
    #[allow(clippy::cast_possible_truncation)]
    let floor = args.similarity_threshold as f32;
    let matches = sqry_db::queries::structural_neighbors(
        &db,
        snapshot.as_ref(),
        probe_id,
        floor,
        args.max_results,
    );

    let results: Vec<StructuralNeighborData> = matches
        .into_iter()
        .filter_map(|m| {
            let entry = snapshot.nodes().get(m.node)?;
            let symbol = build_node_ref_from_node(entry, m.node, snapshot.as_ref(), workspace_root);
            Some(StructuralNeighborData {
                symbol,
                shape_hash_exact: m.shape_hash_exact,
                jaccard: f64::from(m.jaccard),
            })
        })
        .collect();

    let total = results.len() as u64;
    Ok(ToolExecution {
        data: StructuralSimilarData {
            reference: reference_ref,
            results,
            total,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Build `NodeRefData` from a graph node entry.
fn build_node_ref_from_node(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    node_id: NodeId,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &Path,
) -> crate::execution::types::NodeRefData {
    use crate::execution::types::{NodeRefData, PositionData, RangeData};

    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);

    let file_path = files
        .resolve(entry.file)
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        std::convert::Into::into,
    );

    let language = files
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    let loc = node_location_for_reporting_snapshot(snapshot, node_id, workspace_root);
    let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));

    NodeRefData {
        name,
        qualified_name,
        kind: format!("{:?}", entry.kind),
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                character: loc.as_ref().map_or(entry.start_column, |l| l.column),
            },
            end: PositionData {
                line: loc.as_ref().map_or(entry.end_line, |l| l.end_line),
                character: loc.as_ref().map_or(entry.end_column, |l| l.end_column),
            },
        },
        metadata: None,
        resolution_source,
    }
}

#[cfg(test)]
mod tests {
    use super::SemanticSortKey;

    #[test]
    fn semantic_sort_key_orders_by_display_name_then_location() {
        let mut keys = [
            SemanticSortKey {
                display_name: "beta.run".to_string(),
                relative_path: "src/lib.rs".to_string(),
                start_line: 10,
                start_column: 0,
                end_line: 12,
                end_column: 1,
            },
            SemanticSortKey {
                display_name: "alpha.run".to_string(),
                relative_path: "src/z.rs".to_string(),
                start_line: 8,
                start_column: 0,
                end_line: 9,
                end_column: 1,
            },
            SemanticSortKey {
                display_name: "alpha.run".to_string(),
                relative_path: "src/a.rs".to_string(),
                start_line: 4,
                start_column: 0,
                end_line: 6,
                end_column: 1,
            },
        ];

        keys.sort();

        assert_eq!(keys[0].relative_path, "src/a.rs");
        assert_eq!(keys[1].relative_path, "src/z.rs");
        assert_eq!(keys[2].display_name, "beta.run");
    }
}
