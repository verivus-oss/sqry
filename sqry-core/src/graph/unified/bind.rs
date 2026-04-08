//! Binding query facade with declaration/reference classification.
//!
//! Provides `BindingQuery`, a builder-pattern API for resolving symbols and
//! classifying them as declarations, references, imports, or ambiguous. This
//! module builds on the witness-bearing resolution API in `resolution.rs`.

use super::concurrent::GraphSnapshot;
use super::edge::kind::{EdgeKind, ExportKind};
use super::node::id::NodeId;
use super::node::kind::NodeKind;
use super::resolution::{
    FileScope, NormalizedSymbolQuery, ResolutionMode, SymbolCandidateBucket,
    SymbolCandidateOutcome, SymbolQuery, SymbolResolutionOutcome,
};

/// How a resolved symbol relates to its definition site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolClassification {
    /// The node IS the declaration (target of `Defines`/`Contains` edges,
    /// `NodeKind` is not `CallSite`/`Import`/`Export`).
    Declaration,
    /// The node is a reference/use of a declaration elsewhere.
    Reference,
    /// The node is an import statement (has `Defines` edge but is not
    /// the source-of-truth declaration).
    Import,
    /// Multiple interpretations possible (e.g., re-export that is both
    /// a reference and a local declaration).
    Ambiguous,
    /// Classification could not be determined from graph structure.
    Unknown,
}

/// A single resolved binding with classification and provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinding {
    /// The resolved node ID.
    pub node_id: NodeId,
    /// Declaration vs reference vs import classification.
    pub classification: SymbolClassification,
    /// Which resolution bucket produced this candidate.
    pub bucket: SymbolCandidateBucket,
    /// The node's kind (saves consumers a `get_node()` round-trip).
    pub kind: NodeKind,
}

/// Result of a binding query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingResult {
    /// Normalized query (`None` if file scope resolution failed).
    pub query: Option<NormalizedSymbolQuery>,
    /// Resolved bindings with classification and provenance.
    pub bindings: Vec<ResolvedBinding>,
    /// Resolution outcome (same semantics as `resolve_symbol`).
    pub outcome: SymbolResolutionOutcome,
}

/// Builder for binding queries.
///
/// # Example
///
/// ```rust,ignore
/// let result = BindingQuery::new("MyClass")
///     .file_scope(FileScope::Any)
///     .mode(ResolutionMode::AllowSuffixCandidates)
///     .resolve(&snapshot);
/// ```
pub struct BindingQuery<'a> {
    symbol: &'a str,
    file_scope: FileScope<'a>,
    mode: ResolutionMode,
}

impl<'a> BindingQuery<'a> {
    /// Creates a new binding query for the given symbol.
    ///
    /// Defaults to `FileScope::Any` and `ResolutionMode::AllowSuffixCandidates`.
    #[must_use]
    pub fn new(symbol: &'a str) -> Self {
        Self {
            symbol,
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }
    }

    /// Restricts the query to a specific file scope.
    #[must_use]
    pub fn file_scope(mut self, scope: FileScope<'a>) -> Self {
        self.file_scope = scope;
        self
    }

    /// Sets the resolution mode.
    #[must_use]
    pub fn mode(mut self, mode: ResolutionMode) -> Self {
        self.mode = mode;
        self
    }

    /// Resolves the query against the given snapshot.
    ///
    /// Performs symbol resolution via the witness-bearing API, then classifies
    /// each candidate based on its `NodeKind` and incoming edge structure.
    #[must_use]
    pub fn resolve(self, snapshot: &GraphSnapshot) -> BindingResult {
        let witness = snapshot.find_symbol_candidates_with_witness(&SymbolQuery {
            symbol: self.symbol,
            file_scope: self.file_scope,
            mode: self.mode,
        });

        // Determine outcome
        let outcome = match &witness.outcome {
            SymbolCandidateOutcome::Candidates(candidates) => match candidates.as_slice() {
                [] => SymbolResolutionOutcome::NotFound,
                [node_id] => SymbolResolutionOutcome::Resolved(*node_id),
                _ => SymbolResolutionOutcome::Ambiguous(candidates.clone()),
            },
            SymbolCandidateOutcome::NotFound => SymbolResolutionOutcome::NotFound,
            SymbolCandidateOutcome::FileNotIndexed => SymbolResolutionOutcome::FileNotIndexed,
        };

        // Build bindings with classification
        let bindings: Vec<ResolvedBinding> = witness
            .candidates
            .iter()
            .filter_map(|candidate| {
                let entry = snapshot.get_node(candidate.node_id)?;
                let classification = classify_node(snapshot, candidate.node_id, entry.kind);
                Some(ResolvedBinding {
                    node_id: candidate.node_id,
                    classification,
                    bucket: candidate.bucket,
                    kind: entry.kind,
                })
            })
            .collect();

        BindingResult {
            query: witness.normalized_query,
            bindings,
            outcome,
        }
    }
}

/// Classify a node as declaration, reference, import, or ambiguous.
///
/// Classification logic:
/// 1. `NodeKind::Import` → `Import`
/// 2. `NodeKind::Export` → check for re-export edge → `Ambiguous` if re-export, else `Import`
/// 3. `NodeKind::CallSite` → `Reference`
/// 4. Has incoming `Defines` or `Contains` edge → `Declaration`
/// 5. No structural incoming edges → `Reference`
/// 6. Fallback → `Unknown`
fn classify_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    kind: NodeKind,
) -> SymbolClassification {
    // Step 1: Check NodeKind first
    if kind == NodeKind::Import {
        return SymbolClassification::Import;
    }

    if kind == NodeKind::Export {
        // Check if this export has a re-export edge
        let incoming = snapshot.edges().edges_to(node_id);
        let has_reexport = incoming.iter().any(|e| {
            matches!(
                &e.kind,
                EdgeKind::Exports {
                    kind: ExportKind::Reexport | ExportKind::Namespace,
                    ..
                }
            )
        });
        // Also check outgoing exports for re-export classification
        let outgoing = snapshot.edges().edges_from(node_id);
        let has_reexport_outgoing = outgoing.iter().any(|e| {
            matches!(
                &e.kind,
                EdgeKind::Exports {
                    kind: ExportKind::Reexport | ExportKind::Namespace,
                    ..
                }
            )
        });

        return if has_reexport || has_reexport_outgoing {
            SymbolClassification::Ambiguous
        } else {
            SymbolClassification::Import
        };
    }

    if kind == NodeKind::CallSite {
        return SymbolClassification::Reference;
    }

    // Step 2: Check incoming edges for structural (Defines/Contains)
    let incoming = snapshot.edges().edges_to(node_id);
    let has_structural = incoming
        .iter()
        .any(|e| matches!(&e.kind, EdgeKind::Defines | EdgeKind::Contains));

    if has_structural {
        return SymbolClassification::Declaration;
    }

    // Step 3: No structural incoming edges → Reference
    if !incoming.is_empty() {
        return SymbolClassification::Reference;
    }

    // Step 4: No incoming edges at all — could be a root declaration or orphan
    // If the node has outgoing Defines/Contains edges, it's likely a root module/declaration
    let outgoing = snapshot.edges().edges_from(node_id);
    let has_outgoing_structural = outgoing
        .iter()
        .any(|e| matches!(&e.kind, EdgeKind::Defines | EdgeKind::Contains));

    if has_outgoing_structural {
        return SymbolClassification::Declaration;
    }

    SymbolClassification::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::storage::arena::NodeEntry;

    struct TestGraph {
        graph: CodeGraph,
        file_id: Option<FileId>,
    }

    impl TestGraph {
        fn new() -> Self {
            Self {
                graph: CodeGraph::new(),
                file_id: None,
            }
        }

        fn ensure_file_id(&mut self) -> FileId {
            if let Some(fid) = self.file_id {
                return fid;
            }
            let file_path = std::path::PathBuf::from("/bind-tests/test.rs");
            let fid = self
                .graph
                .files_mut()
                .register_with_language(&file_path, Some(Language::Rust))
                .unwrap();
            self.file_id = Some(fid);
            fid
        }

        fn add_node(&mut self, name: &str, kind: NodeKind) -> NodeId {
            let file_id = self.ensure_file_id();
            let name_id = self.graph.strings_mut().intern(name).unwrap();
            let qn_id = self
                .graph
                .strings_mut()
                .intern(&format!("test::{name}"))
                .unwrap();

            let entry = NodeEntry::new(kind, name_id, file_id)
                .with_qualified_name(qn_id)
                .with_location(1, 0, 10, 0);

            let node_id = self.graph.nodes_mut().alloc(entry).unwrap();
            self.graph
                .indices_mut()
                .add(node_id, kind, name_id, Some(qn_id), file_id);
            node_id
        }

        fn add_edge(&mut self, source: NodeId, target: NodeId, kind: EdgeKind) {
            let file_id = self.ensure_file_id();
            self.graph
                .edges_mut()
                .add_edge(source, target, kind, file_id);
        }

        fn snapshot(&self) -> GraphSnapshot {
            self.graph.snapshot()
        }
    }

    #[test]
    fn declaration_classification() {
        let mut tg = TestGraph::new();
        let module_node = tg.add_node("my_module", NodeKind::Module);
        let func_node = tg.add_node("my_func", NodeKind::Function);
        tg.add_edge(module_node, func_node, EdgeKind::Defines);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("my_func").resolve(&snapshot);

        assert!(!result.bindings.is_empty(), "expected at least one binding");
        let binding = result
            .bindings
            .iter()
            .find(|b| b.node_id == func_node)
            .expect("expected binding for func_node");
        assert_eq!(binding.classification, SymbolClassification::Declaration);
        assert_eq!(binding.kind, NodeKind::Function);
    }

    #[test]
    fn reference_classification_callsite() {
        let mut tg = TestGraph::new();
        let _call_node = tg.add_node("some_call", NodeKind::CallSite);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("some_call").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        assert_eq!(
            result.bindings[0].classification,
            SymbolClassification::Reference
        );
    }

    #[test]
    fn import_classification() {
        let mut tg = TestGraph::new();
        let _import_node = tg.add_node("imported_sym", NodeKind::Import);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("imported_sym").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        assert_eq!(
            result.bindings[0].classification,
            SymbolClassification::Import
        );
    }

    #[test]
    fn export_direct_classification() {
        let mut tg = TestGraph::new();
        let _export_node = tg.add_node("exported_sym", NodeKind::Export);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("exported_sym").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        // Direct export without re-export edges → Import classification
        assert_eq!(
            result.bindings[0].classification,
            SymbolClassification::Import
        );
    }

    #[test]
    fn export_reexport_ambiguous() {
        let mut tg = TestGraph::new();
        let source = tg.add_node("source_mod", NodeKind::Module);
        let export_node = tg.add_node("reexported", NodeKind::Export);
        tg.add_edge(
            source,
            export_node,
            EdgeKind::Exports {
                kind: ExportKind::Reexport,
                alias: None,
            },
        );

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("reexported").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        let binding = result
            .bindings
            .iter()
            .find(|b| b.node_id == export_node)
            .expect("expected binding for export_node");
        assert_eq!(binding.classification, SymbolClassification::Ambiguous);
    }

    #[test]
    fn builder_defaults() {
        let query = BindingQuery::new("test_sym");
        assert_eq!(query.symbol, "test_sym");
        assert_eq!(query.file_scope, FileScope::Any);
        assert_eq!(query.mode, ResolutionMode::AllowSuffixCandidates);
    }

    #[test]
    fn not_found_result() {
        let tg = TestGraph::new();
        let snapshot = tg.snapshot();
        let result = BindingQuery::new("nonexistent_symbol_xyz").resolve(&snapshot);

        assert!(result.bindings.is_empty());
        assert_eq!(result.outcome, SymbolResolutionOutcome::NotFound);
    }

    #[test]
    fn declaration_via_contains_edge() {
        let mut tg = TestGraph::new();
        let class_node = tg.add_node("MyClass", NodeKind::Class);
        let method_node = tg.add_node("my_method", NodeKind::Method);
        tg.add_edge(class_node, method_node, EdgeKind::Contains);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("my_method").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        let binding = result
            .bindings
            .iter()
            .find(|b| b.node_id == method_node)
            .expect("expected binding for method_node");
        assert_eq!(binding.classification, SymbolClassification::Declaration);
    }

    #[test]
    fn root_declaration_with_outgoing_defines() {
        let mut tg = TestGraph::new();
        let module_node = tg.add_node("root_mod", NodeKind::Module);
        let child = tg.add_node("child_func", NodeKind::Function);
        tg.add_edge(module_node, child, EdgeKind::Defines);

        let snapshot = tg.snapshot();
        let result = BindingQuery::new("root_mod").resolve(&snapshot);

        assert!(!result.bindings.is_empty());
        let binding = result
            .bindings
            .iter()
            .find(|b| b.node_id == module_node)
            .expect("expected binding for module_node");
        // Root module has no incoming edges but has outgoing Defines → Declaration
        assert_eq!(binding.classification, SymbolClassification::Declaration);
    }
}
