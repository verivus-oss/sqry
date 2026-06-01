//! T3 Cluster B (02_DESIGN §4.3.e) — end-to-end Phase 4d-prime wire-through
//! tests for staging `NodeMetadataStore` → live `CodeGraph::macro_metadata`.
//!
//! Uses a synthetic `LanguagePlugin` whose `GraphBuilder::build_graph`
//! stages one `Function` node plus a `MacroNodeMetadata` carrying a
//! `cfg_condition`. The full-build entrypoint runs end-to-end; the test
//! asserts the metadata reached `CodeGraph::macro_metadata` keyed under
//! the canonical arena `NodeId` (not the staging-local `NodeId`).
//!
//! These tests cover:
//! - `staging_macro_metadata_reaches_snapshot_full_build` (full-build plane)
//! - `staging_macro_metadata_propagates_for_multiple_files` (multi-file)

use sqry_core::ast::Scope;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::{
    BuildConfig, GraphBuildHelper, StagingGraph, build_unified_graph,
};
use sqry_core::graph::unified::storage::metadata::{MacroNodeMetadata, NodeMetadataStore};
use sqry_core::graph::unified::{NodeId, NodeKind};
use sqry_core::graph::{GraphBuilder, GraphResult};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin, PluginManager};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tree_sitter::{Parser, Tree};

/// Plugin that emits one `Function` node + one `MacroNodeMetadata` entry
/// keyed under that node's staging-local `NodeId`, for every file it
/// receives. The qualified-name includes the file stem so per-file
/// fixtures with distinct names are distinguishable.
#[derive(Default)]
struct StagedMetadataBuilder;

impl GraphBuilder for StagedMetadataBuilder {
    fn build_graph(
        &self,
        _tree: &Tree,
        _content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("anon")
            .to_string();

        // Use the standard GraphBuildHelper so the node + interning go
        // through the same staging path real plugins use.
        let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
        let qn = format!("test::{stem}");
        let func_id =
            helper.add_function(&qn, None, /*is_async*/ false, /*is_unsafe*/ false);

        // Stage a MacroNodeMetadata under the staging-local NodeId.
        // The Cluster B Phase 4d-prime wire-through (with its
        // staging-local → arena rekey step) must move this metadata onto
        // the corresponding arena NodeId before the publish boundary.
        let mut store = NodeMetadataStore::new();
        let macro_meta = MacroNodeMetadata {
            cfg_condition: Some(format!("cfg_for_{stem}")),
            ..Default::default()
        };
        store.insert(func_id, macro_meta);
        helper.staging_mut().merge_macro_metadata(&store);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

#[derive(Default)]
struct StagedMetadataPlugin {
    builder: StagedMetadataBuilder,
}

impl LanguagePlugin for StagedMetadataPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "test-staged-metadata",
            name: "TestStagedMetadata",
            version: "0.1.0",
            author: "sqry-t3",
            description: "T3 Cluster B Phase 4d-prime wire-through test plugin",
            tree_sitter_version: "0.23",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Distinct extension so this plugin is the only handler.
        &["t3meta"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|e| ParseError::LanguageSetFailed(format!("set test language: {e}")))?;
        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
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
        Some(&self.builder)
    }
}

fn plugin_manager_with_staged_metadata() -> PluginManager {
    let mut pm = PluginManager::new();
    pm.register_builtin(Box::new(StagedMetadataPlugin::default()));
    pm
}

fn find_node_by_qn(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    qn_str: &str,
) -> Option<NodeId> {
    let snap = graph.snapshot();
    for (nid, entry) in snap.nodes().iter() {
        if entry.kind != NodeKind::Function {
            continue;
        }
        let Some(qn_id) = entry.qualified_name else {
            continue;
        };
        let Some(qn) = snap.strings().resolve(qn_id) else {
            continue;
        };
        if qn.as_ref() == qn_str {
            return Some(nid);
        }
    }
    None
}

#[test]
fn staging_macro_metadata_reaches_snapshot_full_build() {
    // One file, one staged metadata entry. Run the full-build entrypoint
    // end-to-end; assert the staged `cfg_condition` reached
    // `CodeGraph::macro_metadata` under the canonical arena NodeId.
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("alpha.t3meta"), "// minimal\n").expect("write fixture file");

    let plugins = plugin_manager_with_staged_metadata();
    let config = BuildConfig::default();
    let graph = build_unified_graph(temp.path(), &plugins, &config)
        .expect("build_unified_graph must succeed");

    let arena_id =
        find_node_by_qn(&graph, "test::alpha").expect("plugin's Function node must be committed");

    // Phase 4d-prime must have moved the staging metadata onto this
    // arena NodeId. Pre-Cluster B (T3 sequence), this entry never
    // reached the graph at all.
    let snap = graph.snapshot();
    let macro_meta = snap
        .macro_metadata()
        .get_macro(arena_id)
        .expect("arena NodeId carries the propagated MacroNodeMetadata");
    assert_eq!(
        macro_meta.cfg_condition.as_deref(),
        Some("cfg_for_alpha"),
        "the staged cfg_condition survives the staging→arena rekey + Phase 4d-prime merge",
    );
}

#[test]
fn staging_macro_metadata_propagates_for_multiple_files() {
    // Multi-file variant: two files each stage their own cfg_condition.
    // Both arena NodeIds must carry their respective values, with no
    // cross-contamination from the chunk-loop accumulation.
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("alpha.t3meta"), "// alpha\n").expect("write alpha");
    fs::write(temp.path().join("beta.t3meta"), "// beta\n").expect("write beta");

    let plugins = plugin_manager_with_staged_metadata();
    let config = BuildConfig::default();
    let graph = build_unified_graph(temp.path(), &plugins, &config)
        .expect("build_unified_graph must succeed");

    let alpha_id = find_node_by_qn(&graph, "test::alpha").expect("alpha committed");
    let beta_id = find_node_by_qn(&graph, "test::beta").expect("beta committed");
    assert_ne!(
        alpha_id, beta_id,
        "two files produce two distinct arena nodes"
    );

    let snap = graph.snapshot();
    let alpha_cfg = snap
        .macro_metadata()
        .get_macro(alpha_id)
        .and_then(|m| m.cfg_condition.clone());
    let beta_cfg = snap
        .macro_metadata()
        .get_macro(beta_id)
        .and_then(|m| m.cfg_condition.clone());
    assert_eq!(alpha_cfg, Some("cfg_for_alpha".to_string()));
    assert_eq!(beta_cfg, Some("cfg_for_beta".to_string()));
}
