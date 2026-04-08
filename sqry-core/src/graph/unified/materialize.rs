//! Core node materialization and seed lookup contracts.
//!
//! These helpers operate on `GraphSnapshot` and produce crate-agnostic output
//! types. Consumer crates (LSP, MCP, CLI) build their protocol-specific types
//! from the `MaterializedNode` output.

use super::concurrent::GraphSnapshot;
use super::node::id::NodeId;
use super::resolution::{
    FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery, display_graph_qualified_name,
};
use super::storage::StringInterner;
use super::storage::arena::NodeEntry;
use super::storage::registry::FileRegistry;

/// Crate-agnostic representation of a fully resolved graph node.
///
/// Consumers (LSP, MCP, CLI) convert this into their protocol-specific types
/// without reimplementing resolution or formatting logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedNode {
    /// Graph node identity — stable within a snapshot for index-based edge linking.
    pub node_id: NodeId,
    /// Simple (unqualified) name of the symbol.
    pub name: String,
    /// Language-aware qualified name (e.g., `MyApp.User.GetName` for C#).
    pub qualified_name: String,
    /// Lowercased debug representation of the `NodeKind` variant.
    pub kind: String,
    /// Lowercased language name derived from the file registry.
    pub language: String,
    /// Display path of the file containing the symbol.
    pub file_path: String,
    /// One-indexed start line of the symbol span.
    pub start_line: u32,
    /// One-indexed end line of the symbol span.
    pub end_line: u32,
}

/// Build a language-aware display qualified name for a `NodeEntry`.
///
/// This is the **canonical** implementation. Consumer crates (LSP, MCP, CLI)
/// should call this instead of maintaining their own copy.
///
/// Resolves the entry's `qualified_name` through the string interner, applies
/// language-specific display normalization (e.g., `::` → `.` for C#), and falls
/// back to `fallback_name` when no qualified name is stored.
#[must_use]
pub fn display_entry_qualified_name(
    entry: &NodeEntry,
    strings: &StringInterner,
    files: &FileRegistry,
    fallback_name: &str,
) -> String {
    entry
        .qualified_name
        .and_then(|qn_id| strings.resolve(qn_id))
        .map_or_else(
            || fallback_name.to_string(),
            |qualified| {
                files.language_for_file(entry.file).map_or_else(
                    || qualified.to_string(),
                    |language| {
                        display_graph_qualified_name(
                            language,
                            qualified.as_ref(),
                            entry.kind,
                            entry.is_static,
                        )
                    },
                )
            },
        )
}

/// Resolve one symbol query into ordered candidate seeds.
///
/// Uses `FileScope::Any` and `ResolutionMode::AllowSuffixCandidates` to produce
/// the broadest set of matches suitable for graph traversal entry points.
#[must_use]
pub fn find_nodes_by_name(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    match snapshot.find_symbol_candidates(&SymbolQuery {
        symbol: name,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    }) {
        SymbolCandidateOutcome::Candidates(matches) => matches,
        SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
    }
}

/// Resolve several symbol queries into a stable, deduplicated seed set.
///
/// Calls [`find_nodes_by_name`] for each symbol, then sorts and deduplicates
/// the combined result for deterministic downstream processing.
#[must_use]
pub fn collect_symbol_seeds(snapshot: &GraphSnapshot, symbols: &[String]) -> Vec<NodeId> {
    let mut seeds: Vec<NodeId> = Vec::new();
    for symbol in symbols {
        seeds.extend(find_nodes_by_name(snapshot, symbol));
    }
    seeds.sort_unstable();
    seeds.dedup();
    seeds
}

/// Resolve a graph node id to its display qualified name.
///
/// Returns `None` if the node does not exist or has an empty qualified name
/// after resolution.
#[must_use]
pub fn qualified_node_name(snapshot: &GraphSnapshot, node_id: NodeId) -> Option<String> {
    let strings = snapshot.strings();
    let files = snapshot.files();
    let entry = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let qualified_name = display_entry_qualified_name(entry, strings, files, &name);

    (!qualified_name.is_empty()).then_some(qualified_name)
}

/// Materialize a graph node into a crate-agnostic `MaterializedNode`.
///
/// Returns `None` if the node does not exist or has an empty qualified name
/// after resolution. Consumers build protocol-specific types from the returned
/// value.
#[must_use]
pub fn materialize_node(snapshot: &GraphSnapshot, node_id: NodeId) -> Option<MaterializedNode> {
    let strings = snapshot.strings();
    let files = snapshot.files();
    let entry = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|value| value.to_string())
        .unwrap_or_default();

    let qualified_name = display_entry_qualified_name(entry, strings, files, &name);
    if qualified_name.is_empty() {
        return None;
    }

    let kind = format!("{:?}", entry.kind).to_lowercase();
    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |lang| {
            lang.to_string().to_ascii_lowercase()
        });
    let file_path = files
        .resolve(entry.file)
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    Some(MaterializedNode {
        node_id,
        name,
        qualified_name,
        kind,
        language,
        file_path,
        start_line: entry.start_line,
        end_line: entry.end_line,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;

    use super::{
        MaterializedNode, collect_symbol_seeds, find_nodes_by_name, materialize_node,
        qualified_node_name,
    };

    struct TestNode {
        node_id: NodeId,
    }

    fn abs_path(relative: &str) -> PathBuf {
        PathBuf::from("/materialize-tests").join(relative)
    }

    trait NodeEntryExt {
        fn with_qualified_name_opt(
            self,
            qualified_name: Option<crate::graph::unified::string::id::StringId>,
        ) -> Self;
    }

    impl NodeEntryExt for NodeEntry {
        fn with_qualified_name_opt(
            mut self,
            qualified_name: Option<crate::graph::unified::string::id::StringId>,
        ) -> Self {
            self.qualified_name = qualified_name;
            self
        }
    }

    fn add_node(
        graph: &mut CodeGraph,
        kind: NodeKind,
        name: &str,
        qualified_name: Option<&str>,
        file_path: &Path,
        language: Option<Language>,
        start_line: u32,
        end_line: u32,
    ) -> TestNode {
        let name_id = graph.strings_mut().intern(name).unwrap();
        let qualified_name_id =
            qualified_name.map(|value| graph.strings_mut().intern(value).unwrap());
        let file_id = graph
            .files_mut()
            .register_with_language(file_path, language)
            .unwrap();

        let entry = NodeEntry::new(kind, name_id, file_id)
            .with_qualified_name_opt(qualified_name_id)
            .with_location(start_line, 0, end_line, 0);

        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, kind, name_id, qualified_name_id, file_id);

        TestNode { node_id }
    }

    #[test]
    fn find_nodes_by_name_returns_matching_candidates() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/lib.rs");
        let node = add_node(
            &mut graph,
            NodeKind::Function,
            "process",
            Some("crate::process"),
            &path,
            Some(Language::Rust),
            1,
            10,
        );

        let snapshot = graph.snapshot();
        let results = find_nodes_by_name(&snapshot, "process");
        assert!(
            results.contains(&node.node_id),
            "expected node_id {:?} in results {:?}",
            node.node_id,
            results
        );
    }

    #[test]
    fn find_nodes_by_name_returns_empty_for_nonexistent() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/lib.rs");
        let _node = add_node(
            &mut graph,
            NodeKind::Function,
            "existing",
            Some("crate::existing"),
            &path,
            Some(Language::Rust),
            1,
            5,
        );

        let snapshot = graph.snapshot();
        let results = find_nodes_by_name(&snapshot, "nonexistent_symbol_xyz");
        assert!(
            results.is_empty(),
            "expected empty results, got {results:?}"
        );
    }

    #[test]
    fn collect_symbol_seeds_deduplicates() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/lib.rs");
        let node = add_node(
            &mut graph,
            NodeKind::Function,
            "dedup_target",
            Some("crate::dedup_target"),
            &path,
            Some(Language::Rust),
            1,
            10,
        );

        let snapshot = graph.snapshot();

        // Query the same symbol twice — should deduplicate.
        let symbols = vec!["dedup_target".to_string(), "dedup_target".to_string()];
        let seeds = collect_symbol_seeds(&snapshot, &symbols);

        assert_eq!(
            seeds.iter().filter(|id| **id == node.node_id).count(),
            1,
            "expected exactly one occurrence of node_id {:?}, got seeds {:?}",
            node.node_id,
            seeds
        );
    }

    #[test]
    fn qualified_node_name_returns_language_aware_name() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/Program.cs");
        let node = add_node(
            &mut graph,
            NodeKind::Method,
            "GetName",
            Some("MyApp::User::GetName"),
            &path,
            Some(Language::CSharp),
            5,
            15,
        );

        let snapshot = graph.snapshot();
        let name = qualified_node_name(&snapshot, node.node_id);

        // C# uses `.` as separator, so `::` should be normalized.
        assert_eq!(name, Some("MyApp.User.GetName".to_string()));
    }

    #[test]
    fn materialize_node_produces_complete_node() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/main.rs");
        let node = add_node(
            &mut graph,
            NodeKind::Function,
            "main",
            Some("crate::main"),
            &path,
            Some(Language::Rust),
            1,
            20,
        );

        let snapshot = graph.snapshot();
        let materialized = materialize_node(&snapshot, node.node_id);

        let expected = MaterializedNode {
            node_id: node.node_id,
            name: "main".to_string(),
            qualified_name: "crate::main".to_string(),
            kind: "function".to_string(),
            language: "rust".to_string(),
            file_path: path.display().to_string(),
            start_line: 1,
            end_line: 20,
        };

        assert_eq!(materialized, Some(expected));
    }

    #[test]
    fn materialize_node_returns_none_for_empty_qualified_name() {
        let mut graph = CodeGraph::new();
        let path = abs_path("src/lib.rs");
        // Node with no qualified name and an empty simple name — produces an
        // empty qualified name after resolution, so materialization should skip.
        let node = add_node(
            &mut graph,
            NodeKind::Function,
            "",
            None,
            &path,
            Some(Language::Rust),
            1,
            1,
        );

        let snapshot = graph.snapshot();
        let materialized = materialize_node(&snapshot, node.node_id);
        assert!(
            materialized.is_none(),
            "expected None for node with empty qualified name, got {materialized:?}"
        );
    }
}
