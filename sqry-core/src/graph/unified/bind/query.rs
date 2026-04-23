//! `BindingQuery` builder — moved out of the old single-file `bind.rs` into
//! its own file so the new `bind/` module layout can coexist with later P2U
//! units that split the facade and the query bridge.
//!
//! P2U01 moves the struct and its `impl<'a>` block verbatim — no logic
//! change. P2U07 rewires `resolve()` to delegate through the private
//! `resolve_shared()` helper in `bind/plane.rs`, which is also called by
//! `BindingPlane::resolve()`. This is the byte-equality refactor; the T19
//! gate in `phase2_binding_plane_in_memory.rs` proves the output is
//! identical to the pre-P2U07 snapshot captured before this change.
//!
//! P2U08 adds [`BindingQuery::resolve_with_witness`] — an opt-in upgrade path
//! that returns the full [`super::plane::BindingResolution`] (result + witness
//! step trace) without disturbing the existing [`BindingQuery::resolve`] API.

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::resolution::{FileScope, ResolutionMode, SymbolQuery};

use super::BindingResult;
use super::plane::BindingResolution;

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
    /// Delegates to [`super::plane::resolve_shared`] which is the shared
    /// implementation core used by both `BindingQuery::resolve()` and
    /// `BindingPlane::resolve()`. Returns only the `BindingResult` (without
    /// the witness step trace) for backward compatibility with existing
    /// callers.
    ///
    /// The byte-equality T19 gate in `phase2_binding_plane_in_memory.rs`
    /// asserts that the output of this method is identical to the pre-P2U07
    /// snapshot captured in `test-fixtures/phase2_binding_result_snapshots/`.
    #[must_use]
    pub fn resolve(self, snapshot: &GraphSnapshot) -> BindingResult {
        let query = SymbolQuery {
            symbol: self.symbol,
            file_scope: self.file_scope,
            mode: self.mode,
        };
        super::plane::resolve_shared(&query, snapshot).result
    }

    /// Opt-in upgrade path — resolves the query and returns the full
    /// [`BindingResolution`] containing both the [`BindingResult`] and the
    /// ordered witness step trace.
    ///
    /// Use this method when you need access to the resolution witness (e.g.,
    /// for CLI `--explain` output, rule-layer consumers, or diagnostic tooling
    /// introduced in Phase 2). Callers that only need the binding outcome
    /// should continue to use [`BindingQuery::resolve`] which returns the
    /// leaner [`BindingResult`] directly.
    ///
    /// The `result` field of the returned [`BindingResolution`] is byte-equal
    /// to the value that [`BindingQuery::resolve`] would return for the same
    /// query — both delegate to the same [`super::plane::resolve_shared`] core.
    #[must_use]
    pub fn resolve_with_witness(self, snapshot: &GraphSnapshot) -> BindingResolution {
        let query = SymbolQuery {
            symbol: self.symbol,
            file_scope: self.file_scope,
            mode: self.mode,
        };
        super::plane::resolve_shared(&query, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::kind::EdgeKind;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;

    #[test]
    fn builder_defaults() {
        let query = BindingQuery::new("test_sym");
        assert_eq!(query.symbol, "test_sym");
        assert_eq!(query.file_scope, FileScope::Any);
        assert_eq!(query.mode, ResolutionMode::AllowSuffixCandidates);
    }

    /// Builds a minimal graph containing a single named function under a root module.
    fn make_graph_with_function(sym: &str) -> CodeGraph {
        let mut graph = CodeGraph::new();
        let path = std::path::PathBuf::from("/query-tests/test.rs");
        let file_id = graph
            .files_mut()
            .register_with_language(&path, Some(Language::Rust))
            .expect("register file");
        let name = graph.strings_mut().intern(sym).expect("intern sym");
        let qn = graph
            .strings_mut()
            .intern(&format!("crate::{sym}"))
            .expect("intern qn");
        let mod_name = graph.strings_mut().intern("root").expect("intern root");
        let mod_qn = graph.strings_mut().intern("crate").expect("intern crate");
        let mod_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, mod_name, file_id)
                    .with_qualified_name(mod_qn)
                    .with_byte_range(0, 100),
            )
            .expect("alloc mod");
        graph
            .indices_mut()
            .add(mod_id, NodeKind::Module, mod_name, Some(mod_qn), file_id);
        let fn_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, name, file_id)
                    .with_qualified_name(qn)
                    .with_byte_range(5, 80),
            )
            .expect("alloc fn");
        graph
            .indices_mut()
            .add(fn_id, NodeKind::Function, name, Some(qn), file_id);
        graph
            .edges_mut()
            .add_edge(mod_id, fn_id, EdgeKind::Contains, file_id);
        graph
    }

    /// T19-companion: `resolve_with_witness().result` must be byte-equal to
    /// `resolve()` for the same query parameters.
    #[test]
    fn resolve_with_witness_result_matches_resolve() {
        let graph = make_graph_with_function("witness_fn");
        let snapshot = graph.snapshot();

        let result_only = BindingQuery::new("witness_fn")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve(&snapshot);

        let with_witness = BindingQuery::new("witness_fn")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve_with_witness(&snapshot);

        assert_eq!(
            result_only, with_witness.result,
            "resolve_with_witness().result must be byte-equal to resolve()"
        );
    }

    /// `resolve_with_witness()` must carry a non-empty step trace when the
    /// symbol is found — proving the witness is populated, not empty.
    #[test]
    fn resolve_with_witness_has_non_empty_steps_on_found() {
        let graph = make_graph_with_function("stepped_fn");
        let snapshot = graph.snapshot();

        let resolution = BindingQuery::new("stepped_fn")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve_with_witness(&snapshot);

        assert!(
            !resolution.witness.steps.is_empty(),
            "witness step trace must be non-empty for a resolved symbol"
        );
    }

    /// `resolve_with_witness()` for a missing symbol must be consistent:
    /// `result` matches `resolve()` and the witness step trace is non-empty.
    #[test]
    fn resolve_with_witness_not_found_consistent_with_resolve() {
        let graph = make_graph_with_function("any_fn");
        let snapshot = graph.snapshot();

        let result_only = BindingQuery::new("does_not_exist")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve(&snapshot);

        let with_witness = BindingQuery::new("does_not_exist")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve_with_witness(&snapshot);

        assert_eq!(
            result_only, with_witness.result,
            "resolve_with_witness().result must match resolve() for missing symbols"
        );
        assert!(
            !with_witness.witness.steps.is_empty(),
            "witness step trace must be non-empty even for unresolved symbols"
        );
    }
}
