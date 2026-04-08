//! Macro and proc-macro boundary analysis for the Rust language plugin.
//!
//! This module implements Section 2 of the macro/proc-macro boundaries design,
//! providing 6 sub-analyzers that detect, classify, and track macro boundaries
//! in Rust source code.
//!
//! # Sub-Analyzers
//!
//! | Module | ID | Runs In | Purpose |
//! |---|---|---|---|
//! | [`attribute_macros`] | 4.5a | `build_graph()` | Detect attribute macros on items |
//! | [`metavariables`] | 4.5b | `build_graph()` | Extract metavariables from `macro_rules!` |
//! | [`proc_macro_classify`] | 4.5c | `build_graph()` | Classify proc-macro functions |
//! | [`cross_crate_macros`] | 4.5d | `entrypoint.rs` | Cross-crate macro resolution |
//! | [`cfg_analysis`] | 4.5e | `build_graph()` | Parse `cfg`/`cfg_attr` predicates |
//! | [`expand_cache`] | 4.5f | CLI-triggered | Expansion cache storage |
//! | [`generated_symbols`] | 4.5f | CLI-triggered | Expansion diffing |
//!
//! # Integration Points
//!
//! - **`build_graph()`**: Call [`analyze_macro_boundaries_in_build_graph`] for each
//!   file during AST analysis. This dispatches to 4.5a, 4.5b, 4.5c, and 4.5e.
//!
//! - **`entrypoint.rs`**: Call [`cross_crate_macros::resolve_cross_crate_macros`]
//!   between Pass 4 and Pass 5 for cross-crate macro resolution (4.5d).
//!
//! - **CLI**: Use [`expand_cache`] and [`generated_symbols`] for `sqry cache expand`.

pub mod attribute_macros;
pub mod cfg_analysis;
pub mod cross_crate_macros;
pub mod expand_cache;
pub mod generated_symbols;
pub mod metavariables;
pub mod proc_macro_classify;

use sqry_core::graph::unified::{GraphBuildHelper, NodeMetadataStore};
use tree_sitter::{Node, Tree};

/// Configuration for macro boundary analysis.
///
/// Controls which sub-analyzers are enabled and provides configuration for
/// cfg evaluation. All features are enabled by default.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Each bool controls a distinct sub-analyzer; a state machine would obscure this intent
pub struct MacroBoundaryConfig {
    /// Enable attribute macro detection (4.5a). Default: `true`.
    pub enable_attribute_macros: bool,
    /// Enable metavariable extraction from `macro_rules!` (4.5b). Default: `true`.
    pub enable_scope_analysis: bool,
    /// Enable proc-macro function classification (4.5c). Default: `true`.
    pub enable_proc_macro_io: bool,
    /// Enable cross-crate macro resolution (4.5d). Default: `true`.
    /// Note: 4.5d runs in `entrypoint.rs`, not in `build_graph()`.
    pub enable_cross_crate: bool,
    /// Enable `cfg`/`cfg_attr` analysis (4.5e). Default: `true`.
    pub enable_cfg_analysis: bool,
    /// Enable macro-generated symbol extraction (4.5f). Default: `true`.
    /// Note: 4.5f is CLI-triggered, not in `build_graph()`.
    pub enable_generated_symbols: bool,
    /// Active Cargo features for cfg evaluation (e.g., `["serde", "json"]`).
    /// Empty means cfg evaluation returns `None` (unknown).
    pub active_features: Vec<String>,
    /// Active cfg flags for cfg evaluation (e.g., `["unix", "test"]`).
    /// Empty means cfg evaluation returns `None` (unknown).
    pub active_cfg_flags: Vec<String>,
}

impl Default for MacroBoundaryConfig {
    fn default() -> Self {
        Self {
            enable_attribute_macros: true,
            enable_scope_analysis: true,
            enable_proc_macro_io: true,
            enable_cross_crate: true,
            enable_cfg_analysis: true,
            enable_generated_symbols: true,
            active_features: Vec::new(),
            active_cfg_flags: Vec::new(),
        }
    }
}

impl MacroBoundaryConfig {
    /// Create a configuration with all features disabled.
    ///
    /// Useful for tests or when macro boundary analysis is not needed.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enable_attribute_macros: false,
            enable_scope_analysis: false,
            enable_proc_macro_io: false,
            enable_cross_crate: false,
            enable_cfg_analysis: false,
            enable_generated_symbols: false,
            active_features: Vec::new(),
            active_cfg_flags: Vec::new(),
        }
    }

    /// Returns true if any in-build_graph analyzer is enabled.
    #[must_use]
    pub fn any_build_graph_analyzer_enabled(&self) -> bool {
        self.enable_attribute_macros
            || self.enable_scope_analysis
            || self.enable_proc_macro_io
            || self.enable_cfg_analysis
    }
}

/// Run all in-`build_graph()` macro boundary analyzers on the parsed AST.
///
/// This is the main entry point for macro boundary analysis during graph building.
/// It dispatches to the 4 sub-analyzers that run inside `build_graph()`:
///
/// - **4.5a** Attribute macro detection on all attributable items
/// - **4.5b** Metavariable extraction from `macro_rules!` definitions
/// - **4.5c** Proc-macro function classification
/// - **4.5e** `cfg/cfg_attr` analysis
///
/// Sub-analyzers 4.5d (cross-crate) and 4.5f (expand cache) run outside
/// `build_graph()` and are not called here.
///
/// # Arguments
///
/// * `tree` — parsed tree-sitter tree for the file
/// * `content` — source file bytes
/// * `helper` — graph build helper with nodes already created by the main builder
/// * `metadata_store` — sparse metadata store for macro-specific metadata
/// * `config` — configuration controlling which analyzers are enabled
/// * `node_map` — mapping from tree-sitter node IDs to graph `NodeId`s, provided
///   by the caller so we can associate AST nodes with their graph nodes
pub fn analyze_macro_boundaries_in_build_graph<S: std::hash::BuildHasher>(
    tree: &Tree,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
    config: &MacroBoundaryConfig,
    node_map: &std::collections::HashMap<usize, (sqry_core::graph::unified::NodeId, String), S>,
) {
    if !config.any_build_graph_analyzer_enabled() {
        return;
    }

    // Walk the entire AST and dispatch to the appropriate analyzers based on
    // the node kind.
    walk_and_analyze(
        tree.root_node(),
        content,
        helper,
        metadata_store,
        config,
        node_map,
    );
}

/// Recursively walk the AST and run enabled analyzers on relevant nodes.
fn walk_and_analyze<S: std::hash::BuildHasher>(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
    config: &MacroBoundaryConfig,
    node_map: &std::collections::HashMap<usize, (sqry_core::graph::unified::NodeId, String), S>,
) {
    let kind = node.kind();

    // 4.5a: Attribute macro detection on attributable items.
    if config.enable_attribute_macros
        && is_attributable_item(kind)
        && let Some(&(item_id, ref item_qualified)) = node_map.get(&node.id())
    {
        attribute_macros::detect_attribute_macros(
            node,
            content,
            item_qualified,
            item_id,
            helper,
            metadata_store,
        );
    }

    // 4.5b: Metavariable extraction from macro_rules! definitions.
    if config.enable_scope_analysis
        && kind == "macro_definition"
        && let Some(&(macro_id, ref macro_qualified)) = node_map.get(&node.id())
    {
        metavariables::extract_metavariables(node, content, macro_qualified, macro_id, helper);
    }

    // 4.5c: Proc-macro function classification.
    if config.enable_proc_macro_io
        && kind == "function_item"
        && let Some(&(func_id, ref _func_qualified)) = node_map.get(&node.id())
    {
        proc_macro_classify::classify_proc_macro(node, content, func_id, metadata_store);
    }

    // 4.5e: cfg/cfg_attr analysis on all attributable items.
    if config.enable_cfg_analysis
        && is_attributable_item(kind)
        && let Some(&(item_id, ref _item_qualified)) = node_map.get(&node.id())
    {
        cfg_analysis::analyze_cfg_attributes(
            node,
            content,
            item_id,
            helper,
            metadata_store,
            &config.active_cfg_flags,
            &config.active_features,
        );
    }

    // Recurse into children.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_analyze(child, content, helper, metadata_store, config, node_map);
    }
}

/// Check if a tree-sitter node kind represents an item that can have attributes.
fn is_attributable_item(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "impl_item"
            | "mod_item"
            | "trait_item"
            | "type_item"
            | "union_item"
            | "const_item"
            | "static_item"
            | "use_declaration"
            | "extern_crate_declaration"
            | "foreign_mod_item"
            | "let_declaration"
            | "expression_statement"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default_all_enabled() {
        let config = MacroBoundaryConfig::default();
        assert!(config.enable_attribute_macros);
        assert!(config.enable_scope_analysis);
        assert!(config.enable_proc_macro_io);
        assert!(config.enable_cross_crate);
        assert!(config.enable_cfg_analysis);
        assert!(config.enable_generated_symbols);
        assert!(config.active_features.is_empty());
        assert!(config.active_cfg_flags.is_empty());
        assert!(config.any_build_graph_analyzer_enabled());
    }

    #[test]
    fn test_config_disabled_none_enabled() {
        let config = MacroBoundaryConfig::disabled();
        assert!(!config.enable_attribute_macros);
        assert!(!config.enable_scope_analysis);
        assert!(!config.enable_proc_macro_io);
        assert!(!config.enable_cross_crate);
        assert!(!config.enable_cfg_analysis);
        assert!(!config.enable_generated_symbols);
        assert!(!config.any_build_graph_analyzer_enabled());
    }

    #[test]
    fn test_is_attributable_item() {
        assert!(is_attributable_item("function_item"));
        assert!(is_attributable_item("struct_item"));
        assert!(is_attributable_item("enum_item"));
        assert!(is_attributable_item("impl_item"));
        assert!(is_attributable_item("mod_item"));
        assert!(is_attributable_item("trait_item"));
        assert!(is_attributable_item("type_item"));
        assert!(is_attributable_item("union_item"));
        assert!(is_attributable_item("const_item"));
        assert!(is_attributable_item("static_item"));
        assert!(is_attributable_item("use_declaration"));
        assert!(is_attributable_item("extern_crate_declaration"));
        assert!(is_attributable_item("foreign_mod_item"));
        assert!(is_attributable_item("let_declaration"));
        assert!(is_attributable_item("expression_statement"));

        assert!(!is_attributable_item("identifier"));
        assert!(!is_attributable_item("source_file"));
        assert!(!is_attributable_item("attribute_item"));
    }

    #[test]
    fn test_analyze_with_disabled_config_is_noop() {
        use sqry_core::graph::Language;
        use sqry_core::graph::unified::StagingGraph;
        use std::path::Path;
        use tree_sitter::Parser;

        let source = r"
#[tokio::main]
async fn main() {}
";
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();

        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();
        let config = MacroBoundaryConfig::disabled();
        let node_map = std::collections::HashMap::new();

        analyze_macro_boundaries_in_build_graph(
            &tree,
            source.as_bytes(),
            &mut helper,
            &mut metadata_store,
            &config,
            &node_map,
        );

        // With all analyzers disabled, no metadata should be created.
        assert!(metadata_store.is_empty());
    }
}
