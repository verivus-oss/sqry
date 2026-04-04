//! Analysis tool execution.
//!
//! This module implements analysis tools: `cross_language_edges`, `dependency_impact`,
//! `semantic_diff`.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use sqry_core::graph::unified::{
    FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery, SymbolResolutionOutcome,
};

use crate::engine::{Engine, canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{CrossLanguageEdgesArgs, DependencyImpactArgs, SemanticDiffArgs};

use crate::execution::diff_comparator;
use crate::execution::git_worktree;
use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::types::{
    CrossLanguageEdgesData, DependencyImpactData, FindUnusedData, ImpactedSymbol, NodeRefData,
    PositionData, RangeData, RelationEdgeData, SemanticDiffData, ToolExecution, UnusedSymbolData,
};
use crate::execution::utils::{duration_to_ms, paginate};

fn resolve_workspace_path(path: &str) -> Option<std::path::PathBuf> {
    if path == "." {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

fn resolve_global_symbol_strict(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Result<sqry_core::graph::unified::node::NodeId> {
    match snapshot.resolve_symbol(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::Strict,
    }) {
        SymbolResolutionOutcome::Resolved(node_id) => Ok(node_id),
        SymbolResolutionOutcome::NotFound | SymbolResolutionOutcome::FileNotIndexed => {
            Err(anyhow!("Symbol '{symbol}' not found in graph."))
        }
        SymbolResolutionOutcome::Ambiguous(candidates) => Err(anyhow!(
            "Symbol '{symbol}' is ambiguous in graph ({} candidates). Use a canonical qualified name.",
            candidates.len()
        )),
    }
}

fn candidate_bucket_for_symbol(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Vec<sqry_core::graph::unified::node::NodeId> {
    match snapshot.find_symbol_candidates(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    }) {
        SymbolCandidateOutcome::Candidates(candidates) => candidates,
        SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
    }
}

/// Extract cycles from precomputed SCC data
///
/// Converts non-trivial SCCs to the same format as `find_all_cycles_graph`
fn extract_cycles_from_scc(
    graph: &sqry_core::graph::unified::CodeGraph,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    config: &sqry_core::query::CircularConfig,
) -> Vec<Vec<String>> {
    let snapshot = graph.snapshot();
    let strings = snapshot.strings();

    let mut index_to_node_id = vec![None; scc_data.node_count as usize];
    for (node_id, _entry) in snapshot.nodes().iter() {
        let idx = node_id.index() as usize;
        if idx < index_to_node_id.len() {
            index_to_node_id[idx] = Some(node_id);
        }
    }

    let mut cycles = Vec::new();

    // Iterate through all SCCs, using precomputed self-loop data from SccData.
    // This avoids the expensive O(V * edges_per_node) full-edge iteration that
    // was causing timeouts on large graphs (238K+ nodes).
    for scc_id in 0..scc_data.scc_count {
        let members = scc_data.scc_members(scc_id);
        let size = members.len();

        let is_self_loop = size == 1
            && scc_data
                .has_self_loop
                .get(scc_id as usize)
                .copied()
                .unwrap_or(false);
        if !should_include_scc(size, is_self_loop, config) {
            continue;
        }

        if cycles.len() >= config.max_results {
            break;
        }

        // Convert node IDs to qualified names
        let cycle: Vec<String> = members
            .iter()
            .filter_map(|&node_idx| {
                let node_id = index_to_node_id.get(node_idx as usize).and_then(|id| *id)?;
                snapshot.get_node(node_id).and_then(|entry| {
                    entry
                        .qualified_name
                        .and_then(|sid| strings.resolve(sid))
                        .or_else(|| strings.resolve(entry.name))
                        .map(|s| s.to_string())
                })
            })
            .collect();

        if !cycle.is_empty() {
            cycles.push(cycle);
        }
    }

    cycles
}

fn cycle_edge_kind_name(cycle_type: CycleType) -> &'static str {
    match cycle_type {
        CycleType::Calls => "calls",
        CycleType::Imports | CycleType::Modules => "imports",
    }
}

fn cycle_edge_kind(cycle_type: CycleType) -> sqry_core::graph::unified::edge::EdgeKind {
    match cycle_type {
        CycleType::Calls => sqry_core::graph::unified::edge::EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        },
        CycleType::Imports | CycleType::Modules => {
            sqry_core::graph::unified::edge::EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            }
        }
    }
}

fn cycle_type_label(cycle_type: CycleType) -> &'static str {
    match cycle_type {
        CycleType::Calls => "calls",
        CycleType::Imports => "imports",
        CycleType::Modules => "modules",
    }
}

fn compute_cycles_from_graph(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    cycle_type: CycleType,
    core_cycle_type: CircularType,
    config: &CircularConfig,
) -> Result<Vec<Vec<String>>> {
    use sqry_core::graph::unified::analysis::csr::CsrAdjacency;
    use sqry_core::graph::unified::compaction::build::snapshot_edges;

    let edge_kind = cycle_edge_kind(cycle_type);
    let node_count = snapshot.nodes().len();

    let csr_result = {
        let forward_store = graph.edges().forward();
        let compaction_snapshot = snapshot_edges(&forward_store, node_count);
        drop(forward_store);
        CsrAdjacency::build_from_snapshot(&compaction_snapshot)
    };

    match csr_result {
        Ok(csr) => match sqry_core::graph::unified::analysis::scc::SccData::compute_tarjan(
            &csr, &edge_kind,
        ) {
            Ok(scc_data) => Ok(extract_cycles_from_scc(graph, &scc_data, config)),
            Err(error) => handle_cycle_analysis_failure(
                "SCC computation",
                error,
                node_count,
                core_cycle_type,
                graph,
                config,
            ),
        },
        Err(error) => handle_cycle_analysis_failure(
            "CSR build",
            error,
            node_count,
            core_cycle_type,
            graph,
            config,
        ),
    }
}

fn handle_cycle_analysis_failure<E: std::fmt::Display>(
    stage: &str,
    error: E,
    node_count: usize,
    core_cycle_type: CircularType,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    config: &CircularConfig,
) -> Result<Vec<Vec<String>>> {
    tracing::warn!("{stage} failed: {error}. Falling back to graph traversal.");
    if node_count > 100_000 {
        return Err(anyhow!(
            "Graph has {node_count} nodes and {stage} failed: {error}. \
             Run `sqry index --force` or `sqry analyze` to rebuild cycle data.",
        ));
    }
    Ok(find_all_cycles_graph(core_cycle_type, graph, config))
}

fn should_include_scc(
    size: usize,
    is_self_loop: bool,
    config: &sqry_core::query::CircularConfig,
) -> bool {
    if is_self_loop {
        return config.should_include_self_loops;
    }

    if size == 1 {
        return false;
    }

    if size < config.min_depth {
        return false;
    }

    if config.max_depth.is_some_and(|max| size > max) {
        return false;
    }

    true
}

/// Fast O(1) check if node is in a cycle using precomputed SCC data
fn is_node_in_cycle_precomputed(
    node_id: sqry_core::graph::unified::node::NodeId,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    config: &sqry_core::query::CircularConfig,
) -> bool {
    // Get the SCC ID for this node
    let Some(scc_id) = scc_data.scc_of(node_id) else {
        return false;
    };

    let members = scc_data.scc_members(scc_id);
    let size = members.len();
    let is_self_loop = size == 1 && scc_data.has_self_loop[scc_id as usize];

    should_include_scc(size, is_self_loop, config)
}

/// Check whether a node should be included in unused results based on filters.
///
/// Returns `true` if the node passes all scope, language, and kind filters.
fn should_include_in_unused_results(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    args: &FindUnusedArgs,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
) -> bool {
    // Apply scope filter
    let visibility_str = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .map(|s| s.to_string());
    if !matches_scope_filter(entry.kind, visibility_str.as_deref(), args.scope) {
        return false;
    }

    // Apply language filter
    if !args.languages.is_empty() {
        if let Some(lang) = files.language_for_file(entry.file) {
            let lang_str = lang.to_string();
            if !args
                .languages
                .iter()
                .any(|l| l.eq_ignore_ascii_case(&lang_str))
            {
                return false;
            }
        } else {
            return false;
        }
    }

    // Apply kind filter
    if !args.kinds.is_empty() {
        let kind_str = format!("{:?}", entry.kind).to_lowercase();
        if !args.kinds.iter().any(|k| k.eq_ignore_ascii_case(&kind_str)) {
            return false;
        }
    }

    true
}

/// Build an `UnusedSymbolData` from a graph node entry.
fn build_unused_symbol_data(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
) -> UnusedSymbolData {
    let name = strings
        .resolve(entry.name)
        .map_or_else(String::new, |s| s.to_string());
    let language = files.language_for_file(entry.file);
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry, strings, language, &name,
    );

    let file_path = files.resolve(entry.file);
    let full_path = file_path
        .as_ref()
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
        Into::into,
    );

    let language = language.map_or("unknown".to_string(), |l| l.to_string());

    let kind = format!("{:?}", entry.kind).to_lowercase();
    let visibility = format!("{:?}", entry.visibility).to_lowercase();

    UnusedSymbolData {
        name,
        qualified_name,
        kind,
        file_uri,
        line: entry.start_line,
        language,
        visibility,
    }
}

/// Mark all nodes reachable from entry points using SCC data and condensation DAG.
fn mark_reachable_from_entries(
    entry_points: &[sqry_core::graph::unified::node::NodeId],
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    cond_dag: &sqry_core::graph::unified::analysis::CondensationDag,
) -> HashSet<sqry_core::graph::unified::node::NodeId> {
    use sqry_core::graph::unified::node::NodeId;

    // Step 1: Collect entry SCCs (deduplicated)
    let mut entry_sccs = HashSet::new();
    for entry_node in entry_points {
        if let Some(scc_id) = scc_data.scc_of(*entry_node) {
            entry_sccs.insert(scc_id);
        }
    }

    // Step 2: BFS on the condensation DAG to find all reachable SCCs.
    // This is O(S + E_cond) instead of O(entry_count * S * label_check).
    let mut reachable_sccs = HashSet::new();
    let mut queue = VecDeque::new();

    for &scc_id in &entry_sccs {
        if reachable_sccs.insert(scc_id) {
            queue.push_back(scc_id);
        }
    }

    while let Some(current_scc) = queue.pop_front() {
        for &successor in cond_dag.successors(current_scc) {
            if reachable_sccs.insert(successor) {
                queue.push_back(successor);
            }
        }
    }

    // Step 3: Expand reachable SCCs to reachable nodes
    let mut reachable = HashSet::with_capacity(entry_points.len() * 2);
    for &scc_id in &reachable_sccs {
        for &node_idx in scc_data.scc_members(scc_id) {
            reachable.insert(NodeId::new(node_idx, 0));
        }
    }

    reachable
}

/// Find unused code using Pass 5 reachability analysis
fn find_unused_with_reachability(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    cond_dag: &sqry_core::graph::unified::analysis::CondensationDag,
    args: &FindUnusedArgs,
    workspace_root: &std::path::Path,
) -> Vec<UnusedSymbolData> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Step 1: Identify entry points
    let entry_points = identify_entry_points(snapshot, args);

    if entry_points.is_empty() {
        tracing::warn!("No entry points identified for unused code detection");
        return Vec::new();
    }

    tracing::debug!(
        "Found {} entry points for reachability analysis",
        entry_points.len()
    );

    // Step 2: Mark reachable nodes using 2-hop labels
    let reachable = mark_reachable_from_entries(&entry_points, scc_data, cond_dag);

    tracing::debug!("Marked {} nodes as reachable", reachable.len());

    // Step 3: Collect unreachable nodes that match filters
    let mut unused = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if reachable.contains(&node_id) {
            continue;
        }

        if !should_include_in_unused_results(entry, args, strings, files) {
            continue;
        }

        unused.push(build_unused_symbol_data(
            entry,
            strings,
            files,
            workspace_root,
        ));

        if unused.len() >= args.max_results {
            break;
        }
    }

    unused
}

/// Identify entry points for reachability analysis
fn identify_entry_points(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    _args: &FindUnusedArgs,
) -> Vec<sqry_core::graph::unified::node::NodeId> {
    let files = snapshot.files();
    let mut entry_points = Vec::new();

    for (node_id, entry) in snapshot.nodes().iter() {
        // Language-specific entry point heuristics
        let is_entry = if let Some(lang) = files.language_for_file(entry.file) {
            match lang {
                sqry_core::graph::Language::Rust => {
                    is_rust_entry_point(node_id, entry, files, snapshot)
                }
                sqry_core::graph::Language::Python => {
                    is_python_entry_point(node_id, entry, files, snapshot)
                }
                sqry_core::graph::Language::JavaScript | sqry_core::graph::Language::TypeScript => {
                    is_js_ts_entry_point(node_id, entry, snapshot)
                }
                sqry_core::graph::Language::Java => is_java_entry_point(node_id, entry, snapshot),
                sqry_core::graph::Language::Go => is_go_entry_point(node_id, entry, snapshot),
                sqry_core::graph::Language::C | sqry_core::graph::Language::Cpp => {
                    is_c_cpp_entry_point(node_id, entry, snapshot)
                }
                _ => {
                    // For other languages, use generic heuristic: public exports or main
                    is_generic_entry_point(node_id, entry, snapshot)
                }
            }
        } else {
            false
        };

        if is_entry {
            entry_points.push(node_id);
        }
    }

    entry_points
}

/// Check if a symbol matches the scope filter
fn matches_scope_filter(
    kind: sqry_core::graph::unified::node::NodeKind,
    visibility_str: Option<&str>,
    scope: UnusedScope,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    match scope {
        UnusedScope::Public => visibility_str.is_some_and(|v| v.eq_ignore_ascii_case("public")),
        UnusedScope::Private => visibility_str.is_some_and(|v| v.eq_ignore_ascii_case("private")),
        UnusedScope::Function => matches!(kind, NodeKind::Function | NodeKind::Method),
        UnusedScope::Struct => matches!(
            kind,
            NodeKind::Struct | NodeKind::Class | NodeKind::Interface | NodeKind::Trait
        ),
        UnusedScope::All => true,
    }
}

// Language-specific entry point detection

fn is_rust_entry_point(
    _node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();

    // main() function
    if entry.kind == NodeKind::Function
        && let Some(name) = strings.resolve(entry.name)
        && name.as_ref() == "main"
    {
        return true;
    }

    // Public items in lib.rs or main.rs
    let is_public = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .is_some_and(|v| v.as_ref().eq_ignore_ascii_case("public"));

    if is_public && let Some(file_path) = files.resolve(entry.file) {
        let path_str = file_path.to_string_lossy();
        if path_str.ends_with("lib.rs") || path_str.ends_with("main.rs") {
            return true;
        }
    }

    // Test functions (#[test] - detected as Function with specific markers)
    // Note: This requires attributes to be stored in the graph, which may not be available
    // For now, we'll consider all pub fn as potential entry points

    // Exported items (pub use, pub fn, pub struct, etc.)
    is_public && !matches!(entry.kind, NodeKind::Variable | NodeKind::Constant)
}

fn is_python_entry_point(
    _node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    _snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    // Functions/classes in __init__.py
    if let Some(file_path) = files.resolve(entry.file) {
        let path_str = file_path.to_string_lossy();
        if path_str.ends_with("__init__.py") {
            return matches!(entry.kind, NodeKind::Function | NodeKind::Class);
        }
    }

    // if __name__ == "__main__" (heuristic: top-level functions)
    if entry.kind == NodeKind::Function {
        return true;
    }

    // Classes are often entry points
    entry.kind == NodeKind::Class
}

fn is_js_ts_entry_point(
    node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    // Check for export edges from this node
    for edge in snapshot.edges().edges_from(node_id) {
        if matches!(
            edge.kind,
            sqry_core::graph::unified::edge::EdgeKind::Exports { .. }
        ) {
            return true;
        }
    }

    // Default export (heuristic: top-level functions/classes)
    matches!(
        entry.kind,
        NodeKind::Function | NodeKind::Class | NodeKind::Component
    )
}

fn is_java_entry_point(
    _node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();
    let is_public = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .is_some_and(|v| v.as_ref().eq_ignore_ascii_case("public"));

    // public static void main(String[] args)
    if entry.kind == NodeKind::Method
        && is_public
        && let Some(name) = strings.resolve(entry.name)
        && name.as_ref() == "main"
    {
        return true;
    }

    // Public classes
    entry.kind == NodeKind::Class && is_public
}

fn is_go_entry_point(
    _node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();

    // func main()
    if entry.kind == NodeKind::Function
        && let Some(name) = strings.resolve(entry.name)
        && name.as_ref() == "main"
    {
        return true;
    }

    // Exported symbols (capitalized names)
    let is_public = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .is_some_and(|v| v.as_ref().eq_ignore_ascii_case("public"));

    if is_public && let Some(name) = strings.resolve(entry.name) {
        let first_char = name.chars().next();
        if first_char.is_some_and(char::is_uppercase) {
            return true;
        }
    }

    false
}

fn is_c_cpp_entry_point(
    _node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();

    // main() function
    if entry.kind == NodeKind::Function
        && let Some(name) = strings.resolve(entry.name)
        && name.as_ref() == "main"
    {
        return true;
    }

    // Functions without static modifier (public functions)
    // Note: Static detection requires parsing attributes, may not be available
    // Heuristic: all non-private functions are entry points
    let is_private = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .is_some_and(|v| v.as_ref().eq_ignore_ascii_case("private"));

    entry.kind == NodeKind::Function && !is_private
}

fn is_generic_entry_point(
    node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();

    // Generic heuristic: public exports or main
    if entry.kind == NodeKind::Function
        && let Some(name) = strings.resolve(entry.name)
        && name.as_ref() == "main"
    {
        return true;
    }

    // Check for export edges from this node
    for edge in snapshot.edges().edges_from(node_id) {
        if matches!(
            edge.kind,
            sqry_core::graph::unified::edge::EdgeKind::Exports { .. }
        ) {
            return true;
        }
    }

    // Public visibility
    entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .is_some_and(|v| v.as_ref().eq_ignore_ascii_case("public"))
}

/// Execute the `cross_language_edges` tool to find cross-language dependencies.
///
/// Uses the unified graph (`GraphSnapshot`) to find edges where source and target
/// nodes belong to different programming languages.
pub fn execute_cross_language_edges(
    args: &CrossLanguageEdgesArgs,
) -> Result<ToolExecution<CrossLanguageEdgesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph for cross-language edge detection
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let files = snapshot.files();
    let strings = snapshot.strings();

    let mut edges: Vec<RelationEdgeData> = Vec::new();

    // Iterate over all edges and find cross-language ones
    for (source_id, target_id, edge_kind) in snapshot.iter_edges() {
        let Some(source_node) = snapshot.get_node(source_id) else {
            continue;
        };
        let Some(target_node) = snapshot.get_node(target_id) else {
            continue;
        };

        // Get languages from file registry
        let source_lang = files.language_for_file(source_node.file);
        let target_lang = files.language_for_file(target_node.file);

        // Skip if same language or unknown languages
        let (Some(from_lang), Some(to_lang)) = (source_lang, target_lang) else {
            continue;
        };
        if from_lang == to_lang {
            continue;
        }

        // Apply language filters
        if let Some(ref fl) = args.from_lang
            && !from_lang.to_string().eq_ignore_ascii_case(fl)
        {
            continue;
        }
        if let Some(ref tl) = args.to_lang
            && !to_lang.to_string().eq_ignore_ascii_case(tl)
        {
            continue;
        }

        // Build NodeRefData for source node
        let from_ref =
            build_node_ref_from_node(source_node, from_lang, files, strings, &workspace_root);

        // Build NodeRefData for target node
        let to_ref =
            build_node_ref_from_node(target_node, to_lang, files, strings, &workspace_root);

        edges.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: format!("{edge_kind:?}").to_lowercase(),
            depth: 1,
            metadata: None,
        });

        if edges.len() >= args.max_results {
            break;
        }
    }

    let total = edges.len();
    let (page_slice, next_page_token) = paginate(&edges, &args.pagination);
    let page_edges = page_slice.to_vec();

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: CrossLanguageEdgesData {
            edges: page_edges,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Builds a `NodeRefData` from a unified graph `NodeEntry`.
fn build_node_ref_from_node(
    node: &sqry_core::graph::unified::storage::arena::NodeEntry,
    language: sqry_core::graph::Language,
    files: &sqry_core::graph::unified::storage::FileRegistry,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    workspace_root: &std::path::Path,
) -> NodeRefData {
    use sqry_core::graph::unified::node::NodeKind;

    // Get symbol name from string interner
    let name = strings
        .resolve(node.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Get qualified name if present
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        node,
        strings,
        Some(language),
        &name,
    );

    // Map NodeKind to kind string
    let kind = match node.kind {
        NodeKind::Class => "class",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Method => "method",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Type => "type",
        _ => "function",
    };

    // Get file path using resolve() which returns Option<Arc<Path>>
    let file_path = files
        .resolve(node.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language: language.to_string(),
        file_uri,
        range: RangeData {
            start: PositionData {
                line: node.start_line,
                character: node.start_column,
            },
            end: PositionData {
                line: node.end_line,
                character: node.end_column,
            },
        },
        metadata: None,
    }
}

/// Build a `NodeRefData` for an impact analysis result from a graph node entry.
fn build_impact_node_ref(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
) -> NodeRefData {
    let name = strings
        .resolve(entry.name)
        .map_or_else(String::new, |s| s.to_string());
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry,
        strings,
        files.language_for_file(entry.file),
        &name,
    );

    let file_path = files.resolve(entry.file);
    let full_path = file_path
        .as_ref()
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
        Into::into,
    );

    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |l| l.to_string());

    let kind = format!("{:?}", entry.kind).to_lowercase();

    NodeRefData {
        name,
        qualified_name,
        kind,
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: entry.start_line,
                character: entry.start_column,
            },
            end: PositionData {
                line: entry.end_line,
                character: entry.end_column,
            },
        },
        metadata: None,
    }
}

/// Process a caller node and return impacted symbol data and file URI.
fn process_caller_node(
    entry: &sqry_core::graph::unified::NodeEntry,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
    depth: usize,
    _args: &DependencyImpactArgs,
) -> (ImpactedSymbol, String) {
    let node_ref = build_impact_node_ref(entry, strings, files, workspace_root);
    let file_uri = node_ref.file_uri.clone();

    let symbol = ImpactedSymbol {
        symbol: node_ref,
        depth: u32::try_from(depth + 1).unwrap_or(u32::MAX),
        impact_type: "caller".to_string(),
    };

    (symbol, file_uri)
}

/// BFS traversal to collect all impacted callers of a target symbol.
///
/// Returns the list of impacted symbols and set of affected file URIs.
fn collect_impacted_callers_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_node_id: sqry_core::graph::unified::node::NodeId,
    args: &DependencyImpactArgs,
    workspace_root: &std::path::Path,
) -> (Vec<ImpactedSymbol>, HashSet<String>) {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let mut impacted: Vec<ImpactedSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(sqry_core::graph::unified::node::NodeId, usize)> = VecDeque::new();
    let mut visited: HashSet<sqry_core::graph::unified::node::NodeId> = HashSet::new();

    queue.push_back((target_node_id, 0));
    visited.insert(target_node_id);

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= args.max_depth {
            continue;
        }

        let callers = snapshot.get_callers(current_id);
        for caller_id in callers {
            if visited.contains(&caller_id) {
                continue;
            }

            let Some(entry) = snapshot.get_node(caller_id) else {
                continue;
            };
            let (symbol, file_uri) =
                process_caller_node(entry, strings, files, workspace_root, depth, args);

            if args.include_files {
                affected_files.insert(file_uri);
            }

            impacted.push(symbol);

            if args.include_indirect {
                visited.insert(caller_id);
                queue.push_back((caller_id, depth + 1));
            }
        }
    }

    (impacted, affected_files)
}

/// Execute the `dependency_impact` tool to analyze impact of changes.
///
/// Uses the unified `CodeGraph` for dependency traversal via BFS on call/import edges.
pub fn execute_dependency_impact(
    args: &DependencyImpactArgs,
) -> Result<ToolExecution<DependencyImpactData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    tracing::debug!(
        symbol = %args.symbol,
        max_depth = args.max_depth,
        include_files = args.include_files,
        include_indirect = args.include_indirect,
        "Executing dependency_impact tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Find the target symbol
    let target_node_id = resolve_global_symbol_strict(&snapshot, &args.symbol)?;

    let (mut impacted, affected_files) =
        collect_impacted_callers_bfs(&snapshot, target_node_id, args, &workspace_root);

    let total = impacted.len();
    let truncated = total > args.max_results;
    impacted.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&impacted, &args.pagination);
    let page_impacted: Vec<ImpactedSymbol> = page_slice.to_vec();

    let affected_files_vec = if args.include_files {
        let mut files_list: Vec<String> = affected_files.into_iter().collect();
        files_list.sort();
        Some(files_list)
    } else {
        None
    };

    let truncated_flag = truncated || next_page_token.is_some();

    Ok(ToolExecution {
        data: DependencyImpactData {
            target_symbol: args.symbol.clone(),
            impacted_symbols: page_impacted,
            affected_files: affected_files_vec,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated_flag),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `semantic_diff` tool to compare code between git refs.
///
/// Uses `CodeGraph` for semantic comparison between two git refs.
pub fn execute_semantic_diff(args: &SemanticDiffArgs) -> Result<ToolExecution<SemanticDiffData>> {
    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};

    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(
        base_ref = %args.base.git_ref,
        target_ref = %args.target.git_ref,
        include_unchanged = args.include_unchanged,
        max_results = args.max_results,
        "Executing semantic_diff tool"
    );

    // Phase 1: Create git worktrees
    let worktree_mgr = git_worktree::WorktreeManager::create(
        &workspace_root,
        &args.base.git_ref,
        &args.target.git_ref,
    )?;

    // Phase 2: Build CodeGraphs for both worktrees
    let plugins = Engine::plugin_manager();
    let config = BuildConfig::default();

    let base_graph = Arc::new(
        build_unified_graph(worktree_mgr.base_path(), &plugins, &config)
            .map_err(|e| anyhow!("Failed to build base graph: {e}"))?,
    );
    let target_graph = Arc::new(
        build_unified_graph(worktree_mgr.target_path(), &plugins, &config)
            .map_err(|e| anyhow!("Failed to build target graph: {e}"))?,
    );

    // Phase 3: Compare graphs using CodeGraph comparator
    let comparator = diff_comparator::GraphComparator::new(
        base_graph,
        target_graph,
        workspace_root.clone(),
        worktree_mgr.base_path().to_path_buf(),
        worktree_mgr.target_path().to_path_buf(),
    );
    let mut changes = comparator.compute_changes()?;

    // Phase 4: Apply filters
    if !args.filters.change_types.is_empty() {
        changes.retain(|change| {
            args.filters
                .change_types
                .iter()
                .any(|ct| ct.as_str() == change.change_type)
        });
    }

    if !args.filters.symbol_kinds.is_empty() {
        changes.retain(|change| {
            args.filters
                .symbol_kinds
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case(&change.kind))
        });
    }

    if !args.include_unchanged {
        changes.retain(|change| change.change_type != "unchanged");
    }

    // Phase 5: Compute summary before pagination
    let summary = diff_comparator::compute_summary(&changes);

    // Phase 6: Apply pagination
    let total = changes.len();
    let truncated = total > args.max_results;
    changes.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&changes, &args.pagination);
    let page_changes = page_slice.to_vec();

    let execution_ms = duration_to_ms(start.elapsed());

    tracing::debug!(
        total_changes = total,
        page_size = page_changes.len(),
        execution_ms = execution_ms,
        "semantic_diff tool completed"
    );

    Ok(ToolExecution {
        data: SemanticDiffData {
            base_ref: args.base.git_ref.clone(),
            target_ref: args.target.git_ref.clone(),
            changes: page_changes,
            summary,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms,
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
    // worktree_mgr drops here, automatic cleanup
}

// ============================================================================
// Find Duplicates Tool
// ============================================================================

use crate::execution::types::{
    CycleData, CycleNodeData, DuplicateGroupData, DuplicateSymbolData, FindCyclesData,
    FindDuplicatesData,
};
use crate::tools::{
    CycleType, DuplicateType, FindCyclesArgs, FindDuplicatesArgs, FindUnusedArgs, UnusedScope,
};
use sqry_core::query::DuplicateType as CoreDuplicateType;
use sqry_core::query::{
    CircularConfig, CircularType, DuplicateConfig, build_duplicate_groups_graph,
    find_all_cycles_graph, is_node_in_cycle,
};

/// Convert raw duplicate groups from the core library into output-format `DuplicateGroupData`.
///
/// Performs node lookup, name resolution, file path construction, and URL building
/// for each node in each group.
fn convert_duplicate_groups(
    groups: Vec<sqry_core::query::DuplicateGroup>,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &std::path::Path,
) -> Vec<DuplicateGroupData> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    groups
        .into_iter()
        .filter(|g| g.node_ids.len() > 1)
        .map(|group| {
            let symbols: Vec<DuplicateSymbolData> = group
                .node_ids
                .iter()
                .filter_map(|&node_id| {
                    let entry = snapshot.get_node(node_id)?;

                    let name = strings
                        .resolve(entry.name)
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    let language = files.language_for_file(entry.file);
                    let qualified_name =
                        crate::execution::symbol_utils::display_entry_qualified_name(
                            entry, strings, language, &name,
                        );

                    let file_path = files.resolve(entry.file)?;
                    let full_path = workspace_root.join(file_path.as_ref());
                    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                        Into::into,
                    );

                    let language = language.map_or("unknown".to_string(), |l| l.to_string());

                    Some(DuplicateSymbolData {
                        name,
                        qualified_name,
                        kind: format!("{:?}", entry.kind).to_lowercase(),
                        file_uri,
                        line: entry.start_line,
                        language,
                    })
                })
                .collect();

            // Format group_id as hex string:
            // - For body duplicates with 128-bit hash: 32-char hex
            // - For others: 16-char hex from u64
            let group_id = if let Some(body_hash) = group.body_hash_128 {
                format!("{body_hash}") // BodyHash128::Display is 32-char hex
            } else {
                format!("{:016x}", group.hash)
            };

            DuplicateGroupData {
                group_id,
                count: symbols.len(),
                symbols,
            }
        })
        .filter(|g| g.count > 1)
        .collect()
}

/// Execute the `find_duplicates` tool to find duplicate code patterns.
///
/// Uses `CodeGraph` with `body_hash` for body duplicate detection and signature/struct
/// hashing for other duplicate types.
pub fn execute_find_duplicates(
    args: &FindDuplicatesArgs,
) -> Result<ToolExecution<FindDuplicatesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    // Convert MCP DuplicateType to core DuplicateType
    let (core_dup_type, dup_type_str) = match args.duplicate_type {
        DuplicateType::Body => (CoreDuplicateType::Body, "body"),
        DuplicateType::Signature => (CoreDuplicateType::Signature, "signature"),
        DuplicateType::Struct => (CoreDuplicateType::Struct, "struct"),
    };

    // Build duplicate detection config
    let config = DuplicateConfig {
        threshold: if args.exact {
            1.0
        } else {
            f64::from(args.threshold) / 100.0
        },
        max_results: args.max_results,
        is_exact_only: args.exact || args.threshold >= 100,
    };

    // Find duplicates using CodeGraph
    let groups = build_duplicate_groups_graph(core_dup_type, &graph, &config);

    let snapshot = graph.snapshot();

    // Convert to output format
    let mut output_groups = convert_duplicate_groups(groups, &snapshot, &workspace_root);

    // Sort by group size (largest first), secondary by group_id for stability
    output_groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.group_id.cmp(&b.group_id))
    });

    let total = output_groups.len();
    let truncated = total > args.max_results;
    output_groups.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&output_groups, &args.pagination);
    let page_groups = page_slice.to_vec();

    Ok(ToolExecution {
        data: FindDuplicatesData {
            duplicate_type: dup_type_str.to_string(),
            threshold: args.threshold,
            groups: page_groups,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

// ============================================================================
// Find Cycles Tool
// ============================================================================

/// Resolve a single cycle symbol name to a `CycleNodeData`, looking up node info from the graph.
fn resolve_cycle_node(
    name: &str,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &std::path::Path,
) -> CycleNodeData {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let node_ids = candidate_bucket_for_symbol(snapshot, name);
    if let Some(&node_id) = node_ids.first()
        && let Some(entry) = snapshot.get_node(node_id)
    {
        let node_name = strings
            .resolve(entry.name)
            .map_or_else(|| name.to_string(), |s| s.to_string());
        let language = files.language_for_file(entry.file);
        let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry, strings, language, &node_name,
        );
        let file_path = files
            .resolve(entry.file)
            .map(|p| workspace_root.join(p.as_ref()))
            .unwrap_or_default();
        let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
            || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
            Into::into,
        );

        return CycleNodeData {
            name: node_name,
            qualified_name,
            file_uri,
            line: entry.start_line,
        };
    }

    // Node not found, use name as-is
    CycleNodeData {
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_uri: String::new(),
        line: 0,
    }
}

/// Convert raw cycle data (lists of symbol name strings) into output-format `CycleData` structs.
fn convert_cycles_to_output(
    cycles: Vec<Vec<String>>,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &std::path::Path,
) -> Vec<CycleData> {
    cycles
        .into_iter()
        .map(|cycle| {
            let nodes: Vec<CycleNodeData> = cycle
                .iter()
                .map(|name| resolve_cycle_node(name, snapshot, workspace_root))
                .collect();

            // Build chain string: A -> B -> C -> A
            let chain = if cycle.is_empty() {
                String::new()
            } else {
                let mut parts = cycle.clone();
                parts.push(cycle[0].clone()); // Close the cycle
                parts.join(" \u{2192} ")
            };

            CycleData {
                depth: nodes.len(),
                nodes,
                chain,
            }
        })
        .collect()
}

/// Execute the `find_cycles` tool to find circular dependencies.
///
/// Uses `CodeGraph` with `find_all_cycles_graph` for cycle detection.
pub fn execute_find_cycles(args: &FindCyclesArgs) -> Result<ToolExecution<FindCyclesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    // Convert args to core types
    let core_cycle_type = match args.cycle_type {
        CycleType::Calls => CircularType::Calls,
        CycleType::Imports => CircularType::Imports,
        CycleType::Modules => CircularType::Modules,
    };

    let config = CircularConfig {
        min_depth: args.min_depth,
        max_depth: args.max_depth,
        max_results: args.max_results,
        should_include_self_loops: args.include_self_loops,
    };

    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&workspace_root);
    let snapshot = graph.snapshot();
    let cycles = if let Some(scc_data) = sqry_core::graph::unified::analysis::try_load_scc(
        &storage,
        &snapshot,
        cycle_edge_kind_name(args.cycle_type),
    ) {
        extract_cycles_from_scc(&graph, &scc_data, &config)
    } else {
        compute_cycles_from_graph(&graph, &snapshot, args.cycle_type, core_cycle_type, &config)?
    };

    // Convert to output format
    let output_cycles = convert_cycles_to_output(cycles, &snapshot, &workspace_root);

    let total = output_cycles.len();
    let (page_slice, next_page_token) = paginate(&output_cycles, &args.pagination);
    let page_cycles = page_slice.to_vec();

    Ok(ToolExecution {
        data: FindCyclesData {
            cycle_type: cycle_type_label(args.cycle_type).to_string(),
            cycles: page_cycles,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

// ============================================================================
// Find Unused Tool
// ============================================================================

/// Execute the `find_unused` tool to find unused/dead code.
///
/// Note: This feature requires reachability analysis which is being migrated to `CodeGraph`.
/// Currently returns empty results as unused detection is being migrated.
pub fn execute_find_unused(args: &FindUnusedArgs) -> Result<ToolExecution<FindUnusedData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    let scope_str = match args.scope {
        UnusedScope::Public => "public",
        UnusedScope::Private => "private",
        UnusedScope::Function => "function",
        UnusedScope::Struct => "struct",
        UnusedScope::All => "all",
    };

    // Try Pass 5 optimization: use precomputed analyses for reachability
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&workspace_root);

    let unused_symbols = if let Some((scc, cond)) =
        sqry_core::graph::unified::analysis::try_load_scc_and_condensation(
            &storage, &snapshot, "calls",
        ) {
        // Fast path: reachability-based unused detection
        find_unused_with_reachability(&snapshot, &scc, &cond, args, &workspace_root)
    } else {
        // No analyses available — skip rather than hang
        tracing::warn!(
            "Analysis files not found at {}. Run `sqry index --force` or `sqry analyze` to rebuild analysis data for find_unused.",
            storage.analysis_dir().display(),
        );
        Vec::new()
    };

    let total = unused_symbols.len();
    let (page_slice, next_page_token) = paginate(&unused_symbols, &args.pagination);

    Ok(ToolExecution {
        data: FindUnusedData {
            scope: scope_str.to_string(),
            symbols: page_slice.to_vec(),
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

// ============================================================================
// New Graph-Based Analysis Tools
// ============================================================================

use crate::execution::types::{
    CallerCalleeData, DirectCalleesData, DirectCallersData, NodeInCycleData, PatternMatchData,
    PatternSearchData,
};
use crate::tools::{DirectCalleesArgs, DirectCallersArgs, IsNodeInCycleArgs, PatternSearchArgs};

/// Find the cycle containing a specific node, if it is in one.
///
/// Returns `(true, Some(cycle))` if the node is in a cycle, `(false, None)` otherwise.
/// Uses precomputed SCC data when available for O(1) membership check.
fn find_cycle_containing_node(
    node_id: sqry_core::graph::unified::node::NodeId,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    core_cycle_type: CircularType,
    config: &CircularConfig,
    scc_data_opt: Option<&sqry_core::graph::unified::analysis::SccData>,
) -> (bool, Option<Vec<String>>) {
    let strings = snapshot.strings();

    let node_in_cycle = if let Some(scc_data) = scc_data_opt {
        is_node_in_cycle_precomputed(node_id, scc_data, config)
    } else {
        is_node_in_cycle(node_id, core_cycle_type, graph, config)
    };

    if !node_in_cycle {
        return (false, None);
    }

    // Only fetch cycle details if node is actually in a cycle
    let cycles = if let Some(scc_data) = scc_data_opt {
        extract_cycles_from_scc(graph, scc_data, config)
    } else {
        find_all_cycles_graph(core_cycle_type, graph, config)
    };

    // Get the node's name for comparison
    let entry = snapshot.get_node(node_id);
    let node_name = entry
        .and_then(|e| {
            e.qualified_name
                .and_then(|id| strings.resolve(id))
                .or_else(|| strings.resolve(e.name))
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let found_cycle = cycles
        .iter()
        .find(|c| c.iter().any(|name| name == &node_name));

    (true, found_cycle.cloned())
}

/// Execute the `is_node_in_cycle` tool to check if a symbol is in a cycle.
///
/// Uses the optimized `is_node_in_cycle` function for efficient cycle detection.
/// Only performs full cycle enumeration if the node is found to be in a cycle.
pub fn execute_is_node_in_cycle(
    args: &IsNodeInCycleArgs,
) -> Result<ToolExecution<NodeInCycleData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Find the node by name
    let node_id = resolve_global_symbol_strict(&snapshot, &args.symbol)?;

    let core_cycle_type = match args.cycle_type {
        CycleType::Calls => CircularType::Calls,
        CycleType::Imports => CircularType::Imports,
        CycleType::Modules => CircularType::Modules,
    };

    let config = CircularConfig {
        min_depth: args.min_depth,
        max_depth: args.max_depth,
        max_results: 100,
        should_include_self_loops: args.include_self_loops,
    };

    let edge_kind_name = match args.cycle_type {
        CycleType::Calls | CycleType::Modules => "calls",
        CycleType::Imports => "imports",
    };

    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&workspace_root);
    let scc_data_opt =
        sqry_core::graph::unified::analysis::try_load_scc(&storage, &snapshot, edge_kind_name);

    let (in_cycle, cycle) = find_cycle_containing_node(
        node_id,
        &graph,
        &snapshot,
        core_cycle_type,
        &config,
        scc_data_opt.as_ref(),
    );

    let cycle_type_str = match args.cycle_type {
        CycleType::Calls => "calls",
        CycleType::Imports => "imports",
        CycleType::Modules => "modules",
    };

    Ok(ToolExecution {
        data: NodeInCycleData {
            symbol: args.symbol.clone(),
            in_cycle,
            cycle_type: cycle_type_str.to_string(),
            cycle,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: None,
        truncated: None,
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `pattern_search` tool to find symbols by pattern.
pub fn execute_pattern_search(
    args: &PatternSearchArgs,
) -> Result<ToolExecution<PatternSearchData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find nodes matching the pattern
    let matching_ids = snapshot.find_by_pattern(&args.pattern);

    // Filter out classpath (external) nodes unless explicitly requested
    let filtered_ids: Vec<_> = if args.include_classpath {
        matching_ids
    } else {
        matching_ids
            .into_iter()
            .filter(|&node_id| {
                !crate::execution::symbol_utils::is_node_external(&snapshot, node_id)
            })
            .collect()
    };

    // Convert to output format
    let mut matches: Vec<PatternMatchData> = filtered_ids
        .into_iter()
        .filter_map(|node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let language = files.language_for_file(entry.file);
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry, strings, language, &name,
            );

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                Into::into,
            );

            let language = language.map_or("unknown".to_string(), |l| l.to_string());

            let kind = format!("{:?}", entry.kind).to_lowercase();

            let provenance = crate::execution::symbol_utils::get_classpath_provenance_for_node(
                &snapshot, node_id,
            );

            Some(PatternMatchData {
                name,
                qualified_name,
                kind,
                file_uri,
                line: entry.start_line,
                language,
                provenance,
            })
        })
        .collect();

    let total = matches.len();
    matches.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&matches, &args.pagination);
    let page_matches = page_slice.to_vec();

    Ok(ToolExecution {
        data: PatternSearchData {
            pattern: args.pattern.clone(),
            matches: page_matches,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Find one symbol using shared candidate semantics.
///
/// Candidate lookup preserves suffix fallback but does not silently collapse
/// ambiguity to the first match.
fn resolve_single_candidate_node(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Result<sqry_core::graph::unified::node::NodeId> {
    match snapshot.find_symbol_candidates(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    }) {
        SymbolCandidateOutcome::Candidates(candidates) => match candidates.as_slice() {
            [node_id] => Ok(*node_id),
            [] => Err(anyhow!("Symbol '{symbol}' not found in graph.")),
            _ => Err(anyhow!(
                "Symbol '{symbol}' is ambiguous in graph ({} candidates). Use a canonical qualified name.",
                candidates.len()
            )),
        },
        SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => {
            Err(anyhow!("Symbol '{symbol}' not found in graph."))
        }
    }
}

/// Find similar symbol names for error message suggestions.
///
/// Uses simple substring and prefix matching to find candidates.
fn find_similar_symbols(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    query: &str,
    max_suggestions: usize,
) -> Vec<String> {
    let query_lower = query.to_lowercase();
    let strings = snapshot.strings();
    let mut suggestions = Vec::new();

    // Extract the simple name part (after last :: or .)
    let simple_query = query_lower
        .rsplit("::")
        .next()
        .unwrap_or(&query_lower)
        .split('.')
        .next_back()
        .unwrap_or(&query_lower);

    for (_node_id, entry) in snapshot.nodes().iter() {
        if suggestions.len() >= max_suggestions * 2 {
            break;
        }

        let name = match entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .or_else(|| strings.resolve(entry.name))
        {
            Some(n) => n.to_string(),
            None => continue,
        };

        let name_lower = name.to_lowercase();

        // Check for substring match (either direction) with a minimum length threshold
        // to avoid matching single-character names like "b" for "build_all_with_budget"
        let is_substring_match = name_lower.contains(simple_query)
            || (simple_query.contains(&name_lower) && name_lower.len() >= 3);
        if is_substring_match && name != query {
            suggestions.push(name);
        }
    }

    // Deduplicate and limit
    suggestions.sort();
    suggestions.dedup();
    suggestions.truncate(max_suggestions);
    suggestions
}

fn decorate_single_symbol_lookup_error(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    if !error.to_string().contains("not found in graph") {
        return error;
    }

    let suggestions = find_similar_symbols(snapshot, symbol, 3);
    let suggestions_str = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDid you mean one of these?\n  • {}",
            suggestions.join("\n  • ")
        )
    };

    anyhow!(
        "Symbol '{symbol}' not found in graph.\n\n\
         Hints:\n\
         • Use the full qualified name (e.g., 'module::function_name')\n\
         • Check for typos in the symbol name\n\
         • Run 'semantic_search' to find the correct symbol name{suggestions_str}"
    )
}

/// Execute the `direct_callers` tool to find direct callers of a symbol.
pub fn execute_direct_callers(
    args: &DirectCallersArgs,
) -> Result<ToolExecution<DirectCallersData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find the target node (with suffix matching for unqualified names)
    let target_node_id = resolve_single_candidate_node(&snapshot, &args.symbol)
        .map_err(|error| decorate_single_symbol_lookup_error(&snapshot, &args.symbol, error))?;

    // Get callers using the graph's reverse edges
    let caller_ids = snapshot.get_callers(target_node_id);

    // Convert to output format
    let mut callers: Vec<CallerCalleeData> = caller_ids
        .into_iter()
        .filter_map(|node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let language = files.language_for_file(entry.file);
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry, strings, language, &name,
            );

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                Into::into,
            );

            let language = language.map_or("unknown".to_string(), |l| l.to_string());

            let kind = format!("{:?}", entry.kind).to_lowercase();

            Some(CallerCalleeData {
                name,
                qualified_name,
                kind,
                file_uri,
                line: entry.start_line,
                language,
            })
        })
        .collect();

    let total = callers.len();
    callers.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&callers, &args.pagination);
    let page_callers = page_slice.to_vec();

    Ok(ToolExecution {
        data: DirectCallersData {
            target: args.symbol.clone(),
            callers: page_callers,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `direct_callees` tool to find direct callees of a symbol.
pub fn execute_direct_callees(
    args: &DirectCalleesArgs,
) -> Result<ToolExecution<DirectCalleesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find the source node (with suffix matching for unqualified names)
    let source_node_id = resolve_single_candidate_node(&snapshot, &args.symbol)
        .map_err(|error| decorate_single_symbol_lookup_error(&snapshot, &args.symbol, error))?;

    // Get callees using the graph's forward edges
    let callee_ids = snapshot.get_callees(source_node_id);

    // Convert to output format
    let mut callees: Vec<CallerCalleeData> = callee_ids
        .into_iter()
        .filter_map(|node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let language = files.language_for_file(entry.file);
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry, strings, language, &name,
            );

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                Into::into,
            );

            let language = language.map_or("unknown".to_string(), |l| l.to_string());

            let kind = format!("{:?}", entry.kind).to_lowercase();

            Some(CallerCalleeData {
                name,
                qualified_name,
                kind,
                file_uri,
                line: entry.start_line,
                language,
            })
        })
        .collect();

    let total = callees.len();
    callees.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&callees, &args.pagination);
    let page_callees = page_slice.to_vec();

    Ok(ToolExecution {
        data: DirectCalleesData {
            source: args.symbol.clone(),
            callees: page_callees,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::analysis::CsrAdjacency;
    use sqry_core::graph::unified::compaction::snapshot_edges;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;
    use sqry_core::query::CircularConfig;
    use std::path::Path;

    #[test]
    fn test_extract_cycles_from_scc_self_loop_respects_config() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let name_a = graph.strings_mut().intern("A").unwrap();
        let name_b = graph.strings_mut().intern("B").unwrap();

        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_b, file_id))
            .unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_a, kind.clone(), file_id);
        graph
            .edges_mut()
            .add_edge(node_a, node_b, kind.clone(), file_id);

        let snapshot = graph.snapshot();
        let edges = snapshot.edges();
        let forward_store = edges.forward();
        let node_count = snapshot.nodes().len();
        let compaction_snapshot = snapshot_edges(&forward_store, node_count);

        let csr = CsrAdjacency::build_from_snapshot(&compaction_snapshot).unwrap();
        let scc_data =
            sqry_core::graph::unified::analysis::SccData::compute_tarjan(&csr, &kind).unwrap();

        let config_no_self = CircularConfig {
            min_depth: 3,
            max_depth: None,
            max_results: 10,
            should_include_self_loops: false,
        };
        let cycles_no_self = extract_cycles_from_scc(&graph, &scc_data, &config_no_self);
        assert!(cycles_no_self.is_empty());

        let config_with_self = CircularConfig {
            min_depth: 3,
            max_depth: None,
            max_results: 10,
            should_include_self_loops: true,
        };
        let cycles_with_self = extract_cycles_from_scc(&graph, &scc_data, &config_with_self);
        assert_eq!(cycles_with_self.len(), 1);
        assert_eq!(cycles_with_self[0], vec!["A".to_string()]);
    }

    // ========================================================================
    // Find Unused Tests
    // ========================================================================

    /// Helper to create a test graph with entry points and reachable/unreachable nodes
    fn create_test_graph_for_unused() -> CodeGraph {
        let mut graph = CodeGraph::new();

        // Register test files
        let main_rs = graph
            .files_mut()
            .register(Path::new("src/main.rs"))
            .unwrap();
        let lib_rs = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let helper_rs = graph
            .files_mut()
            .register(Path::new("src/helper.rs"))
            .unwrap();

        // Create nodes
        let main_fn = graph.strings_mut().intern("main").unwrap();
        let public_fn = graph.strings_mut().intern("public_function").unwrap();
        let used_fn = graph.strings_mut().intern("used_helper").unwrap();
        let unused_fn = graph.strings_mut().intern("unused_helper").unwrap();

        // Create visibility strings
        let vis_public = graph.strings_mut().intern("public").unwrap();
        let vis_private = graph.strings_mut().intern("private").unwrap();

        // Allocate nodes
        let mut main_entry = NodeEntry::new(NodeKind::Function, main_fn, main_rs);
        main_entry.visibility = Some(vis_public);
        let node_main = graph.nodes_mut().alloc(main_entry).unwrap();
        graph
            .indices_mut()
            .add(node_main, NodeKind::Function, main_fn, None, main_rs);

        let mut public_entry = NodeEntry::new(NodeKind::Function, public_fn, lib_rs);
        public_entry.visibility = Some(vis_public);
        let node_public = graph.nodes_mut().alloc(public_entry).unwrap();
        graph
            .indices_mut()
            .add(node_public, NodeKind::Function, public_fn, None, lib_rs);

        let mut used_entry = NodeEntry::new(NodeKind::Function, used_fn, helper_rs);
        used_entry.visibility = Some(vis_private);
        let node_used = graph.nodes_mut().alloc(used_entry).unwrap();
        graph
            .indices_mut()
            .add(node_used, NodeKind::Function, used_fn, None, helper_rs);

        let mut unused_entry = NodeEntry::new(NodeKind::Function, unused_fn, helper_rs);
        unused_entry.visibility = Some(vis_private);
        let node_unused = graph.nodes_mut().alloc(unused_entry).unwrap();
        graph
            .indices_mut()
            .add(node_unused, NodeKind::Function, unused_fn, None, helper_rs);

        // Create call edges: main -> public_function -> used_helper
        // unused_helper is not called by anyone
        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        graph
            .edges_mut()
            .add_edge(node_main, node_public, call_kind.clone(), main_rs);
        graph
            .edges_mut()
            .add_edge(node_public, node_used, call_kind, lib_rs);

        // Add export edge from lib.rs for public_function
        use sqry_core::graph::unified::ExportKind;
        let export_kind = EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        };
        graph
            .edges_mut()
            .add_edge(node_public, node_public, export_kind, lib_rs);

        graph
    }

    #[test]
    fn test_rust_entry_point_detection_main() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let files = snapshot.files();

        // Find main function
        let main_node = candidate_bucket_for_symbol(&snapshot, "main")
            .into_iter()
            .next()
            .unwrap();
        let entry = snapshot.get_node(main_node).unwrap();

        // Test that main() is detected as an entry point
        assert!(is_rust_entry_point(main_node, entry, files, &snapshot));
    }

    #[test]
    fn test_rust_entry_point_detection_public_export() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let files = snapshot.files();

        // Find public_function
        let public_node = candidate_bucket_for_symbol(&snapshot, "public_function")
            .into_iter()
            .next()
            .unwrap();
        let entry = snapshot.get_node(public_node).unwrap();

        // Test that public function with Export edge is detected as entry point
        assert!(is_rust_entry_point(public_node, entry, files, &snapshot));
    }

    #[test]
    fn test_rust_entry_point_detection_private_not_entry() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let files = snapshot.files();

        // Find unused_helper (private, no export)
        let unused_node = candidate_bucket_for_symbol(&snapshot, "unused_helper")
            .into_iter()
            .next()
            .unwrap();
        let entry = snapshot.get_node(unused_node).unwrap();

        // Test that private function without export is NOT an entry point
        assert!(!is_rust_entry_point(unused_node, entry, files, &snapshot));
    }

    #[test]
    fn test_entry_point_detection_integration() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let files = snapshot.files();

        // Verify main is detected as entry point
        let main_nodes = candidate_bucket_for_symbol(&snapshot, "main");
        assert!(!main_nodes.is_empty(), "main function should exist");
        let main_node = main_nodes[0];
        let main_entry = snapshot.get_node(main_node).unwrap();
        assert!(
            is_rust_entry_point(main_node, main_entry, files, &snapshot),
            "main should be detected as entry point"
        );

        // Verify public_function is detected as entry point
        let public_nodes = candidate_bucket_for_symbol(&snapshot, "public_function");
        assert!(!public_nodes.is_empty(), "public_function should exist");
        let public_node = public_nodes[0];
        let public_entry = snapshot.get_node(public_node).unwrap();
        assert!(
            is_rust_entry_point(public_node, public_entry, files, &snapshot),
            "public_function should be detected as entry point"
        );

        // Verify unused_helper is NOT detected as entry point
        let unused_nodes = candidate_bucket_for_symbol(&snapshot, "unused_helper");
        assert!(!unused_nodes.is_empty(), "unused_helper should exist");
        let unused_node = unused_nodes[0];
        let unused_entry = snapshot.get_node(unused_node).unwrap();
        assert!(
            !is_rust_entry_point(unused_node, unused_entry, files, &snapshot),
            "unused_helper should NOT be detected as entry point"
        );
    }

    #[test]
    fn test_matches_scope_filter_public() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();

        let public_node = candidate_bucket_for_symbol(&snapshot, "public_function")
            .into_iter()
            .next()
            .unwrap();
        let public_entry = snapshot.get_node(public_node).unwrap();

        let visibility_string = public_entry
            .visibility
            .and_then(|vid| strings.resolve(vid))
            .map(|s| s.to_string());
        let visibility_str = visibility_string.as_deref();

        // Public function should match Public scope
        assert!(matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::Public
        ));

        // Public function should match All scope
        assert!(matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::All
        ));

        // Public function should NOT match Private scope
        assert!(!matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::Private
        ));
    }

    #[test]
    fn test_matches_scope_filter_private() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();

        let unused_node = candidate_bucket_for_symbol(&snapshot, "unused_helper")
            .into_iter()
            .next()
            .unwrap();
        let unused_entry = snapshot.get_node(unused_node).unwrap();

        let visibility_string = unused_entry
            .visibility
            .and_then(|vid| strings.resolve(vid))
            .map(|s| s.to_string());
        let visibility_str = visibility_string.as_deref();

        // Private function should match Private scope
        assert!(matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::Private
        ));

        // Private function should match All scope
        assert!(matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::All
        ));

        // Private function should NOT match Public scope
        assert!(!matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::Public
        ));
    }

    #[test]
    fn test_matches_scope_filter_function() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();

        let main_node = candidate_bucket_for_symbol(&snapshot, "main")
            .into_iter()
            .next()
            .unwrap();
        let main_entry = snapshot.get_node(main_node).unwrap();

        // Function should match Function scope
        assert!(matches_scope_filter(
            main_entry.kind,
            None,
            UnusedScope::Function
        ));

        // Function should NOT match Struct scope
        assert!(!matches_scope_filter(
            main_entry.kind,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_python_entry_point_detection() {
        let mut graph = CodeGraph::new();
        let init_py = graph
            .files_mut()
            .register(Path::new("mypackage/__init__.py"))
            .unwrap();

        let func_name = graph.strings_mut().intern("my_function").unwrap();
        let vis_public = graph.strings_mut().intern("public").unwrap();

        let mut func_entry = NodeEntry::new(NodeKind::Function, func_name, init_py);
        func_entry.visibility = Some(vis_public);
        let func_node = graph.nodes_mut().alloc(func_entry).unwrap();

        let snapshot = graph.snapshot();
        let files = snapshot.files();
        let entry = snapshot.get_node(func_node).unwrap();

        // Functions in __init__.py should be entry points
        assert!(is_python_entry_point(func_node, entry, files, &snapshot));
    }

    #[test]
    fn test_js_ts_entry_point_detection() {
        use sqry_core::graph::unified::ExportKind;

        let mut graph = CodeGraph::new();
        let index_js = graph
            .files_mut()
            .register(Path::new("src/index.js"))
            .unwrap();

        let func_name = graph.strings_mut().intern("exportedFunction").unwrap();
        let func_entry = NodeEntry::new(NodeKind::Function, func_name, index_js);
        let func_node = graph.nodes_mut().alloc(func_entry).unwrap();

        // Add export edge
        let export_kind = EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        };
        graph
            .edges_mut()
            .add_edge(func_node, func_node, export_kind, index_js);

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(func_node).unwrap();

        // Functions with Export edges should be entry points
        assert!(is_js_ts_entry_point(func_node, entry, &snapshot));
    }

    #[test]
    fn test_generic_entry_point_detection_main() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("main.unknown"))
            .unwrap();

        let main_name = graph.strings_mut().intern("main").unwrap();
        let main_entry = NodeEntry::new(NodeKind::Function, main_name, file_id);
        let main_node = graph.nodes_mut().alloc(main_entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(main_node).unwrap();

        // main() should always be an entry point
        assert!(is_generic_entry_point(main_node, entry, &snapshot));
    }

    // ========================================================================
    // Cycle helper mapping function tests
    // ========================================================================

    #[test]
    fn test_cycle_edge_kind_name_calls() {
        assert_eq!(cycle_edge_kind_name(CycleType::Calls), "calls");
    }

    #[test]
    fn test_cycle_edge_kind_name_imports() {
        assert_eq!(cycle_edge_kind_name(CycleType::Imports), "imports");
    }

    #[test]
    fn test_cycle_edge_kind_name_modules() {
        assert_eq!(cycle_edge_kind_name(CycleType::Modules), "imports");
    }

    #[test]
    fn test_cycle_edge_kind_calls() {
        let kind = cycle_edge_kind(CycleType::Calls);
        assert_eq!(
            kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            }
        );
    }

    #[test]
    fn test_cycle_edge_kind_imports() {
        let kind = cycle_edge_kind(CycleType::Imports);
        assert_eq!(
            kind,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            }
        );
    }

    #[test]
    fn test_cycle_edge_kind_modules() {
        let kind = cycle_edge_kind(CycleType::Modules);
        assert_eq!(
            kind,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            }
        );
    }

    #[test]
    fn test_cycle_type_label_calls() {
        assert_eq!(cycle_type_label(CycleType::Calls), "calls");
    }

    #[test]
    fn test_cycle_type_label_imports() {
        assert_eq!(cycle_type_label(CycleType::Imports), "imports");
    }

    #[test]
    fn test_cycle_type_label_modules() {
        assert_eq!(cycle_type_label(CycleType::Modules), "modules");
    }

    // ========================================================================
    // should_include_scc tests
    // ========================================================================

    fn make_circular_config(
        min_depth: usize,
        max_depth: Option<usize>,
        self_loops: bool,
    ) -> CircularConfig {
        CircularConfig {
            min_depth,
            max_depth,
            max_results: 100,
            should_include_self_loops: self_loops,
        }
    }

    #[test]
    fn test_should_include_scc_self_loop_included_when_enabled() {
        let config = make_circular_config(2, None, true);
        // size=1 + is_self_loop=true + config allows self loops
        assert!(should_include_scc(1, true, &config));
    }

    #[test]
    fn test_should_include_scc_self_loop_excluded_when_disabled() {
        let config = make_circular_config(2, None, false);
        assert!(!should_include_scc(1, true, &config));
    }

    #[test]
    fn test_should_include_scc_size_one_no_self_loop_excluded() {
        let config = make_circular_config(1, None, false);
        // size=1, not a self-loop => excluded
        assert!(!should_include_scc(1, false, &config));
    }

    #[test]
    fn test_should_include_scc_smaller_than_min_depth_excluded() {
        let config = make_circular_config(3, None, false);
        // size=2, min_depth=3
        assert!(!should_include_scc(2, false, &config));
    }

    #[test]
    fn test_should_include_scc_at_min_depth_included() {
        let config = make_circular_config(2, None, false);
        assert!(should_include_scc(2, false, &config));
    }

    #[test]
    fn test_should_include_scc_larger_than_max_depth_excluded() {
        let config = make_circular_config(1, Some(3), false);
        assert!(!should_include_scc(4, false, &config));
    }

    #[test]
    fn test_should_include_scc_at_max_depth_included() {
        let config = make_circular_config(1, Some(3), false);
        assert!(should_include_scc(3, false, &config));
    }

    #[test]
    fn test_should_include_scc_no_max_depth_any_size_included() {
        let config = make_circular_config(1, None, false);
        assert!(should_include_scc(100, false, &config));
    }

    // ========================================================================
    // resolve_workspace_path tests
    // ========================================================================

    #[test]
    fn test_resolve_workspace_path_dot_returns_none() {
        assert!(resolve_workspace_path(".").is_none());
    }

    #[test]
    fn test_resolve_workspace_path_explicit_path_returns_some() {
        let result = resolve_workspace_path("/some/path");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), std::path::PathBuf::from("/some/path"));
    }

    // ========================================================================
    // matches_scope_filter for Struct scope
    // ========================================================================

    #[test]
    fn test_matches_scope_filter_struct_for_class() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Class,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_struct_for_interface() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Interface,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_struct_for_trait() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Trait,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_function_for_method() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Method,
            None,
            UnusedScope::Function
        ));
    }

    // ========================================================================
    // convert_cycles_to_output tests
    // ========================================================================

    #[test]
    fn test_convert_cycles_to_output_empty_cycles() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let result = convert_cycles_to_output(vec![], &snapshot, ws);
        assert!(result.is_empty());
    }

    #[test]
    fn test_convert_cycles_to_output_nonempty_cycle() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");

        // A cycle with two symbol names
        let cycles = vec![vec!["alpha".to_string(), "beta".to_string()]];
        let result = convert_cycles_to_output(cycles, &snapshot, ws);
        assert_eq!(result.len(), 1);
        let cycle = &result[0];
        assert_eq!(cycle.depth, 2);
        // Chain should contain both names and close the cycle
        assert!(cycle.chain.contains("alpha"));
        assert!(cycle.chain.contains("beta"));
        // Chain closes: last entry repeated at end
        assert!(cycle.chain.ends_with("alpha"));
    }

    #[test]
    fn test_convert_cycles_to_output_single_node_cycle() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");

        let cycles = vec![vec!["selfloop".to_string()]];
        let result = convert_cycles_to_output(cycles, &snapshot, ws);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].depth, 1);
        // Chain: "selfloop → selfloop"
        assert!(result[0].chain.contains("selfloop"));
    }

    // ========================================================================
    // candidate_bucket_for_symbol tests
    // ========================================================================

    #[test]
    fn test_candidate_bucket_finds_registered_symbol() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/x.rs")).unwrap();
        let nm = graph.strings_mut().intern("my_func").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let candidates = candidate_bucket_for_symbol(&snapshot, "my_func");
        assert!(!candidates.is_empty());
        assert!(candidates.contains(&node_id));
    }

    #[test]
    fn test_candidate_bucket_returns_empty_for_unknown() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let candidates = candidate_bucket_for_symbol(&snapshot, "nonexistent_xyz_123");
        assert!(candidates.is_empty());
    }

    // ========================================================================
    // find_similar_symbols tests
    // ========================================================================

    #[test]
    fn test_find_similar_symbols_finds_substring_match() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();

        for name in &["process_request", "process_response", "other_func"] {
            let nm = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
            let nid = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(nid, NodeKind::Function, nm, None, file_id);
        }

        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "process", 5);
        // Should find process_request and process_response
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.contains("process")));
    }

    #[test]
    fn test_find_similar_symbols_empty_graph_returns_empty() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "anything", 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_find_similar_symbols_truncated_to_max() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        for i in 0..20 {
            let name = format!("foo_func_{i}");
            let nm = graph.strings_mut().intern(&name).unwrap();
            let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
            let nid = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(nid, NodeKind::Function, nm, None, file_id);
        }

        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "foo_func", 3);
        assert!(suggestions.len() <= 3);
    }

    // ========================================================================
    // decorate_single_symbol_lookup_error tests
    // ========================================================================

    #[test]
    fn test_decorate_error_for_not_found_adds_hints() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let err = anyhow::anyhow!("Symbol 'xyz' not found in graph.");
        let decorated = decorate_single_symbol_lookup_error(&snapshot, "xyz", err);
        let msg = decorated.to_string();
        assert!(msg.contains("not found in graph"));
        assert!(msg.contains("Hints:") || msg.contains("Use the full qualified name"));
    }

    #[test]
    fn test_decorate_error_for_other_error_unchanged() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let err = anyhow::anyhow!("Some other error.");
        let decorated = decorate_single_symbol_lookup_error(&snapshot, "xyz", err);
        assert_eq!(decorated.to_string(), "Some other error.");
    }

    // ========================================================================
    // build_unused_symbol_data tests
    // ========================================================================

    #[test]
    fn test_build_unused_symbol_data_basic() {
        let mut graph = CodeGraph::new();
        let ws = Path::new("/workspace");
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("orphan_fn").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_pub);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let data = build_unused_symbol_data(&entry, strings, files, ws);
        assert_eq!(data.name, "orphan_fn");
        assert_eq!(data.kind, "function");
        assert!(!data.file_uri.is_empty());
    }

    // ========================================================================
    // is_java_entry_point tests
    // ========================================================================

    #[test]
    fn test_java_entry_point_public_main_method() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("Main.java")).unwrap();
        let nm = graph.strings_mut().intern("main").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Method, nm, file_id);
        entry.visibility = Some(vis_pub);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_java_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_java_entry_point_public_class() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("MyClass.java"))
            .unwrap();
        let nm = graph.strings_mut().intern("MyClass").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Class, nm, file_id);
        entry.visibility = Some(vis_pub);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_java_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_java_entry_point_private_class_is_not_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("Helper.java"))
            .unwrap();
        let nm = graph.strings_mut().intern("Helper").unwrap();
        let vis_priv = graph.strings_mut().intern("private").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Class, nm, file_id);
        entry.visibility = Some(vis_priv);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(!is_java_entry_point(node_id, entry, &snapshot));
    }

    // ========================================================================
    // is_go_entry_point tests
    // ========================================================================

    #[test]
    fn test_go_entry_point_main_function() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("main.go")).unwrap();
        let nm = graph.strings_mut().intern("main").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_go_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_go_entry_point_capitalized_public_symbol() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("handler.go")).unwrap();
        let nm = graph.strings_mut().intern("HandleRequest").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_pub);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_go_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_go_entry_point_lowercase_private_not_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("internal.go"))
            .unwrap();
        let nm = graph.strings_mut().intern("internalHelper").unwrap();
        let vis_priv = graph.strings_mut().intern("private").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_priv);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(!is_go_entry_point(node_id, entry, &snapshot));
    }

    // ========================================================================
    // is_c_cpp_entry_point tests
    // ========================================================================

    #[test]
    fn test_c_entry_point_main() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("main.c")).unwrap();
        let nm = graph.strings_mut().intern("main").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_c_cpp_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_c_entry_point_non_private_function() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.c")).unwrap();
        let nm = graph.strings_mut().intern("process_data").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        // No private visibility => entry point
        assert!(is_c_cpp_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_c_entry_point_private_function_not_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.c")).unwrap();
        let nm = graph.strings_mut().intern("static_helper").unwrap();
        let vis_priv = graph.strings_mut().intern("private").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_priv);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(!is_c_cpp_entry_point(node_id, entry, &snapshot));
    }

    // ========================================================================
    // is_generic_entry_point tests
    // ========================================================================

    #[test]
    fn test_generic_entry_point_public_visibility() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("main.xyz")).unwrap();
        let nm = graph.strings_mut().intern("public_api").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_pub);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_generic_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_generic_entry_point_with_export_edge() {
        use sqry_core::graph::unified::ExportKind;

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("main.xyz")).unwrap();
        let nm = graph.strings_mut().intern("exported_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let nm2 = graph.strings_mut().intern("target").unwrap();
        let entry2 = NodeEntry::new(NodeKind::Function, nm2, file_id);
        let node2 = graph.nodes_mut().alloc(entry2).unwrap();
        graph
            .indices_mut()
            .add(node2, NodeKind::Function, nm2, None, file_id);

        graph.edges_mut().add_edge(
            node_id,
            node2,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            file_id,
        );

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_generic_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_generic_entry_point_private_no_export_not_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.xyz")).unwrap();
        let nm = graph.strings_mut().intern("private_helper").unwrap();
        let vis_priv = graph.strings_mut().intern("private").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_priv);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(!is_generic_entry_point(node_id, entry, &snapshot));
    }

    // ========================================================================
    // is_python_entry_point edge cases
    // ========================================================================

    #[test]
    fn test_python_entry_point_class_in_init_py() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("mymod/__init__.py"))
            .unwrap();
        let nm = graph.strings_mut().intern("MyClass").unwrap();
        let entry = NodeEntry::new(NodeKind::Class, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let files = snapshot.files();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_python_entry_point(node_id, entry, files, &snapshot));
    }

    #[test]
    fn test_python_entry_point_class_outside_init_is_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("mymod/models.py"))
            .unwrap();
        let nm = graph.strings_mut().intern("SomeClass").unwrap();
        let entry = NodeEntry::new(NodeKind::Class, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let files = snapshot.files();
        let entry = snapshot.get_node(node_id).unwrap();
        // Classes outside __init__.py: matches `entry.kind == NodeKind::Class`
        assert!(is_python_entry_point(node_id, entry, files, &snapshot));
    }

    #[test]
    fn test_python_entry_point_function_outside_init() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("mymod/utils.py"))
            .unwrap();
        let nm = graph.strings_mut().intern("helper").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let files = snapshot.files();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(is_python_entry_point(node_id, entry, files, &snapshot));
    }

    // ========================================================================
    // is_js_ts_entry_point without export edge
    // ========================================================================

    #[test]
    fn test_js_ts_entry_point_function_without_export_is_entry() {
        use sqry_core::graph::unified::node::NodeKind;
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/component.js"))
            .unwrap();
        let nm = graph.strings_mut().intern("MyComponent").unwrap();
        let entry = NodeEntry::new(NodeKind::Component, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        // Component without export edge — falls through to default matches!
        assert!(is_js_ts_entry_point(node_id, entry, &snapshot));
    }

    #[test]
    fn test_js_ts_entry_point_variable_without_export_not_entry() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/utils.js"))
            .unwrap();
        let nm = graph.strings_mut().intern("localVar").unwrap();
        let entry = NodeEntry::new(NodeKind::Variable, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let entry = snapshot.get_node(node_id).unwrap();
        assert!(!is_js_ts_entry_point(node_id, entry, &snapshot));
    }

    // ========================================================================
    // resolve_global_symbol_strict error paths
    // ========================================================================

    #[test]
    fn test_resolve_global_symbol_strict_not_found() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let result = resolve_global_symbol_strict(&snapshot, "nonexistent_symbol_xyz");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_resolve_global_symbol_strict_found() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("unique_function_xyz").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let result = resolve_global_symbol_strict(&snapshot, "unique_function_xyz");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), node_id);
    }

    // ========================================================================
    // resolve_single_candidate_node tests
    // ========================================================================

    #[test]
    fn test_resolve_single_candidate_node_not_found() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let result = resolve_single_candidate_node(&snapshot, "totally_missing");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_single_candidate_node_unique_found() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("solo_func").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let result = resolve_single_candidate_node(&snapshot, "solo_func");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), node_id);
    }

    // ========================================================================
    // collect_impacted_callers_bfs tests
    // ========================================================================

    #[test]
    fn test_collect_impacted_callers_bfs_direct_caller() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("caller_of_target").unwrap();
        let nm_b = graph.strings_mut().intern("target_fn").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm_b, None, file_id);

        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = DependencyImpactArgs {
            symbol: "target_fn".to_string(),
            path: ".".to_string(),
            max_depth: 2,
            max_results: 100,
            include_indirect: false,
            include_files: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        let (impacted, _) = collect_impacted_callers_bfs(&snapshot, node_b, &args, ws);
        assert_eq!(impacted.len(), 1);
        assert_eq!(impacted[0].symbol.name, "caller_of_target");
        assert_eq!(impacted[0].impact_type, "caller");
    }

    #[test]
    fn test_collect_impacted_callers_bfs_include_files() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("caller_with_files").unwrap();
        let nm_b = graph.strings_mut().intern("target_with_files").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm_b, None, file_id);

        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = DependencyImpactArgs {
            symbol: "target_with_files".to_string(),
            path: ".".to_string(),
            max_depth: 1,
            max_results: 100,
            include_indirect: false,
            include_files: true,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        let (impacted, affected_files) = collect_impacted_callers_bfs(&snapshot, node_b, &args, ws);
        assert_eq!(impacted.len(), 1);
        assert!(!affected_files.is_empty());
    }

    // ========================================================================
    // should_include_in_unused_results tests
    // ========================================================================

    #[test]
    fn test_should_include_in_unused_results_passes_all_scope() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("some_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec![],
            kinds: vec![],
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    #[test]
    fn test_should_include_in_unused_results_fails_language_filter() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("some_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        // Filter for Python, but file has no language (registered without language)
        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec!["python".to_string()],
            kinds: vec![],
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(!should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    #[test]
    fn test_should_include_in_unused_results_fails_kind_filter() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("my_struct").unwrap();
        let entry = NodeEntry::new(NodeKind::Struct, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec![],
            kinds: vec!["function".to_string()], // Only functions, but this is a struct
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(!should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    // ========================================================================
    // mark_reachable_from_entries tests (via direct call with real SCC)
    // ========================================================================

    #[test]
    fn test_mark_reachable_empty_entry_points() {
        // Build minimal graph + SCC to test mark_reachable_from_entries
        use sqry_core::graph::unified::analysis::CondensationDag;
        use sqry_core::graph::unified::analysis::CsrAdjacency;
        use sqry_core::graph::unified::compaction::snapshot_edges;

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("empty.rs")).unwrap();
        let nm = graph.strings_mut().intern("lone").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let forward = snapshot.edges().forward();
        let node_count = snapshot.nodes().len();
        let compaction = snapshot_edges(&forward, node_count);
        drop(forward);

        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let csr = CsrAdjacency::build_from_snapshot(&compaction).unwrap();
        let scc =
            sqry_core::graph::unified::analysis::SccData::compute_tarjan(&csr, &call_kind).unwrap();
        let cond = CondensationDag::build(&scc, &csr).unwrap();

        // Zero entry points => empty reachable set
        let reachable = mark_reachable_from_entries(&[], &scc, &cond);
        assert!(reachable.is_empty());
    }

    // ========================================================================
    // process_caller_node tests
    // ========================================================================

    #[test]
    fn test_process_caller_node_builds_impacted_symbol() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("caller_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();
        let ws = Path::new("/workspace");

        let args = DependencyImpactArgs {
            symbol: "caller_fn".to_string(),
            path: ".".to_string(),
            max_depth: 2,
            max_results: 100,
            include_indirect: false,
            include_files: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        let (symbol, _file_uri) = process_caller_node(&entry, strings, files, ws, 0, &args);
        assert_eq!(symbol.symbol.name, "caller_fn");
        assert_eq!(symbol.impact_type, "caller");
        assert_eq!(symbol.depth, 1); // depth+1
    }
}
