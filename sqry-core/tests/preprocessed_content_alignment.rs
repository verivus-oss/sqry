use std::borrow::Cow;
use std::fs;
use std::path::Path;

use sqry_core::ast::Scope;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::{BuildConfig, GraphBuildHelper, build_unified_graph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language};
use sqry_core::plugin::error::ScopeError;
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin, PluginManager};
use tempfile::TempDir;
use tree_sitter::Tree;

static PREPARED_RUST_SOURCE: &[u8] = b"fn prepared_from_preprocess() {}\n";

struct PreparedContentPlugin;

impl LanguagePlugin for PreparedContentPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "prepared-rust",
            name: "Prepared Rust",
            version: "test",
            author: "sqry-core tests",
            description: "test plugin for preprocessed content alignment",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn preprocess<'a>(&self, _content: &'a [u8]) -> Cow<'a, [u8]> {
        Cow::Borrowed(PREPARED_RUST_SOURCE)
    }

    fn extract_scopes(
        &self,
        _tree: &Tree,
        _content: &[u8],
        _file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        Ok(Vec::new())
    }

    fn graph_builder(&self) -> Option<&dyn GraphBuilder> {
        Some(self)
    }
}

impl GraphBuilder for PreparedContentPlugin {
    fn build_graph(
        &self,
        _tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        if content != PREPARED_RUST_SOURCE {
            return Err(GraphBuilderError::CrossLanguageError {
                reason: "builder did not receive preprocessed content".to_string(),
            });
        }

        let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
        helper.add_function("prepared_from_preprocess", None, false, false);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

struct PlainRustPlugin;

impl LanguagePlugin for PlainRustPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "plain-rust",
            name: "Plain Rust",
            version: "test",
            author: "sqry-core tests",
            description: "test plugin for default non-preprocessing path",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn extract_scopes(
        &self,
        _tree: &Tree,
        _content: &[u8],
        _file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        Ok(Vec::new())
    }

    fn graph_builder(&self) -> Option<&dyn GraphBuilder> {
        Some(self)
    }
}

impl GraphBuilder for PlainRustPlugin {
    fn build_graph(
        &self,
        _tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        if content != b"fn untouched() {}\n" {
            return Err(GraphBuilderError::CrossLanguageError {
                reason: "default path unexpectedly changed source bytes".to_string(),
            });
        }

        let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
        helper.add_function("untouched", None, false, false);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

#[test]
fn unified_graph_uses_preprocessed_bytes_for_parse_and_build() {
    let temp_dir = TempDir::new().expect("temp dir");
    let file_path = temp_dir.path().join("broken_raw.rs");
    fs::write(&file_path, "<<< definitely not rust >>>").expect("write raw fixture");

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(PreparedContentPlugin));

    let graph = build_unified_graph(temp_dir.path(), &plugins, &BuildConfig::default())
        .expect("build graph with preprocessed content");
    assert!(graph.node_count() > 0, "expected at least one staged node");
}

#[test]
fn unified_graph_preserves_default_non_preprocessed_plugins() {
    let temp_dir = TempDir::new().expect("temp dir");
    let file_path = temp_dir.path().join("plain.rs");
    fs::write(&file_path, "fn untouched() {}\n").expect("write raw fixture");

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(PlainRustPlugin));

    let graph = build_unified_graph(temp_dir.path(), &plugins, &BuildConfig::default())
        .expect("build graph without preprocessing");
    assert!(graph.node_count() > 0, "expected at least one staged node");
}
