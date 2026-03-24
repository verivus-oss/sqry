//! Graph builder trait.
//!
//! Language integrations implement this trait to populate the unified
//! `CodeGraph` using tree-sitter ASTs.  Builders operate on mutable references
//! to `StagingGraph` buffers, which are per-file and can be built in parallel
//! without synchronisation. After building, staging buffers are merged into
//! the main graph.
//!
//! # Architecture
//!
//! The build pipeline uses a transactional staging pattern:
//! 1. Each file gets its own `StagingGraph` (thread-local)
//! 2. Builders populate the staging buffer via mutable reference
//! 3. After all files are processed, staging buffers are committed atomically
//!
//! This enables parallel builds while maintaining correctness guarantees.

use std::path::Path;

use tree_sitter::{InputEdit, Tree};

use super::edge::CodeEdge;
use super::error::GraphResult;
use super::node::Language;
use super::unified::build::staging::StagingGraph;
use super::unified::concurrent::GraphSnapshot;

/// Trait implemented by all language-specific graph builders.
///
/// #
///
/// This trait now uses the unified graph architecture with `StagingGraph` buffers
/// for transactional builds. The staging pattern enables:
/// - Parallel per-file building (each file gets its own staging buffer)
/// - Atomic commits (all-or-nothing semantics)
/// - Rollback on error (discards partial work)
pub trait GraphBuilder: Send + Sync {
    /// Build graph artifacts for the given file into a staging buffer.
    ///
    /// Builders are expected to walk the supplied `tree` (parsed from `content`)
    /// and insert nodes/edges into `staging`.  Implementations should be idempotent
    /// so repeated calls with the same inputs produce identical results.
    ///
    /// The staging buffer will be committed to the main graph after all files
    /// in a batch are processed successfully.
    ///
    /// # Errors
    ///
    /// Implementations return an error when the AST cannot be traversed or when
    /// extracted metadata violates graph invariants (for example, invalid spans
    /// or malformed identifiers).
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()>;

    /// The language handled by this builder.
    fn language(&self) -> Language;

    /// Incrementally update the graph after an edit.
    ///
    /// The default implementation simply rebuilds the file from scratch.  Builders
    /// that can take advantage of the `edit` can override this method to provide
    /// faster updates.
    ///
    /// # Errors
    ///
    /// Implementations should return an error when incremental updates cannot be
    /// applied safely (e.g., inconsistent edit ranges or graph mutation failures).
    fn update_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        edit: &InputEdit,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let _ = edit;
        self.build_graph(tree, content, file, staging)
    }

    /// Perform any cross-language edge detection that requires whole-graph context.
    ///
    /// Implementations can return additional edges to be merged into the graph
    /// after all files are processed.  The default implementation returns an empty
    /// list, which is suitable for languages that do not emit cross-language edges.
    ///
    /// This method receives an immutable `GraphSnapshot` for read-only access,
    /// enabling cross-file analysis that requires seeing all nodes.
    ///
    /// # Errors
    ///
    /// Return an error when inspecting the graph fails (for example, if required
    /// nodes are missing or metadata cannot be deserialized).
    fn detect_cross_language_edges(&self, _snapshot: &GraphSnapshot) -> GraphResult<Vec<CodeEdge>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tree_sitter::{InputEdit, Parser, Point};

    use crate::graph::node::Language;

    struct TestBuilder {
        language: Language,
        build_calls: Arc<AtomicUsize>,
    }

    impl TestBuilder {
        fn new(language: Language, build_calls: Arc<AtomicUsize>) -> Self {
            Self {
                language,
                build_calls,
            }
        }
    }

    impl GraphBuilder for TestBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            _file: &Path,
            _staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            self.build_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn language(&self) -> Language {
            self.language
        }
    }

    fn parse_rust_tree(source: &str) -> Tree {
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE;
        parser
            .set_language(&language.into())
            .expect("set Rust language");
        parser.parse(source, None).expect("parse rust source")
    }

    fn dummy_edit() -> InputEdit {
        InputEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 0,
            start_position: Point { row: 0, column: 0 },
            old_end_position: Point { row: 0, column: 0 },
            new_end_position: Point { row: 0, column: 0 },
        }
    }

    #[test]
    fn test_update_graph_defaults_to_build_graph() {
        let build_calls = Arc::new(AtomicUsize::new(0));
        let builder = TestBuilder::new(Language::Rust, Arc::clone(&build_calls));
        let tree = parse_rust_tree("fn main() {}");
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("src/main.rs");

        builder
            .update_graph(
                &tree,
                "fn main() {}".as_bytes(),
                &file,
                &dummy_edit(),
                &mut staging,
            )
            .expect("update_graph");

        assert_eq!(build_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_default_cross_language_edges_is_empty() {
        use crate::graph::unified::concurrent::CodeGraph;

        let build_calls = Arc::new(AtomicUsize::new(0));
        let builder = TestBuilder::new(Language::Rust, build_calls);
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let edges = builder
            .detect_cross_language_edges(&snapshot)
            .expect("default detect");
        assert!(edges.is_empty());
    }
}
