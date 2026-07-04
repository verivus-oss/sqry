use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{
    FfiConvention, GraphBuildHelper, LifetimeConstraintKind, MacroExpansionKind, NodeId, NodeKind,
    StagingGraph, build::helper::CalleeKindHint, edge::kind::TypeOfContext,
};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use sqry_core::relations::SyntheticNameBuilder;
use tree_sitter::{Node, Tree};

use crate::confidence::{ConfidenceLevel, ConfidenceTracker};
use crate::lifetime_extractor::{LifetimeExtractionResult, LifetimeExtractor};
use crate::module_resolver::ModuleResolver;
use crate::proc_macro_detector::ProcMacroDetector;
use crate::trait_binder::{BindingResult, TraitMethodBinder};

use super::local_scopes;

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// Synthetic caller name for top-level code (outside any function).
/// Distinct from `FILE_MODULE_NAME` to avoid node kind collision in `GraphBuildHelper` cache.
const TOPLEVEL_CALLER_NAME: &str = "<toplevel>";

#[derive(Debug)]
struct BuiltCall {
    caller_qualified: String,
    callee_qualified: String,
    span: Span,
    has_turbofish: bool,
}

/// Registry of FFI declarations discovered during graph building.
///
/// Maps simple function names (e.g., `printf`) to their qualified FFI name
/// (e.g., `extern::C::printf`) and calling convention. This allows call edge
/// construction to detect when a call targets an FFI function and create
/// `FfiCall` edges instead of regular `Call` edges.
type FfiRegistry = HashMap<String, (String, FfiConvention)>;

/// Configuration for Rust-specific graph building features.
///
/// Controls which P3 features are enabled during graph construction.
///
/// # Default Mode (Full Features)
///
/// By default, all features are enabled for maximum analysis accuracy:
/// - Macro expansion (derive macros, function-like macros)
/// - Trait method binding resolution
/// - Lifetime constraint extraction
///
/// # Safe Mode
///
/// For environments where macro expansion is restricted,
/// use [`RustGraphConfig::safe_mode()`] which disables:
/// - Macro expansion (avoids executing build scripts)
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent feature toggles keep graph config explicit"
)]
#[derive(Debug, Clone, Default)]
pub struct RustGraphConfig {
    /// Enable macro expansion (requires `cargo expand`). Default: true.
    /// Set to false in safe mode to avoid executing build scripts and proc macros.
    pub enable_macro_expansion: bool,
    /// Enable trait method binding resolution. Default: true.
    pub enable_trait_binding: bool,
    /// Enable lifetime constraint extraction. Default: true.
    pub enable_lifetime_extraction: bool,
    /// Workspace root for proc-macro detection and macro expansion.
    pub workspace_root: Option<std::path::PathBuf>,
}

impl RustGraphConfig {
    /// Create a new configuration with all features enabled (full mode).
    ///
    /// This is the recommended configuration for maximum analysis accuracy.
    /// All P3 features are enabled:
    /// - Macro expansion
    /// - Trait method binding
    /// - Lifetime extraction
    #[must_use]
    pub fn new() -> Self {
        Self {
            enable_macro_expansion: true,
            enable_trait_binding: true,
            enable_lifetime_extraction: true,
            workspace_root: None,
        }
    }

    /// Create a safe mode configuration with restricted features.
    ///
    /// Use this in environments where:
    /// - Macro expansion is not safe (untrusted code)
    /// - Minimal resource usage is required
    ///
    /// Enabled features:
    /// - Trait method binding (AST-only resolution)
    /// - Lifetime extraction
    ///
    /// Disabled features:
    /// - Macro expansion (no build script execution)
    #[must_use]
    pub fn safe_mode() -> Self {
        Self {
            enable_macro_expansion: false,
            enable_trait_binding: true,
            enable_lifetime_extraction: true,
            workspace_root: None,
        }
    }

    /// Create a minimal configuration with only basic AST features.
    ///
    /// This is the most restricted mode - only extracts nodes and basic edges
    /// without any P3 analysis. Use for maximum performance or compatibility.
    #[must_use]
    pub fn ast_only() -> Self {
        Self {
            enable_macro_expansion: false,
            enable_trait_binding: false,
            enable_lifetime_extraction: false,
            workspace_root: None,
        }
    }

    /// Set the workspace root for macro expansion and proc-macro detection.
    #[must_use]
    pub fn with_workspace_root(mut self, workspace_root: std::path::PathBuf) -> Self {
        self.workspace_root = Some(workspace_root);
        self
    }

    /// Builder method to disable macro expansion.
    #[must_use]
    pub fn without_macro_expansion(mut self) -> Self {
        self.enable_macro_expansion = false;
        self
    }
}

/// Graph builder for Rust files using unified `CodeGraph` architecture.
///
/// This implementation follows the two-phase `ASTGraph` architecture introduced
/// in JavaScript for O(1) context lookups during call edge detection.
///
/// # Supported Features
///
/// - Function definitions (fn)
/// - Impl blocks (both trait impls and inherent impls)
/// - Call expressions (function calls)
/// - Macro invocations (macro calls)
/// - Use declarations (imports)
/// - Extern crate declarations
/// - Foreign mod items (FFI extern blocks)
/// - Method call resolution (self.method -> `Type::method`)
/// - Async/unsafe detection
/// - Proper argument counting
///
/// # P3 Features (configurable)
///
/// - Trait method binding resolution
/// - Lifetime constraint extraction
/// - Proc-macro detection
/// - Confidence indicators
pub struct RustGraphBuilder {
    max_scope_depth: usize,
    config: RustGraphConfig,
}

impl Default for RustGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
            config: RustGraphConfig::new(),
        }
    }
}

impl RustGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self {
            max_scope_depth,
            config: RustGraphConfig::new(),
        }
    }

    /// Create a new builder with custom configuration.
    #[must_use]
    pub fn with_config(max_scope_depth: usize, config: RustGraphConfig) -> Self {
        Self {
            max_scope_depth,
            config,
        }
    }

    /// Get the current configuration.
    #[must_use]
    pub fn config(&self) -> &RustGraphConfig {
        &self.config
    }
}

impl Clone for RustGraphBuilder {
    fn clone(&self) -> Self {
        // Clone creates independent builder with fresh cache.
        // This is intentional - each builder instance may be used
        // for different workspaces or with different configs.
        Self {
            max_scope_depth: self.max_scope_depth,
            config: self.config.clone(),
        }
    }
}

impl std::fmt::Debug for RustGraphBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustGraphBuilder")
            .field("max_scope_depth", &self.max_scope_depth)
            .field("config", &self.config)
            .finish()
    }
}

/// Context for graph building that holds mutable state and P3 components.
///
/// This struct is passed through the tree walk functions to provide access
/// to confidence tracking, trait method binding, and other P3 features.
struct BuildContext<'a> {
    /// Confidence tracker for recording analysis limitations
    confidence: ConfidenceTracker,
    /// Trait method binder for resolving trait method calls
    trait_binder: TraitMethodBinder,
    /// Proc-macro detector for the current crate
    proc_macro_detector: ProcMacroDetector,
    /// Module resolver for cross-file analysis
    module_resolver: ModuleResolver,
    /// Configuration for enabled features
    config: &'a RustGraphConfig,
    /// Whether rust-analyzer is available and initialized
    ra_available: bool,
    /// File-level module path for qualified name prefixing.
    ///
    /// - `None` for crate roots (`lib.rs`, `main.rs`)
    /// - `Some("extra")` for `src/extra.rs`
    /// - `Some("foo::bar")` for `src/foo/bar.rs`
    ///
    /// Used by `qualify_item_name` to prefix symbols with their file's module context.
    file_module_path: Option<String>,
    /// Mapping from tree-sitter node IDs to graph `(NodeId, qualified_name)` pairs.
    ///
    /// Populated during `walk_tree_for_staging` for items like functions, structs,
    /// enums, macros, etc. Used by the macro boundary orchestrator to find the
    /// graph node corresponding to each AST item.
    node_map: std::collections::HashMap<usize, (sqry_core::graph::unified::NodeId, String)>,
}

impl<'a> BuildContext<'a> {
    fn new(config: &'a RustGraphConfig, file_path: &Path) -> Self {
        // Determine workspace root for proc-macro detection and module resolution
        let workspace_root = config.workspace_root.as_deref().unwrap_or_else(|| {
            // Without workspace root, use file's parent as best guess
            file_path
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or(Path::new("."))
        });

        // Detect proc-macro crate status
        let proc_macro_detector = ProcMacroDetector::detect(workspace_root, file_path);

        // Initialize module resolver for cross-file analysis
        let module_resolver = ModuleResolver::new(workspace_root.to_path_buf());

        // Compute file-level module path for qualified name prefixing
        let file_module_path = module_resolver.compute_file_module_path(file_path);

        // Initialize confidence based on configuration
        let mut confidence = if config.enable_macro_expansion {
            ConfidenceTracker::new(ConfidenceLevel::Partial)
        } else {
            let mut tracker = ConfidenceTracker::new(ConfidenceLevel::Partial);
            tracker.add_limitation("Macro expansion disabled for security");
            tracker.add_unavailable_feature("macro_expansion");
            tracker
        };

        // The graph builder currently performs AST-based analysis only.
        // Do not probe rust-analyzer here unless production RA inference is wired in.
        confidence.add_unavailable_feature("type_inference");

        Self {
            confidence,
            trait_binder: TraitMethodBinder::new(),
            proc_macro_detector,
            module_resolver,
            config,
            // Keep trait binding on AST heuristics until production RA inference exists.
            ra_available: false,
            file_module_path,
            node_map: std::collections::HashMap::new(),
        }
    }
}

impl GraphBuilder for RustGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);

        let mut build_ctx = BuildContext::new(&self.config, file);

        // Build AST graph for call context tracking (O(1) context lookups)
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Two-pass approach for FFI call linking:
        // Pass 1: Collect FFI declarations so calls can be resolved regardless of source order
        let mut ffi_registry = FfiRegistry::new();
        collect_ffi_declarations(tree.root_node(), content, &mut ffi_registry);

        // Pass 1.5: Collect trait implementations for trait method binding (if enabled)
        if self.config.enable_trait_binding {
            collect_trait_impls(tree.root_node(), content, &mut build_ctx.trait_binder);
        }

        // Build local scope tree for variable reference resolution
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Pass 2: Walk tree to find functions, methods, calls, imports, and macros
        // The ffi_registry is now fully populated, so FFI calls will be properly linked
        walk_tree_for_staging(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &ffi_registry,
            &mut build_ctx,
            &mut scope_tree,
        )?;

        // Pass 2.5: Macro boundary analysis (4.5a, 4.5b, 4.5c, 4.5e)
        // Runs after the main tree walk so the node_map is fully populated.
        {
            let macro_config = crate::macro_boundaries::MacroBoundaryConfig::default();
            let mut metadata_store = sqry_core::graph::unified::NodeMetadataStore::new();
            crate::macro_boundaries::analyze_macro_boundaries_in_build_graph(
                tree,
                content,
                &mut helper,
                &mut metadata_store,
                &macro_config,
                &build_ctx.node_map,
            );
            // Merge metadata into staging if any was produced
            if !metadata_store.is_empty() {
                log::debug!(
                    "Macro boundary analysis: {} metadata entries for {}",
                    metadata_store.len(),
                    file.display(),
                );
                // Metadata store will be merged into the graph during commit
                // via the staging graph's macro_metadata field
                staging.merge_macro_metadata(&metadata_store);
            }
        }

        // Store confidence metadata in the staging graph for persistence.
        // The sqry-lang-rust::ConfidenceMetadata and sqry-core::ConfidenceMetadata types
        // have identical structures, so we convert by reconstructing.
        let rust_confidence = build_ctx.confidence.to_metadata();
        let core_confidence = sqry_core::confidence::ConfidenceMetadata {
            level: match rust_confidence.level {
                ConfidenceLevel::Verified => sqry_core::confidence::ConfidenceLevel::Verified,
                ConfidenceLevel::Partial => sqry_core::confidence::ConfidenceLevel::Partial,
                ConfidenceLevel::AstOnly => sqry_core::confidence::ConfidenceLevel::AstOnly,
            },
            limitations: rust_confidence.limitations,
            unavailable_features: rust_confidence.unavailable_features,
        };
        staging.set_confidence(core_confidence);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(rust_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Rust: the reference implementation for the
/// identifier-blind body-shape descriptor feature.
///
/// Holds a precomputed `kind_id -> CfBucket` table so the hot shape walk does a
/// single array index per node instead of a grammar string lookup. The table is
/// built once from the tree-sitter-rust grammar and shared process-wide via
/// [`rust_shape_mapping`]. Everything except this mapping (the subtree walk,
/// histogram counting, shingling, WL relabel, `MinHash`, `shape_hash`) is the one
/// shared `compute_shape_descriptor` routine in sqry-core.
pub struct RustShapeMapping {
    /// `cf_by_kind_id[id]` is the canonical control-flow bucket for grammar
    /// node-kind id `id`, or `None` when that kind is not a control-flow construct
    /// (it still contributes to the structural shingle, just not the histogram).
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl RustShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-rust grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let count = lang.node_kind_count();
        let mut cf_by_kind_id = vec![None; count];
        for (id, slot) in cf_by_kind_id.iter_mut().enumerate() {
            let Ok(kind_id) = u16::try_from(id) else {
                break;
            };
            // Only NAMED kinds are control-flow constructs; anonymous tokens
            // (operators, keywords, punctuation) are structure, not buckets.
            if !lang.node_kind_is_named(kind_id) {
                continue;
            }
            if let Some(name) = lang.node_kind_for_id(kind_id) {
                *slot = cf_bucket_for_rust_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for RustShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        if let Some(params) = fn_node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                match child.kind() {
                    "parameter" | "self_parameter" => {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                    }
                    // `extern "C" fn f(x: i32, ...)` variadic tail.
                    "variadic_parameter" => shape.has_varargs = true,
                    _ => {}
                }
            }
        }
        shape.has_return_annotation = fn_node.child_by_field_name("return_type").is_some();
        shape
    }
}

/// Map one tree-sitter-rust grammar node-kind name to its canonical control-flow
/// bucket. Additive-only: the bucket set is frozen (see `CfBucket`), so new Rust
/// kinds extend the match, never reorder the buckets.
fn cf_bucket_for_rust_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_expression" | "if_let_expression" => CfBucket::Branch,
        "match_expression" => CfBucket::Match,
        "for_expression" | "while_expression" | "while_let_expression" | "loop_expression" => {
            CfBucket::Loop
        }
        "return_expression" => CfBucket::Return,
        "yield_expression" => CfBucket::Yield,
        "await_expression" => CfBucket::Await,
        // The `?` postfix operator: error propagation maps onto the `try` bucket.
        "try_expression" => CfBucket::Try,
        "break_expression" | "continue_expression" => CfBucket::BreakContinue,
        "call_expression" | "macro_invocation" => CfBucket::Call,
        "let_declaration" | "assignment_expression" | "compound_assignment_expr" => {
            CfBucket::Assign
        }
        "closure_expression" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Rust shape mapping, built once on first use.
#[must_use]
pub fn rust_shape_mapping() -> &'static RustShapeMapping {
    static MAPPING: OnceLock<RustShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(RustShapeMapping::build)
}

// ========== StagingGraph Walking Functions ==========

/// Process lifetime extraction results and add nodes/edges to the graph.
fn process_lifetime_extraction(result: &LifetimeExtractionResult, helper: &mut GraphBuildHelper) {
    use std::collections::HashMap;
    let mut node_ids: HashMap<String, NodeId> = HashMap::new();

    // Create all lifetime nodes first
    for node in &result.nodes {
        let node_id = helper.add_lifetime(&node.qualified_name, node.span);
        node_ids.insert(node.qualified_name.clone(), node_id);
    }

    // Create all constraint edges
    for edge in &result.edges {
        // Determine the correct node kind for source based on constraint type
        let source_id = *node_ids
            .entry(edge.source_qualified.clone())
            .or_insert_with(|| {
                match edge.constraint_kind {
                    // For TypeBound (T: 'a), source is a type, not a lifetime
                    LifetimeConstraintKind::TypeBound => {
                        helper.add_node(&edge.source_qualified, None, NodeKind::Type)
                    }
                    // For all other constraints, source is a lifetime
                    _ => helper.add_lifetime(&edge.source_qualified, None),
                }
            });

        // Target is always a lifetime
        let target_id = *node_ids
            .entry(edge.target_qualified.clone())
            .or_insert_with(|| helper.add_lifetime(&edge.target_qualified, None));

        helper.add_lifetime_constraint_edge(source_id, target_id, edge.constraint_kind);
    }
}

/// Process trait method binding and add edges to the graph.
fn process_trait_binding(caller_id: NodeId, result: &BindingResult, helper: &mut GraphBuildHelper) {
    match result {
        BindingResult::Single(info) => {
            let callee_node_id = helper.add_function(&info.method_qualified, None, false, false);
            helper.add_trait_method_binding_edge(
                caller_id,
                callee_node_id,
                &info.trait_name,
                &info.impl_type,
                false,
            );
        }
        BindingResult::Multiple(infos) => {
            for info in infos {
                let callee_node_id =
                    helper.add_function(&info.method_qualified, None, false, false);
                helper.add_trait_method_binding_edge(
                    caller_id,
                    callee_node_id,
                    &info.trait_name,
                    &info.impl_type,
                    true,
                );
            }
        }
        BindingResult::NotFound { .. }
        | BindingResult::UnknownReceiverType { .. }
        | BindingResult::DeferToRa => {
            // No edge emitted - handled by confidence tracker
        }
    }
}

/// Process derive attributes on a struct/enum and emit `MacroExpansion` edges.
///
/// For each `#[derive(Trait)]` attribute, creates a `Macro` node for the derive macro
/// and emits a `MacroExpansion` edge from the item to the macro.
///
/// # Arguments
///
/// * `item_node` - The struct/enum tree-sitter node
/// * `content` - Source file content
/// * `item_qualified` - Qualified name of the item (e.g., `my_module::MyStruct`)
/// * `item_id` - `NodeId` of the struct/enum
/// * `helper` - Graph builder helper
///
/// # Note
///
/// In tree-sitter-rust, attributes appear as **preceding siblings** of the item
/// they annotate, not as children. This function walks backwards through siblings
/// to find all `attribute_item` nodes.
fn process_derive_attributes(
    item_node: Node,
    content: &[u8],
    item_qualified: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    // Walk backwards through preceding siblings to find attribute_item nodes
    // (Rust attributes are siblings, not children of the item they annotate)
    let mut sibling = item_node.prev_sibling();
    while let Some(current) = sibling {
        if current.kind() == "attribute_item" {
            process_single_derive_attribute(current, content, item_qualified, item_id, helper);
        } else if current.kind() != "line_comment" && current.kind() != "block_comment" {
            // Stop at non-comment, non-attribute nodes
            break;
        }
        sibling = current.prev_sibling();
    }
}

/// Process a single `attribute_item` node for derive macros.
fn process_single_derive_attribute(
    attr_node: Node,
    content: &[u8],
    item_qualified: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    // Get the attribute text
    let Ok(attr_text) = attr_node.utf8_text(content) else {
        return;
    };

    // Normalize whitespace: collapse all whitespace sequences to single spaces
    // This handles `#[derive (Debug)]` and multiline derives
    let normalized: String = attr_text.split_whitespace().collect::<Vec<_>>().join(" ");

    // Check if it's a derive attribute (with optional whitespace between derive and paren)
    // Handles: #[derive(Debug)], #[derive (Debug)], etc.
    let derive_start = if normalized.starts_with("#[derive(") {
        Some("#[derive(".len())
    } else if normalized.starts_with("#[derive (") {
        Some("#[derive (".len())
    } else {
        None
    };

    let Some(start_idx) = derive_start else {
        return;
    };

    // Find the closing )]
    let Some(end_idx) = normalized.rfind(")]") else {
        return;
    };

    if start_idx >= end_idx {
        return;
    }

    // Extract the content between derive( and )]
    let derive_content = &normalized[start_idx..end_idx];

    // Split by comma to get individual traits
    for trait_name in derive_content.split(',') {
        let trait_name = trait_name.trim();
        if trait_name.is_empty() {
            continue;
        }

        // Create invocation node for this specific derive
        let invocation_qualified = format!(
            "{}::derive_{}@{}:{}",
            item_qualified,
            trait_name,
            attr_node.start_position().row + 1,
            attr_node.start_position().column + 1
        );
        let invocation_span = Some(span_from_node(attr_node));
        let invocation_id =
            helper.add_node(&invocation_qualified, invocation_span, NodeKind::CallSite);

        // Create macro target node
        let macro_name = format!("derive_{trait_name}");
        let macro_id = helper.add_node(&macro_name, None, NodeKind::Macro);

        // Add Contains edge from owner to invocation
        helper.add_contains_edge(item_id, invocation_id);

        // Add call edge from invocation to macro
        helper.add_call_edge(invocation_id, macro_id);

        // Add MacroExpansion edge from invocation to macro
        helper.add_macro_expansion_edge(
            invocation_id,
            macro_id,
            MacroExpansionKind::Derive,
            false, // Not verified without cargo expand
        );
    }
}

/// Emit a `TypeOf { context: Some(Return), index: Some(0), .. }` edge from a
/// function/method/macro node to the type appearing after `->` in its signature.
///
/// Tree-sitter-rust exposes the explicit return-type annotation via the
/// `return_type` field on `function_item` and `function_signature_item`. When
/// the field is absent the function returns the implicit unit type `()` and
/// no edge is emitted (the byte-exact `returns:<TypeName>` contract has no
/// well-defined target name in that case).
///
/// The byte-exact target text is the raw source slice of the `return_type`
/// node — the leading `->` token is *not* part of that field, so for
/// `fn foo() -> Result<i32, Error>` we register the type `Result<i32, Error>`.
/// A defensive `trim_start_matches("->").trim()` strips any stray arrow tokens
/// in case a future grammar revision shifts the field span.
///
/// Async functions, generic functions, free functions, impl methods, and FFI
/// `extern fn` declarations all flow through this helper. Closures
/// (`closure_expression`) are intentionally not handled — their return types
/// are inferred and lack a stable byte-exact spelling.
fn emit_return_type_edge(
    fn_node: Node<'_>,
    function_id: NodeId,
    function_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    let Some(return_type_node) = fn_node.child_by_field_name("return_type") else {
        return;
    };
    let Ok(raw_text) = return_type_node.utf8_text(content) else {
        return;
    };
    let type_text = raw_text.trim().trim_start_matches("->").trim();
    if type_text.is_empty() {
        return;
    }
    // Anchor the synthesized Type node at the return-type annotation's span
    // so downstream consumers (LSP `textDocument/documentSymbol`, MCP
    // `get_document_symbols`) report a concrete source location rather than
    // line 0.
    let type_span = span_from_node(return_type_node);
    let type_id = helper.add_type(type_text, Some(type_span));
    helper.add_typeof_edge_with_context(
        function_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0),
        Some(function_name),
    );
}

/// This function handles:
/// - Function definitions (`function_item`) → Function nodes
/// - Call expressions (`call_expression`) → Call edges (or `FfiCall` for FFI targets)
/// - Macro invocations (`macro_invocation`) → Call edges to macros
/// - Use declarations (`use_declaration`) → Import edges
/// - Extern crate declarations (`extern_crate_declaration`) → Import edges
/// - Foreign mod items (`foreign_mod`) → FFI declaration nodes
///
/// The `ffi_registry` is pre-populated by `collect_ffi_declarations` to ensure
/// FFI calls are properly linked regardless of source code order.
#[allow(
    clippy::too_many_lines,
    reason = "Rust graph extraction handles multiple node kinds in one traversal."
)]
fn walk_tree_for_staging(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    ffi_registry: &FfiRegistry,
    build_ctx: &mut BuildContext,
    scope_tree: &mut local_scopes::RustScopeTree,
) -> GraphResult<()> {
    // Clone file_path to avoid borrow issues
    let file_path = helper.file_path().to_string();

    match node.kind() {
        "function_item" => {
            // Extract function context from AST graph
            if let Some(context) = ast_graph.get_callable_context(node.id()) {
                let span = span_from_node(node);
                let is_exported = is_unrestricted_pub(node, content);
                let visibility = extract_visibility(node, content);

                // Build qualified name with file module path prefix
                let qualified_name = match &build_ctx.file_module_path {
                    Some(file_mod) if !context.qualified_name.is_empty() => {
                        format!("{file_mod}::{}", context.qualified_name)
                    }
                    _ => context.qualified_name.clone(),
                };

                // Check for proc-macro attributes
                let attr_text = extract_attribute_text(node, content);
                let has_proc_macro_attr = ProcMacroDetector::has_proc_macro_attribute(&attr_text);

                // Add function, method, or macro node and capture the node ID
                let item_id = if context.is_method {
                    let method_id = helper.add_method_with_visibility(
                        &qualified_name,
                        Some(span),
                        context.is_async,
                        false, // Rust methods are instance by default
                        visibility,
                    );
                    if is_exported {
                        export_from_file_module(helper, method_id);
                    }
                    method_id
                } else if has_proc_macro_attr
                    && build_ctx
                        .proc_macro_detector
                        .should_extract_as_macro(true, &mut build_ctx.confidence)
                {
                    // This is a proc-macro function - extract as Macro node
                    let macro_id = helper.add_node_with_visibility(
                        &qualified_name,
                        Some(span),
                        NodeKind::Macro,
                        visibility,
                    );
                    if is_exported {
                        export_from_file_module(helper, macro_id);
                    }
                    macro_id
                } else {
                    let function_id = helper.add_function_with_visibility(
                        &qualified_name,
                        Some(span),
                        context.is_async,
                        context.is_unsafe,
                        visibility,
                    );
                    if is_exported {
                        export_from_file_module(helper, function_id);
                    }
                    function_id
                };

                // Record this item for macro boundary analysis
                build_ctx
                    .node_map
                    .insert(node.id(), (item_id, qualified_name.clone()));

                // Emit Return-context TypeOf edge for the explicit `-> Type`
                // annotation. The bare function name (last `::` segment)
                // becomes the edge's `name` metadata, mirroring the C# /
                // Kotlin / Go plugins. Implicit-unit returns (no annotation)
                // produce no edge.
                let fn_short_name = qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(qualified_name.as_str());
                emit_return_type_edge(node, item_id, fn_short_name, content, helper);

                // Add Contains edge from containing module to this function/method/macro
                // This enables scope.* predicate queries (scope.type:module, scope.parent:X, etc.)
                if let Some(container_id) = find_containing_module(
                    node,
                    content,
                    helper,
                    build_ctx.file_module_path.as_deref(),
                ) {
                    helper.add_contains_edge(container_id, item_id);
                }

                // Extract lifetime constraints if enabled
                if build_ctx.config.enable_lifetime_extraction {
                    let mut extractor = LifetimeExtractor::new(
                        content,
                        context.qualified_name.clone(),
                        &mut build_ctx.confidence,
                    );
                    let lifetime_result = extractor.extract(node);
                    if !lifetime_result.is_empty() {
                        process_lifetime_extraction(&lifetime_result, helper);
                    }
                }

                // NEW: Extract trait bounds from type parameters and where clauses
                build_trait_bound_reference_edges(node, content, helper, item_id)?;

                // Emit per-type-parameter Type nodes + Constraint edges
                // (REQ:R0029 / U20 AC-1, AC-2, AC-3, AC-4). Lifetimes
                // stay on the existing LifetimeConstraint pipeline
                // (AC-5).
                process_type_parameter_declarations(node, content, &qualified_name, helper);
            }

            // Process children (this includes parameter types and function body)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_tree_for_staging(
                    child,
                    content,
                    ast_graph,
                    helper,
                    ffi_registry,
                    build_ctx,
                    scope_tree,
                )?;
            }

            // Clear local types when exiting function scope
            build_ctx.trait_binder.clear_local_types();

            // Early return to prevent double recursion
            return Ok(());
        }
        "struct_item" => {
            // Extract struct name for lifetime extraction regardless of visibility
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    let is_exported = is_unrestricted_pub(node, content);
                    let visibility = extract_visibility(node, content);

                    // Add struct node with visibility metadata (all structs, not just pub)
                    let id = helper.add_struct_with_visibility(&qualified, Some(span), visibility);
                    build_ctx
                        .node_map
                        .insert(node.id(), (id, qualified.clone()));

                    // Export if pub
                    if is_exported {
                        export_from_file_module(helper, id);
                    }

                    // Process derive macros if macro expansion is enabled (for exported structs)
                    if is_exported && build_ctx.config.enable_macro_expansion {
                        process_derive_attributes(node, content, &qualified, id, helper);
                    }

                    // Extract lifetime constraints if enabled (for all structs, not just pub)
                    if build_ctx.config.enable_lifetime_extraction {
                        let mut extractor = LifetimeExtractor::new(
                            content,
                            qualified.clone(),
                            &mut build_ctx.confidence,
                        );
                        let lifetime_result = extractor.extract(node);
                        if !lifetime_result.is_empty() {
                            process_lifetime_extraction(&lifetime_result, helper);
                        }
                    }

                    // NEW: Extract trait bounds from type parameters and where clauses
                    build_trait_bound_reference_edges(node, content, helper, id)?;
                }
            }
        }
        "enum_item" => {
            // Extract enum name for lifetime extraction regardless of visibility
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    let is_exported = is_unrestricted_pub(node, content);
                    let visibility = extract_visibility(node, content);

                    // Add enum node with visibility metadata (all enums, not just pub)
                    let id = helper.add_enum_with_visibility(&qualified, Some(span), visibility);
                    build_ctx
                        .node_map
                        .insert(node.id(), (id, qualified.clone()));

                    // Export if pub
                    if is_exported {
                        export_from_file_module(helper, id);
                    }

                    // Process derive macros if macro expansion is enabled (for exported enums)
                    if is_exported && build_ctx.config.enable_macro_expansion {
                        process_derive_attributes(node, content, &qualified, id, helper);
                    }

                    // Extract lifetime constraints if enabled (for all enums, not just pub)
                    if build_ctx.config.enable_lifetime_extraction {
                        let mut extractor = LifetimeExtractor::new(
                            content,
                            qualified.clone(),
                            &mut build_ctx.confidence,
                        );
                        let lifetime_result = extractor.extract(node);
                        if !lifetime_result.is_empty() {
                            process_lifetime_extraction(&lifetime_result, helper);
                        }
                    }

                    // NEW: Extract trait bounds from type parameters and where clauses
                    build_trait_bound_reference_edges(node, content, helper, id)?;
                }
            }
        }
        "trait_item" => {
            // Extract trait name for lifetime extraction regardless of visibility
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    let is_exported = is_unrestricted_pub(node, content);
                    let visibility = extract_visibility(node, content);

                    // Add trait node with visibility metadata (all traits, not just pub)
                    let id =
                        helper.add_interface_with_visibility(&qualified, Some(span), visibility);
                    build_ctx
                        .node_map
                        .insert(node.id(), (id, qualified.clone()));

                    // Export if pub
                    if is_exported {
                        export_from_file_module(helper, id);
                    }

                    // Extract lifetime constraints if enabled (for all traits, not just pub)
                    if build_ctx.config.enable_lifetime_extraction {
                        let mut extractor = LifetimeExtractor::new(
                            content,
                            qualified.clone(),
                            &mut build_ctx.confidence,
                        );
                        let lifetime_result = extractor.extract(node);
                        if !lifetime_result.is_empty() {
                            process_lifetime_extraction(&lifetime_result, helper);
                        }
                    }

                    // Emit per-type-parameter Type nodes + Constraint
                    // edges (REQ:R0029 / U20 AC-1, AC-2, AC-3, AC-4).
                    // Lifetimes stay on the existing
                    // LifetimeConstraint pipeline (AC-5).
                    process_type_parameter_declarations(node, content, &qualified, helper);
                }
            }
        }
        "type_item" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    let is_exported = is_unrestricted_pub(node, content);
                    let visibility = extract_visibility(node, content);
                    let id = helper.add_type_with_visibility(&qualified, Some(span), visibility);
                    build_ctx
                        .node_map
                        .insert(node.id(), (id, qualified.clone()));
                    if is_exported {
                        export_from_file_module(helper, id);
                    }

                    // NEW: Create Reference edges for type alias
                    build_type_alias_reference_edges(
                        node,
                        content,
                        helper,
                        build_ctx.file_module_path.as_deref(),
                    )?;
                }
            }
        }
        "const_item" | "static_item" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    let is_exported = is_unrestricted_pub(node, content);
                    let visibility = extract_visibility(node, content);
                    let id =
                        helper.add_constant_with_visibility(&qualified, Some(span), visibility);
                    build_ctx
                        .node_map
                        .insert(node.id(), (id, qualified.clone()));

                    // NEW: Create TypeOf and Reference edges for type-annotated const/static
                    build_const_static_typeof_edges(node, content, helper, id)?;

                    if is_exported {
                        export_from_file_module(helper, id);
                    }
                }
            }
        }
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified = qualify_item_name(
                    node,
                    name.trim(),
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    // Create module node for ALL mod declarations (pub or not)
                    // This ensures the module hierarchy is captured in the graph.
                    // Real module declaration (issue #394): opt the dual-use
                    // add_module bare helper into is_definition = true.
                    let mod_id = helper.add_module(&qualified, Some(span));
                    helper.mark_definition(mod_id);
                    build_ctx
                        .node_map
                        .insert(node.id(), (mod_id, qualified.clone()));

                    // Export only if pub
                    if is_unrestricted_pub(node, content) {
                        export_from_file_module(helper, mod_id);
                    }

                    // Add Contains edge from containing module to declared module
                    // For nested modules, this attaches to the parent module
                    // For top-level modules, this attaches to the file module
                    let container_id = find_containing_module(
                        node,
                        content,
                        helper,
                        build_ctx.file_module_path.as_deref(),
                    )
                    .unwrap_or_else(|| helper.add_module(FILE_MODULE_NAME, None));
                    helper.add_contains_edge(container_id, mod_id);

                    // Resolve external modules (mod foo;) to their file paths
                    // Inline modules (mod foo { ... }) have a body and don't need resolution
                    if node.child_by_field_name("body").is_none() {
                        let mod_name = name.trim();
                        let file_path = Path::new(helper.file_path());

                        // Check for #[path = "..."] attribute first
                        let resolved_path =
                            if let Some(path_attr) = extract_path_attribute(node, content) {
                                // Use the explicit path attribute
                                build_ctx
                                    .module_resolver
                                    .resolve_path_attribute(&path_attr, file_path)
                            } else {
                                // Fall back to standard resolution
                                match build_ctx.module_resolver.resolve(
                                    file_path,
                                    mod_name,
                                    &mut build_ctx.confidence,
                                ) {
                                    crate::module_resolver::ModuleResolution::Resolved(path) => {
                                        Some(path)
                                    }
                                    _ => None,
                                }
                            };

                        // Record successful resolution for future Pass 4 cross-file linking
                        //
                        // Note: Cross-file edges cannot be created in single-file processing
                        // because the target file's nodes don't exist yet in the staging graph.
                        // The PendingModuleLink stores the information needed for Pass 4 to
                        // create the cross-file edge once all files are indexed.
                        if let Some(resolved) = resolved_path {
                            // Record pending link for Pass 4
                            build_ctx.module_resolver.record_pending_link(
                                qualified.clone(),
                                PathBuf::from(helper.file_path()),
                                resolved,
                                node.start_position().row + 1,
                                node.start_position().column + 1,
                            );
                        }
                    }
                }
            }
        }
        "macro_definition" => {
            // Extract macro_rules! definitions as NodeKind::Macro symbols
            // Node structure: macro_definition -> "macro_rules!" identifier macro_rule*
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(macro_name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let macro_name = macro_name.trim();

                // Build qualified name including module context
                let qualified = qualify_item_name(
                    node,
                    macro_name,
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                if !qualified.is_empty() {
                    // Create the macro definition node
                    let macro_id = helper.add_node(&qualified, Some(span), NodeKind::Macro);
                    build_ctx
                        .node_map
                        .insert(node.id(), (macro_id, qualified.clone()));

                    // Check if macro has #[macro_export] attribute (makes it globally visible)
                    let is_exported = has_macro_export_attribute(node, content);

                    // If #[macro_export], also export from file module
                    if is_exported {
                        export_from_file_module(helper, macro_id);
                    }

                    // Add Contains edge from containing module to macro
                    if let Some(container_id) = find_containing_module(
                        node,
                        content,
                        helper,
                        build_ctx.file_module_path.as_deref(),
                    ) {
                        helper.add_contains_edge(container_id, macro_id);
                    }
                }
            }
        }
        "call_expression" => {
            // Build call edge from caller to callee
            if let Some(built_call) = build_call_for_staging(ast_graph, node, content, &file_path)?
            {
                let BuiltCall {
                    caller_qualified: source_qname,
                    callee_qualified: target_qname,
                    span,
                    has_turbofish: call_has_turbofish,
                } = built_call;
                // Get caller context for async/unsafe attributes
                let call_context = ast_graph.get_callable_context(node.id());
                let _is_async = call_context.is_some_and(|c| c.is_async);
                let _is_unsafe = call_context.is_some_and(|c| c.is_unsafe);
                let is_awaited = is_directly_awaited(node);
                let argument_count = u8::try_from(count_arguments(node)).unwrap_or(u8::MAX);

                // Ensure caller node exists
                let source_id = helper.ensure_callee(&source_qname, span, CalleeKindHint::Function);

                // Check if the callee is a known FFI function
                // IMPORTANT: Only do FFI lookup for unqualified calls (no `::`)
                // Qualified calls like `module::foo()` should NOT match FFI declarations
                // with the same simple name, as they refer to different functions.
                // This avoids false positives where `b::foo()` matches `extern { fn foo(); }`
                let is_unqualified = !target_qname.contains("::");
                if is_unqualified {
                    // Generic calls using turbofish syntax (e.g., `foo::<T>()`) cannot target
                    // `extern` declarations, since extern functions cannot be generic.
                    if !call_has_turbofish
                        && let Some((ffi_qualified, ffi_convention)) =
                            ffi_registry.get(&target_qname)
                    {
                        // This is a call to an FFI function - create FfiCall edge
                        let ffi_target_id =
                            helper.ensure_callee(ffi_qualified, span, CalleeKindHint::Function);
                        helper.add_ffi_edge(source_id, ffi_target_id, *ffi_convention);
                    } else {
                        // Regular call - create normal Call edge
                        let target_id =
                            helper.ensure_callee(&target_qname, span, CalleeKindHint::Function);
                        helper.add_call_edge_full_with_span(
                            source_id,
                            target_id,
                            argument_count,
                            is_awaited,
                            vec![span],
                        );
                    }
                } else {
                    // Qualified call - create normal Call edge (not FFI lookup)
                    let target_id =
                        helper.ensure_callee(&target_qname, span, CalleeKindHint::Function);
                    helper.add_call_edge_full_with_span(
                        source_id,
                        target_id,
                        argument_count,
                        is_awaited,
                        vec![span],
                    );
                }

                // Check if this is a method call (e.g., x.method()) and attempt trait binding
                if build_ctx.config.enable_trait_binding
                    && let Some(callee_expr) = node.child_by_field_name("function")
                    && callee_expr.kind() == "field_expression"
                    && let Some(receiver_node) = callee_expr.child_by_field_name("value")
                    && let Some(method_node) = callee_expr.child_by_field_name("field")
                    && let Ok(receiver_text) = receiver_node.utf8_text(content)
                    && let Ok(method_name) = method_node.utf8_text(content)
                {
                    // This is a method call - extract receiver and method name
                    let receiver_text = receiver_text.trim();
                    let method_name = method_name.trim();

                    // Resolve the trait method binding
                    let binding_result = build_ctx.trait_binder.resolve_call(
                        receiver_text,
                        method_name,
                        build_ctx.ra_available,
                        &mut build_ctx.confidence,
                    );

                    // Process the binding result and emit edges
                    process_trait_binding(source_id, &binding_result, helper);
                }
            }
        }
        "macro_invocation" => {
            // Build macro call edge with proper invocation node
            if let Some((source_qname, macro_qname, span)) =
                build_macro_for_staging(ast_graph, node, content, &file_path)?
            {
                // Get caller context
                let call_context = ast_graph.get_callable_context(node.id());
                let _is_async = call_context.is_some_and(|c| c.is_async);
                let _is_unsafe = call_context.is_some_and(|c| c.is_unsafe);
                let is_awaited = is_directly_awaited(node);

                // Ensure source function exists
                let source_id = helper.ensure_callee(&source_qname, span, CalleeKindHint::Function);

                // Create CallSite node for this specific invocation
                let invocation_qualified = format!(
                    "{}::{}@{}:{}",
                    source_qname,
                    macro_qname.trim_end_matches('!'),
                    node.start_position().row + 1,
                    node.start_position().column + 1
                );
                let invocation_id =
                    helper.add_node(&invocation_qualified, Some(span), NodeKind::CallSite);

                // Create the macro target node
                let macro_id = helper.add_node(&macro_qname, None, NodeKind::Macro);

                // Add call edge from source function to invocation
                helper.add_call_edge_full_with_span(
                    source_id,
                    invocation_id,
                    0,
                    is_awaited,
                    vec![span],
                );

                // Add call edge from invocation to macro target
                helper.add_call_edge(invocation_id, macro_id);

                // If macro expansion is enabled, add MacroExpansion edge from invocation to macro
                if build_ctx.config.enable_macro_expansion {
                    helper.add_macro_expansion_edge(
                        invocation_id, // Use the specific invocation node
                        macro_id,
                        MacroExpansionKind::Function,
                        false, // Not verified without cargo expand
                    );
                }
            }
        }
        "field_expression" => {
            // Build field access reference edge
            // e.g., `p.x` creates a reference from containing function to `<field:p.x>`
            if let Some((caller_qname, field_target)) =
                build_field_access_for_staging(ast_graph, node, content)
            {
                let call_context = ast_graph.get_callable_context(node.id());
                let _is_async = call_context.is_some_and(|c| c.is_async);
                let _is_unsafe = call_context.is_some_and(|c| c.is_unsafe);

                let source_id = helper.ensure_callee(
                    &caller_qname,
                    span_from_node(node),
                    CalleeKindHint::Function,
                );
                let target_id = helper.add_variable(&field_target, None);
                helper.add_reference_edge(source_id, target_id);
            }
        }
        "use_declaration" => {
            // Build import edges (and export edges for `pub use`)
            let is_pub = is_unrestricted_pub(node, content);
            build_use_for_staging(node, content, helper, is_pub)?;
        }
        "extern_crate_declaration" => {
            // Build extern crate import edge
            build_extern_crate_for_staging(node, content, helper);
        }
        "impl_item" => {
            // Handle trait implementations (impl Trait for Type)
            // Creates an Implements edge from Type to Trait
            build_impl_trait_for_staging(
                node,
                content,
                helper,
                build_ctx.file_module_path.as_deref(),
            );

            // Set current impl type for trait method binding context
            // Also use this type for lifetime extraction owner name
            let impl_type_name = if let Some(type_node) = node.child_by_field_name("type") {
                if let Ok(impl_type) = type_node.utf8_text(content) {
                    let impl_type_str = impl_type.trim().to_string();
                    build_ctx
                        .trait_binder
                        .set_current_impl_type(Some(impl_type_str.clone()));
                    Some(impl_type_str)
                } else {
                    None
                }
            } else {
                None
            };

            // Extract lifetime constraints if enabled
            // Use the impl type as the owner (e.g., impl<'a> Foo<'a> -> lifetimes are owned by "Foo")
            if build_ctx.config.enable_lifetime_extraction
                && let Some(type_name) = &impl_type_name
            {
                // Qualify the type name with module context
                let qualified = qualify_item_name(
                    node,
                    type_name,
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                let owner_qualified = if qualified.is_empty() {
                    type_name.clone()
                } else {
                    qualified
                };
                let mut extractor =
                    LifetimeExtractor::new(content, owner_qualified, &mut build_ctx.confidence);
                let lifetime_result = extractor.extract(node);
                if !lifetime_result.is_empty() {
                    process_lifetime_extraction(&lifetime_result, helper);
                }
            }

            // NEW: Extract trait bounds from type parameters and where clauses
            // Create Reference edges from the impl type to the trait bounds
            if let Some(type_node) = node.child_by_field_name("type")
                && let Ok(impl_type) = type_node.utf8_text(content)
            {
                let type_name = impl_type.trim();
                let qualified = qualify_item_name(
                    node,
                    type_name,
                    content,
                    build_ctx.file_module_path.as_deref(),
                );
                let span = span_from_node(node);
                // Get or create the type node (may already exist from struct/enum definition)
                let type_id = helper.add_struct(&qualified, Some(span));
                build_trait_bound_reference_edges(node, content, helper, type_id)?;

                // Emit per-type-parameter Type nodes + Constraint
                // edges (REQ:R0029 / U20 AC-1, AC-2, AC-3, AC-4) for
                // the impl block. The owner qualified name is the
                // base impl-type identifier with any generic
                // arguments stripped, so `impl<T> Foo<T>` produces
                // `Foo::T` rather than `Foo<T>::T`.
                let base_owner = extract_all_type_names_from_rust_type(type_node, content)
                    .first()
                    .cloned()
                    .map_or_else(
                        || qualified.clone(),
                        |base| {
                            qualify_item_name(
                                node,
                                &base,
                                content,
                                build_ctx.file_module_path.as_deref(),
                            )
                        },
                    );
                process_type_parameter_declarations(node, content, &base_owner, helper);
            }

            // Process children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_tree_for_staging(
                    child,
                    content,
                    ast_graph,
                    helper,
                    ffi_registry,
                    build_ctx,
                    scope_tree,
                )?;
            }

            // Clear impl type when exiting impl block
            build_ctx.trait_binder.set_current_impl_type(None);

            // Early return to prevent double recursion
            return Ok(());
        }
        "foreign_mod_item" => {
            // Handle extern "C" blocks (FFI declarations)
            // Creates FFI function/variable nodes
            // Note: The FFI registry was pre-populated by collect_ffi_declarations
            build_ffi_block_for_staging(node, content, helper);
        }
        "parameter" => {
            // NEW: Create TypeOf and Reference edges for type-annotated parameters
            build_parameter_typeof_edges(node, content, helper)?;

            // EXISTING: Register parameter types for trait method binding
            // Example: `fn foo(x: Vec<i32>)` -> register `x` -> `Vec<i32>`
            // Example: `fn foo(mut x: Vec<i32>)` -> register `x` -> `Vec<i32>`
            // Uses extract_identifier_from_pattern to handle mut/ref patterns correctly
            if let Some(pattern) = node.child_by_field_name("pattern")
                && let Some(type_node) = node.child_by_field_name("type")
                && let Some(var_name) = extract_identifier_from_pattern(pattern, content)
                && let Ok(type_name) = type_node.utf8_text(content)
            {
                build_ctx
                    .trait_binder
                    .register_local_type(var_name, type_name.trim());
            }
        }
        "let_declaration" => {
            // NEW: Create TypeOf and Reference edges for type-annotated variables
            build_let_declaration_typeof_edge(node, content, helper)?;

            // EXISTING: Register typed let bindings for trait method binding
            // Example: `let x: Vec<i32> = ...;` -> register `x` -> `Vec<i32>`
            // Example: `let mut x: Vec<i32> = ...;` -> register `x` -> `Vec<i32>`
            // Only registers if explicit type annotation is present
            // Uses extract_identifier_from_pattern to handle mut/ref patterns correctly
            if let Some(pattern) = node.child_by_field_name("pattern")
                && let Some(type_node) = node.child_by_field_name("type")
                && let Some(var_name) = extract_identifier_from_pattern(pattern, content)
                && let Ok(type_name) = type_node.utf8_text(content)
            {
                build_ctx
                    .trait_binder
                    .register_local_type(var_name, type_name.trim());
            }
        }
        "field_declaration" => {
            // U11 (REQ:R0001..R0005, R0021..R0023): emit Property nodes with
            // qualified names `{module_path}::{Struct}::{field}` (Rust :: retained
            // per design §3.1.2) + per-field visibility (§3.3 row 6) + TypeOf edges
            // carrying TypeOfContext::Field + bare field name. Covers named struct
            // fields and enum struct-variant fields.
            build_field_typeof_edges(node, content, helper, build_ctx.file_module_path.as_deref())?;
        }
        "ordered_field_declaration_list" => {
            // U11 (REQ:R0022): tuple-struct field collision resolved via
            // qualified Property names `{module_path}::{Struct}::{index}`.
            // Covers tuple structs (`struct Point(i32, i32);`) and enum
            // tuple-variants (`enum Foo { Bar(i32) }`).
            build_tuple_field_typeof_edges(
                node,
                content,
                helper,
                build_ctx.file_module_path.as_deref(),
            )?;
        }
        "identifier" => {
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_staging(
            child,
            content,
            ast_graph,
            helper,
            ffi_registry,
            build_ctx,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Build call edge information for staging graph.
///
/// Returns `BuiltCall` with caller/callee names and span for call edge construction.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    _file_path: &str,
) -> GraphResult<Option<BuiltCall>> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic top-level context for calls outside functions
        module_context = CallContext {
            name: Arc::from(TOPLEVEL_CALLER_NAME),
            qualified_name: TOPLEVEL_CALLER_NAME.to_string(),
            span: (0, content.len()),
            is_async: false,
            is_unsafe: false,
            is_method: false,
        };
        &module_context
    };

    // Get callee expression
    let Some(callee_expr) = call_node.child_by_field_name("function") else {
        return Ok(None);
    };

    let callee_text = callee_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim()
        .to_string();

    if callee_text.is_empty() {
        return Ok(None);
    }

    let (callee_text_normalized, removed_turbofish) = strip_turbofish_segments(&callee_text);
    let callee_text_normalized = callee_text_normalized.trim().to_string();

    if callee_text_normalized.is_empty() {
        return Ok(None);
    }

    let callee_simple = simple_name(&callee_text_normalized);
    if callee_simple.is_empty() {
        return Ok(None);
    }

    // Derive qualified callee name with proper self.method() resolution
    let source_qualified = call_context.qualified_name().to_string();
    let target_qualified = if let Some(method_name) = callee_text_normalized.strip_prefix("self.") {
        // Resolve self.method() to Type::method()
        if let Some(scope_idx) = source_qualified.rfind("::") {
            let type_name = &source_qualified[..scope_idx];
            format!("{}::{}", type_name, simple_name(method_name))
        } else {
            callee_text_normalized.clone()
        }
    } else if callee_text_normalized.contains("::") {
        callee_text_normalized.clone()
    } else {
        callee_simple.to_string()
    };

    let span = span_from_node(call_node);

    if removed_turbofish && target_qualified.is_empty() {
        return Ok(None);
    }

    Ok(Some(BuiltCall {
        caller_qualified: source_qualified,
        callee_qualified: target_qualified,
        span,
        has_turbofish: removed_turbofish,
    }))
}

/// Build macro call edge information for staging graph.
///
/// Returns `(caller_qname, callee_qname, span)` where span is the macro invocation location.
fn build_macro_for_staging(
    ast_graph: &ASTGraph,
    macro_node: Node<'_>,
    content: &[u8],
    _file_path: &str,
) -> GraphResult<Option<(String, String, Span)>> {
    // Get or create top-level context for macro calls outside functions
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(macro_node.id()) {
        ctx
    } else {
        module_context = CallContext {
            name: Arc::from(TOPLEVEL_CALLER_NAME),
            qualified_name: TOPLEVEL_CALLER_NAME.to_string(),
            span: (0, content.len()),
            is_async: false,
            is_unsafe: false,
            is_method: false,
        };
        &module_context
    };

    // Get macro name
    let Some(macro_name_node) = macro_node.child_by_field_name("macro") else {
        return Ok(None);
    };

    let macro_name = macro_name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(macro_node),
            reason: "failed to read macro name".to_string(),
        })?
        .trim()
        .to_string();

    if macro_name.is_empty() {
        return Ok(None);
    }

    // Normalize macro name to include ! suffix
    let macro_qualified = if macro_name.ends_with('!') {
        macro_name.clone()
    } else {
        format!("{macro_name}!")
    };

    let source_qualified = call_context.qualified_name().to_string();
    let span = span_from_node(macro_node);

    Ok(Some((source_qualified, macro_qualified, span)))
}

/// Build field access reference edge.
///
/// For expressions like `p.x`, creates a reference edge from the containing
/// function to a synthetic `<field:p.x>` target.
///
/// Returns `(caller_qualified_name, field_target)` on success, where:
/// - `caller_qualified_name` is the qualified name of the containing function
/// - `field_target` is the synthetic field reference like `<field:p.x>`
fn build_field_access_for_staging(
    ast_graph: &ASTGraph,
    node: Node<'_>,
    content: &[u8],
) -> Option<(String, String)> {
    // Get the containing function context
    let call_context = ast_graph.get_callable_context(node.id())?;
    let caller_qname = call_context.qualified_name().to_string();

    // Get the operand (left side of the dot)
    // field_expression has children: value . field
    let value_node = node.child_by_field_name("value")?;
    let field_node = node.child_by_field_name("field")?;

    let operand = value_node.utf8_text(content).ok()?.trim();
    let field = field_node.utf8_text(content).ok()?.trim();

    if operand.is_empty() || field.is_empty() {
        return None;
    }

    // Create the synthetic field target name
    let field_target = format!("<field:{operand}.{field}>");

    Some((caller_qname, field_target))
}

/// Build import edges for use declarations.
///
/// For `pub use` declarations, also emits `Exports` edges with `ExportKind::Reexport`
/// to model re-exports in Rust's module system.
fn build_use_for_staging(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    is_pub: bool,
) -> GraphResult<()> {
    collect_use_imports_for_staging(node, content, helper, "", is_pub)
}

/// Recursively collect imports from `use_tree` nodes.
///
/// When `is_pub` is true, emits `Exports` edges with `ExportKind::Reexport` in addition
/// to the `Imports` edges, modeling Rust's `pub use` re-export semantics.
#[allow(
    clippy::too_many_lines,
    reason = "Rust use syntax requires exhaustive pattern handling."
)]
fn collect_use_imports_for_staging(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    path_prefix: &str,
    is_pub: bool,
) -> GraphResult<()> {
    match node.kind() {
        "scoped_identifier" | "identifier" | "self" => {
            // Simple import: use std::fs;
            if let Ok(import_path) = node.utf8_text(content) {
                let trimmed = import_path.trim();
                if trimmed.is_empty() {
                    return Ok(());
                }

                let full_path = if trimmed == "self" && !path_prefix.is_empty() {
                    // `use foo::{self, bar};` imports the module itself.
                    path_prefix.to_string()
                } else if path_prefix.is_empty() {
                    trimmed.to_string()
                } else {
                    format!("{path_prefix}::{trimmed}")
                };

                // Create import node and edge
                let span = span_from_node(node);
                let module_id = helper.add_module(FILE_MODULE_NAME, None);
                let import_id = helper.add_import(&full_path, Some(span));
                helper.add_import_edge(module_id, import_id);

                // For `pub use`, also emit an export edge (re-export)
                if is_pub {
                    use sqry_core::graph::unified::edge::ExportKind;
                    helper.add_export_edge_full(module_id, import_id, ExportKind::Reexport, None);
                }
            }
        }
        "scoped_use_list" => {
            // Get path prefix from first child (scoped_identifier)
            let new_prefix = if let Some(scope_node) = node.child(0) {
                if let Ok(scope_text) = scope_node.utf8_text(content) {
                    if path_prefix.is_empty() {
                        scope_text.to_string()
                    } else {
                        format!("{path_prefix}::{scope_text}")
                    }
                } else {
                    path_prefix.to_string()
                }
            } else {
                path_prefix.to_string()
            };

            // Process the use_list child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "use_list" {
                    collect_use_imports_for_staging(
                        child,
                        content,
                        helper,
                        new_prefix.as_str(),
                        is_pub,
                    )?;
                }
            }
        }
        "use_list" => {
            // Process each item in the list
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "," && child.kind() != "{" && child.kind() != "}" {
                    collect_use_imports_for_staging(child, content, helper, path_prefix, is_pub)?;
                }
            }
        }
        "use_tree" => {
            // `use_tree` is the top-level container for a `use` declaration.
            //
            // For wildcard imports like `use std::collections::*;`, we need to propagate the
            // path prefix to the `use_wildcard` node so that we stage `std::collections::*`
            // instead of a bare `*`.
            let mut cursor = node.walk();
            let mut wildcard_child = None;
            let mut prefix_child_text = None;

            for child in node.children(&mut cursor) {
                if child.kind() == "use_wildcard" {
                    wildcard_child = Some(child);
                }
                if prefix_child_text.is_none()
                    && matches!(child.kind(), "scoped_identifier" | "identifier")
                    && let Ok(text) = child.utf8_text(content)
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        prefix_child_text = Some(trimmed.to_string());
                    }
                }
            }

            if let Some(wildcard_node) = wildcard_child {
                let effective_prefix = if let Some(child_prefix) = prefix_child_text {
                    if path_prefix.is_empty() {
                        child_prefix
                    } else {
                        format!("{path_prefix}::{child_prefix}")
                    }
                } else {
                    path_prefix.to_string()
                };
                collect_use_imports_for_staging(
                    wildcard_node,
                    content,
                    helper,
                    effective_prefix.as_str(),
                    is_pub,
                )?;
            } else {
                // Non-wildcard: recurse normally.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    collect_use_imports_for_staging(child, content, helper, path_prefix, is_pub)?;
                }
            }
        }
        "use_wildcard" => {
            // Wildcard import: use std::*;
            let full_path = if path_prefix.is_empty() {
                infer_wildcard_use_prefix(node, content)
                    .map_or_else(|| "*".to_string(), |prefix| format!("{prefix}::*"))
            } else {
                format!("{path_prefix}::*")
            };

            let span = span_from_node(node);
            let module_id = helper.add_module(FILE_MODULE_NAME, None);
            let import_id = helper.add_import(&full_path, Some(span));
            helper.add_import_edge_full(module_id, import_id, None, true);

            // For `pub use *`, also emit an export edge (namespace re-export)
            if is_pub {
                use sqry_core::graph::unified::edge::ExportKind;
                helper.add_export_edge_full(module_id, import_id, ExportKind::Namespace, None);
            }
        }
        "use_as_clause" => {
            // Aliased import: use std::io::Result as IoResult;
            if let Some(original) = node.child_by_field_name("path")
                && let Ok(original_path) = original.utf8_text(content)
            {
                let trimmed = original_path.trim();
                if trimmed.is_empty() {
                    return Ok(());
                }

                let full_path = if trimmed == "self" && !path_prefix.is_empty() {
                    // `use foo::{self as alias};` imports the module itself under an alias.
                    path_prefix.to_string()
                } else if path_prefix.is_empty() {
                    trimmed.to_string()
                } else {
                    format!("{path_prefix}::{trimmed}")
                };

                let span = span_from_node(node);
                let module_id = helper.add_module(FILE_MODULE_NAME, None);
                let import_id = helper.add_import(&full_path, Some(span));
                let alias = node
                    .child_by_field_name("alias")
                    .and_then(|alias_node| alias_node.utf8_text(content).ok())
                    .map(str::trim)
                    .filter(|alias_str| !alias_str.is_empty());
                helper.add_import_edge_full(module_id, import_id, alias, false);

                // For `pub use ... as alias`, also emit an export edge with the alias
                if is_pub {
                    use sqry_core::graph::unified::edge::ExportKind;
                    helper.add_export_edge_full(module_id, import_id, ExportKind::Reexport, alias);
                }
            }
        }
        _ => {
            // Recurse into children for other node types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_use_imports_for_staging(child, content, helper, path_prefix, is_pub)?;
            }
        }
    }

    Ok(())
}

fn infer_wildcard_use_prefix(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut current = node.parent();
    for _ in 0..16 {
        let Some(parent) = current else {
            break;
        };
        if let Ok(text) = parent.utf8_text(content) {
            let trimmed = text.trim();
            let normalized = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
            let mut prefix_candidate = None;
            if let Some(prefix) = normalized.strip_suffix("::*") {
                prefix_candidate = Some(prefix);
            } else if let Some((prefix, _)) = normalized.rsplit_once("::*") {
                prefix_candidate = Some(prefix);
            }

            if let Some(prefix) = prefix_candidate {
                let mut prefix_trimmed = prefix.trim();
                if let Some(rest) = prefix_trimmed.strip_prefix("pub use ") {
                    prefix_trimmed = rest.trim();
                } else if let Some(rest) = prefix_trimmed.strip_prefix("use ") {
                    prefix_trimmed = rest.trim();
                }
                if !prefix_trimmed.is_empty() && prefix_trimmed != "*" {
                    return Some(prefix_trimmed.to_string());
                }
            }
        }
        current = parent.parent();
    }
    None
}

/// Build import edge for extern crate declarations.
fn build_extern_crate_for_staging(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Get crate name
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(crate_name) = name_node.utf8_text(content)
    {
        let span = span_from_node(node);
        let module_id = helper.add_module(FILE_MODULE_NAME, None);
        let import_id = helper.add_import(crate_name, Some(span));
        helper.add_import_edge(module_id, import_id);
    }
}

/// Build Implements edge for trait implementations.
///
/// When we encounter `impl Trait for Type`, this creates an Implements edge
/// from Type to Trait. This captures Rust's trait implementation pattern.
///
/// For inherent impls (`impl Type { ... }`) without a trait, no OOP edge is created
/// since there's no inheritance or interface relationship.
fn build_impl_trait_for_staging(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    file_module_path: Option<&str>,
) {
    // Check if this is a trait impl (has "trait" field)
    let Some(trait_node) = node.child_by_field_name("trait") else {
        // Inherent impl (impl Type { ... }) - no OOP edge needed
        return;
    };

    // Get the trait name
    let Ok(trait_text) = trait_node.utf8_text(content) else {
        return;
    };
    let trait_name = trait_text.trim();
    if trait_name.is_empty() {
        return;
    }

    // Get the implementing type
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Ok(type_text) = type_node.utf8_text(content) else {
        return;
    };
    let type_name = type_text.trim();
    if type_name.is_empty() {
        return;
    }

    let span = span_from_node(node);

    // Qualify the names based on module context
    let qualified_type = qualify_item_name(node, type_name, content, file_module_path);
    let qualified_trait = qualify_item_name(node, trait_name, content, file_module_path);

    // Add the type node (as a struct, since we don't know the exact kind)
    // The struct may already exist if defined in this file
    let type_id = helper.add_struct(&qualified_type, Some(span));

    // Add the trait node (as an interface/trait)
    // The trait may already exist if defined in this file
    let trait_id = helper.add_interface(&qualified_trait, None);

    // Create the Implements edge: Type implements Trait
    helper.add_implements_edge(type_id, trait_id);
}

/// Collect trait implementations for trait method binding (Pass 1.5).
///
/// This function walks the AST to find all `impl Trait for Type` blocks
/// and registers them with the trait binder. This must be done before
/// processing method calls so that trait method bindings can be resolved.
fn collect_trait_impls(node: Node<'_>, content: &[u8], trait_binder: &mut TraitMethodBinder) {
    // Check if this is a trait impl (has "trait" field)
    if node.kind() == "impl_item"
        && let Some(trait_node) = node.child_by_field_name("trait")
        && let Ok(trait_text) = trait_node.utf8_text(content)
        && let Some(type_node) = node.child_by_field_name("type")
        && let Ok(type_text) = type_node.utf8_text(content)
        && let Some(body) = node.child_by_field_name("body")
    {
        let trait_name = trait_text.trim();
        let impl_type = type_text.trim();

        // Find methods in the impl block and register them
        let mut cursor = body.walk();
        for item in body.children(&mut cursor) {
            if item.kind() == "function_item"
                && let Some(name_node) = item.child_by_field_name("name")
                && let Ok(method_name) = name_node.utf8_text(content)
            {
                let method_name = method_name.trim();
                // Build qualified method name: Type::method
                let method_qualified = format!("{impl_type}::{method_name}");
                trait_binder.register_impl(impl_type, trait_name, &method_qualified);
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_trait_impls(child, content, trait_binder);
    }
}

/// Collect FFI declarations from extern blocks (Pass 1).
///
/// This function walks the entire AST to find all `extern "ABI" { ... }` blocks
/// and populates the FFI registry with function name → (qualified name, convention)
/// mappings. This must be done before processing calls so that FFI calls can be
/// properly linked regardless of source code order.
fn collect_ffi_declarations(node: Node<'_>, content: &[u8], ffi_registry: &mut FfiRegistry) {
    if node.kind() == "foreign_mod_item" {
        // Get the ABI string (e.g., "C", "system", etc.)
        let abi = extract_ffi_abi(node, content);
        let convention = abi_to_convention(&abi);

        // Find the declaration_list child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                // Process each declaration in the extern block
                let mut decl_cursor = child.walk();
                for decl in child.children(&mut decl_cursor) {
                    if decl.kind() == "function_signature_item" {
                        // FFI function declaration
                        if let Some(name_node) = decl.child_by_field_name("name")
                            && let Ok(fn_name) = name_node.utf8_text(content)
                        {
                            let fn_name = fn_name.trim();
                            if !fn_name.is_empty() {
                                let qualified = format!("extern::{abi}::{fn_name}");
                                ffi_registry.insert(fn_name.to_string(), (qualified, convention));
                            }
                        }
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ffi_declarations(child, content, ffi_registry);
    }
}

/// Build FFI function declarations from extern blocks (Pass 2).
///
/// Handles `extern "C" { ... }` blocks containing:
/// - `fn name(...)` - Foreign function declarations
/// - `static name: Type` - Foreign static variable declarations
///
/// Creates Function nodes for FFI declarations. The FFI registry is pre-populated
/// by `collect_ffi_declarations` so FFI calls can be linked properly.
fn build_ffi_block_for_staging(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Get the ABI string (e.g., "C", "system", etc.)
    let abi = extract_ffi_abi(node, content);

    // Find the declaration_list child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "declaration_list" {
            // Process each declaration in the extern block
            let mut decl_cursor = child.walk();
            for decl in child.children(&mut decl_cursor) {
                match decl.kind() {
                    "function_signature_item" => {
                        // FFI function declaration
                        if let Some(name_node) = decl.child_by_field_name("name")
                            && let Ok(fn_name) = name_node.utf8_text(content)
                        {
                            let fn_name = fn_name.trim();
                            if !fn_name.is_empty() {
                                let span = span_from_node(decl);
                                // Qualify with extern block context
                                let qualified = format!("extern::{abi}::{fn_name}");
                                // Add as unsafe function (FFI functions are inherently unsafe)
                                let fn_id = helper.add_function(
                                    &qualified,
                                    Some(span),
                                    false, // not async
                                    true,  // unsafe (FFI)
                                );
                                // Emit Return-context TypeOf edge for the
                                // explicit return type on this FFI signature.
                                // No-op when the signature returns implicit
                                // unit (no `-> Type` annotation).
                                emit_return_type_edge(decl, fn_id, fn_name, content, helper);
                                // Export FFI functions so they're visible
                                export_from_file_module(helper, fn_id);
                            }
                        }
                    }
                    "static_item" => {
                        // FFI static variable declaration
                        if let Some(name_node) = decl.child_by_field_name("name")
                            && let Ok(static_name) = name_node.utf8_text(content)
                        {
                            let static_name = static_name.trim();
                            if !static_name.is_empty() {
                                let span = span_from_node(decl);
                                // Qualify with extern block context
                                let qualified = format!("extern::{abi}::{static_name}");
                                let static_id = helper.add_constant(&qualified, Some(span));
                                // Export FFI statics so they're visible
                                export_from_file_module(helper, static_id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Convert an ABI string to an FFI calling convention.
///
/// Maps Rust ABI strings like "C", "system", "stdcall", "cdecl", "fastcall"
/// to the corresponding `FfiConvention` enum variant.
fn abi_to_convention(abi: &str) -> FfiConvention {
    // Normalize: strip "-unwind" suffix (e.g., "C-unwind" -> "C")
    // The unwind behavior is a separate axis from the calling convention
    let normalized = abi.to_lowercase();
    let base_abi = normalized.strip_suffix("-unwind").unwrap_or(&normalized);

    match base_abi {
        "cdecl" => FfiConvention::Cdecl,
        "system" | "win64" | "sysv64" => FfiConvention::System,
        "stdcall" => FfiConvention::Stdcall,
        "fastcall" => FfiConvention::Fastcall,
        // Default to C for unknown ABIs (including "c")
        _ => FfiConvention::C,
    }
}

/// Extract the ABI string from an extern block.
///
/// Returns the ABI string (e.g., "C", "system") or "C" as default.
fn extract_ffi_abi(node: Node<'_>, content: &[u8]) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "extern_modifier" {
            // Look for string_literal child
            let mut mod_cursor = child.walk();
            for mod_child in child.children(&mut mod_cursor) {
                if mod_child.kind() == "string_literal" {
                    // Extract the content (without quotes)
                    if let Ok(text) = mod_child.utf8_text(content) {
                        let trimmed = text.trim_matches('"');
                        if !trimmed.is_empty() {
                            return trimmed.to_string();
                        }
                    }
                }
            }
        }
    }
    // Default to "C" if no ABI specified
    "C".to_string()
}

/// Extract the identifier name from a pattern node.
///
/// Tree-sitter patterns in Rust can be:
/// - `identifier` - direct identifier like `x`
/// - `mut_pattern` - mutable pattern like `mut x`, has an `identifier` child
/// - `ref_pattern` - reference pattern like `ref x`, has an `identifier` child
/// - Nested patterns like `ref mut x` - `ref_pattern` -> `mut_pattern` -> `identifier`
/// - Destructuring patterns (tuple, struct, slice) - these are skipped
///
/// Returns `Some(identifier_text)` for simple identifier or mut/ref patterns,
/// `None` for complex patterns (destructuring, wildcards, etc.).
fn extract_identifier_from_pattern<'a>(pattern: Node<'_>, content: &'a [u8]) -> Option<&'a str> {
    let mut current = pattern;

    // Descend through mut_pattern/ref_pattern wrappers to find the identifier
    loop {
        match current.kind() {
            "identifier" => return current.utf8_text(content).ok().map(str::trim),
            "mut_pattern" | "ref_pattern" => {
                // For `mut x`, `ref x`, or nested `ref mut x`, descend into children
                let mut cursor = current.walk();
                let mut found_nested = false;
                for child in current.children(&mut cursor) {
                    match child.kind() {
                        "identifier" => {
                            return child.utf8_text(content).ok().map(str::trim);
                        }
                        "mut_pattern" | "ref_pattern" => {
                            // Nested pattern, continue descending
                            current = child;
                            found_nested = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !found_nested {
                    return None;
                }
            }
            // Wildcards, destructuring patterns, etc. - not simple bindings
            _ => return None,
        }
    }
}

fn simple_name(name: &str) -> &str {
    // Prefer Rust-style module paths (e.g., `std::mem::drop`)
    if let Some((_prefix, last)) = name.rsplit_once("::") {
        return last;
    }
    // Fallback for synthetic or path-like identifiers
    if let Some((_prefix, last)) = name.rsplit_once('/') {
        return last;
    }
    name
}

/// Strip turbofish type arguments from a Rust path-like expression.
///
/// Examples:
/// - `foo::<u8>` -> `foo`
/// - `Vec::<i32>::new` -> `Vec::new`
/// - `std::mem::size_of::<T>` -> `std::mem::size_of`
///
/// Returns `(normalized, removed_any)`. If the input appears malformed (e.g.,
/// unbalanced `<...>`), this returns the original string with `removed_any=false`.
fn strip_turbofish_segments(raw: &str) -> (String, bool) {
    let mut remaining = raw;
    let mut out = String::with_capacity(raw.len());
    let mut removed_any = false;

    while let Some(pos) = remaining.find("::<") {
        out.push_str(&remaining[..pos]);
        // Keep the `<...>` portion intact for bracket matching by skipping only the leading `::`.
        let after = &remaining[(pos + "::".len())..];
        if let Some(end) = find_matching_angle_brackets(after) {
            remaining = &after[end..];
            removed_any = true;
        } else {
            // Malformed generic args; leave string unchanged to avoid corrupting names.
            return (raw.to_string(), false);
        }
    }

    out.push_str(remaining);
    (out, removed_any)
}

/// Find the byte index *after* the matching `>` for an input that starts with `<`.
///
/// This is intentionally conservative: it ignores `<`/`>` balancing while inside
/// `{ ... }` blocks to avoid mis-parsing const generics like `::<{ 1 > 0 }>`.
#[allow(
    clippy::too_many_lines,
    reason = "Generic parsing handles nested and string literal cases in one pass."
)]
fn find_matching_angle_brackets(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.first().copied() != Some(b'<') {
        return None;
    }

    let mut idx = 0usize;
    let mut angle_depth = 0usize;
    let mut brace_depth = 0usize;

    let mut in_string = false;
    let mut in_char = false;
    let mut in_raw_string_hashes: Option<usize> = None;
    let mut escape = false;

    while idx < bytes.len() {
        let b = bytes[idx];

        if let Some(hashes) = in_raw_string_hashes {
            if b == b'"' {
                // End marker is `"` followed by the same number of `#`.
                let mut j = idx + 1;
                let mut matched = true;
                for _ in 0..hashes {
                    if j >= bytes.len() || bytes[j] != b'#' {
                        matched = false;
                        break;
                    }
                    j += 1;
                }
                if matched {
                    in_raw_string_hashes = None;
                    idx = j;
                    continue;
                }
            }
            idx += 1;
            continue;
        }

        if in_string {
            if escape {
                escape = false;
                idx += 1;
                continue;
            }
            if b == b'\\' {
                escape = true;
                idx += 1;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            idx += 1;
            continue;
        }

        if in_char {
            if escape {
                escape = false;
                idx += 1;
                continue;
            }
            if b == b'\\' {
                escape = true;
                idx += 1;
                continue;
            }
            if b == b'\'' {
                in_char = false;
            }
            idx += 1;
            continue;
        }

        // Raw string start: r###" ... "### (also handles br###"...")
        if b == b'r' || (b == b'b' && bytes.get(idx + 1).copied() == Some(b'r')) {
            let mut j = if b == b'b' { idx + 2 } else { idx + 1 };
            let mut hashes = 0usize;
            while bytes.get(j).copied() == Some(b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j).copied() == Some(b'"') {
                in_raw_string_hashes = Some(hashes);
                idx = j + 1;
                continue;
            }
        }

        match b {
            b'"' => {
                in_string = true;
            }
            b'\'' => {
                in_char = true;
            }
            b'{' => {
                brace_depth = brace_depth.saturating_add(1);
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
            }
            b'<' => {
                if brace_depth == 0 {
                    angle_depth += 1;
                }
            }
            b'>' => {
                if brace_depth == 0 {
                    angle_depth = angle_depth.saturating_sub(1);
                    if angle_depth == 0 {
                        return Some(idx + 1);
                    }
                }
            }
            _ => {}
        }

        idx += 1;
    }

    None
}

/// Determine whether an expression is *directly awaited* via `.await`.
///
/// This returns true if there is an `await_expression` ancestor without an
/// intervening *enclosing* `call_expression` (i.e., the `.await` applies to this
/// expression, not to a larger call that merely contains it).
///
/// Examples:
/// - `foo().await` → `foo()` is awaited (true)
/// - `foo().await?` → `foo()` is awaited (true)
/// - `foo().bar().await` → `foo()` is NOT awaited (false) because `.await` applies to `bar()`
/// - `join!(foo(), bar()).await` → `foo()` is considered awaited (true) because `.await` applies
///   to the macro expression that drives these futures.
fn is_directly_awaited(node: Node<'_>) -> bool {
    let mut current = node;
    let mut saw_enclosing_call = false;
    for _ in 0..64 {
        let Some(parent) = current.parent() else {
            break;
        };
        if parent.kind() == "await_expression" {
            return !saw_enclosing_call;
        }
        if parent.kind() == "call_expression" {
            saw_enclosing_call = true;
        }
        current = parent;
    }
    false
}

fn count_arguments(node: Node<'_>) -> usize {
    node.child_by_field_name("arguments").map_or(0, |args| {
        let mut count = 0;
        let mut cursor = args.walk();
        for child in args.children(&mut cursor) {
            // Count non-punctuation nodes
            if !matches!(child.kind(), "(" | ")" | ",") {
                count += 1;
            }
        }
        count
    })
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

/// Extract attribute text from preceding siblings of a node.
///
/// In tree-sitter-rust, attributes appear as preceding `attribute_item` siblings.
/// This function walks backwards through siblings to collect all attribute text.
fn extract_attribute_text(node: Node<'_>, content: &[u8]) -> String {
    let mut attrs = Vec::new();
    let mut sibling = node.prev_sibling();

    while let Some(current) = sibling {
        if current.kind() == "attribute_item" {
            if let Ok(text) = current.utf8_text(content) {
                attrs.push(text.to_string());
            }
        } else if current.kind() != "line_comment" && current.kind() != "block_comment" {
            // Stop at non-comment, non-attribute nodes
            break;
        }
        sibling = current.prev_sibling();
    }

    attrs.join("\n")
}

/// Extract `#[path = "..."]` attribute value from a node's preceding attributes.
///
/// Used for module declarations like `#[path = "custom.rs"] mod foo;`
fn extract_path_attribute(node: Node<'_>, content: &[u8]) -> Option<String> {
    let attr_text = extract_attribute_text(node, content);
    extract_path_from_attr_text(&attr_text)
}

/// Extract path value from attribute text.
///
/// Handles various formats:
/// - `#[path = "custom.rs"]`
/// - `#[path="custom.rs"]`
/// - `#[path = "subdir/custom.rs"]`
fn extract_path_from_attr_text(attr_text: &str) -> Option<String> {
    // First, try to find #[path in the original text line by line
    for line in attr_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#[path") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('=')?;
            let rest = rest.trim_start();
            // Extract the string value
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"')?;
            return Some(rest[..end].to_string());
        }
    }

    // Fallback: search for #[path anywhere in normalized text
    // This handles cases where #[path appears after other attributes
    let normalized: String = attr_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(idx) = normalized.find("#[path") {
        let rest = &normalized[idx + "#[path".len()..];
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=')?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }

    None
}

fn is_unrestricted_pub(node: Node<'_>, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    let vis = node
        .children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier");

    if let Some(vis) = vis {
        return vis
            .utf8_text(content)
            .ok()
            .is_some_and(|t| t.trim() == "pub");
    }

    // Fallback: some parse contexts may not surface the visibility modifier as a named child.
    // Parse the node's leading tokens and accept only `pub` (excluding `pub(...)`).
    let Ok(text) = node.utf8_text(content) else {
        return false;
    };
    let trimmed = text.trim_start();
    let Some(after_pub) = trimmed.strip_prefix("pub") else {
        return false;
    };
    let after_pub = after_pub.trim_start();
    !after_pub.starts_with('(')
}

/// Extract a normalized visibility marker from a Rust item node.
///
/// Returns:
/// - `Some("public")` for any `pub` visibility (including restricted forms)
/// - `Some("private")` when no visibility modifier is present
fn extract_visibility(node: Node<'_>, content: &[u8]) -> Option<&'static str> {
    let mut cursor = node.walk();
    let Some(vis) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier")
    else {
        return Some("private");
    };

    let vis_text = vis.utf8_text(content).ok()?.trim();

    if vis_text.starts_with("pub") {
        Some("public")
    } else {
        Some("private")
    }
}

/// Check if a macro definition has the #[`macro_export`] attribute.
///
/// `#[macro_export]` makes the macro visible at the crate root,
/// which we model by adding an export edge.
fn has_macro_export_attribute(node: Node<'_>, content: &[u8]) -> bool {
    // Look for an attribute child that contains "macro_export"
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if (child.kind() == "attribute_item" || child.kind() == "attribute")
            && let Ok(text) = child.utf8_text(content)
            && text.contains("macro_export")
        {
            return true;
        }
    }
    false
}

/// Find the containing module for a node to create Contains edges.
///
/// Returns the `NodeId` of the nearest containing module if one exists
/// and has been added to the helper.
fn find_containing_module(
    node: Node<'_>,
    content: &[u8],
    helper: &GraphBuildHelper,
    file_module_path: Option<&str>,
) -> Option<sqry_core::graph::unified::node::NodeId> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "mod_item"
            && let Some(mod_name) = parent.child_by_field_name("name")
            && let Ok(text) = mod_name.utf8_text(content)
        {
            let qualified = qualify_item_name(parent, text.trim(), content, file_module_path);
            if !qualified.is_empty() {
                // Try to find this module in the helper's node cache
                return helper.get_node(&qualified);
            }
        }
        current = parent;
    }
    None
}

/// Check if a name is already qualified (contains ::, or starts with `crate/self/super/::`).
///
/// Already-qualified names should NOT have module context prepended.
fn is_already_qualified(name: &str) -> bool {
    // Check for path separators
    if name.contains("::") {
        return true;
    }
    // Check for leading path keywords
    if name.starts_with("crate")
        || name.starts_with("self")
        || name.starts_with("super")
        || name.starts_with("::")
    {
        return true;
    }
    // Check for generic parameters - these are complex types, not simple identifiers
    if name.contains('<') || name.contains('>') {
        return true;
    }
    false
}

/// Extract all type names from a Rust type annotation node.
///
/// Recursively traverses the type AST to extract all referenced type identifiers.
/// This handles all Rust type constructs including generics, trait bounds, associated types, etc.
///
/// # Examples
///
/// ```text
/// Vec<User>                           → ["Vec", "User"]
/// HashMap<String, Vec<Item>>          → ["HashMap", "String", "Vec", "Item"]
/// T: Display + Clone                  → ["T", "Display", "Clone"]
/// &mut DataFrame                      → ["DataFrame"]
/// <T as Iterator>::Item               → ["T", "Iterator", "Item"]
/// impl Display                        → ["Display"]
/// Box<dyn Fn(T) -> String>           → ["Box", "Fn", "T", "String"]
/// ```
///
/// # Arguments
///
/// * `type_node` - The type annotation AST node
/// * `content` - Source file bytes for text extraction
///
/// # Returns
///
/// A vector of all referenced type identifier strings (deduplicated by caller if needed)
#[must_use]
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub fn extract_all_type_names_from_rust_type(type_node: Node<'_>, content: &[u8]) -> Vec<String> {
    match type_node.kind() {
        // Base case: simple type identifier
        "type_identifier" | "primitive_type" => {
            if let Ok(text) = type_node.utf8_text(content) {
                vec![text.trim().to_string()]
            } else {
                Vec::new()
            }
        }

        // Generic types: Vec<T>, HashMap<K, V>
        // Structure: generic_type { type: type_identifier, type_arguments: type_arguments {...} }
        "generic_type" => {
            let mut types = Vec::new();

            // Extract base type (Vec, HashMap, Result, etc.)
            if let Some(base_type) = type_node.child_by_field_name("type") {
                types.extend(extract_all_type_names_from_rust_type(base_type, content));
            }

            // Extract type arguments (<T>, <K, V>)
            if let Some(type_args) = type_node.child_by_field_name("type_arguments") {
                let mut cursor = type_args.walk();
                for child in type_args.named_children(&mut cursor) {
                    types.extend(extract_all_type_names_from_rust_type(child, content));
                }
            }

            types
        }

        // Reference types: &T, &mut T
        // Extract base type T, skip lifetime and mut keyword if present
        "reference_type" => {
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Skip lifetime annotations and mutable_specifier, only extract types
                if child.kind() != "lifetime" && child.kind() != "mutable_specifier" {
                    return extract_all_type_names_from_rust_type(child, content);
                }
            }
            Vec::new()
        }

        // Pointer types: *const T, *mut T
        "pointer_type" => {
            if let Some(inner_type) = type_node.child_by_field_name("type") {
                extract_all_type_names_from_rust_type(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Array types: [T; N]
        // Extract element type T, skip length N
        "array_type" => {
            if let Some(element_type) = type_node.child_by_field_name("element") {
                extract_all_type_names_from_rust_type(element_type, content)
            } else {
                Vec::new()
            }
        }

        // Tuple types: (T, U, V)
        // Bounded type: Display + Clone + Send
        // Trait bounds: Display + Clone + Iterator
        "tuple_type" | "bounded_type" | "trait_bounds" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_rust_type(child, content));
            }
            types
        }

        // Function types: fn(T, U) -> V, Fn(T) -> U, FnMut(T) -> U, FnOnce(T) -> U
        // For trait-based functions (Fn, FnMut, FnOnce), extract the trait name
        "function_type" => {
            let mut types = Vec::new();

            // Check if there's a trait field (for Fn, FnMut, FnOnce traits)
            let has_trait = if let Some(trait_node) = type_node.child_by_field_name("trait") {
                types.extend(extract_all_type_names_from_rust_type(trait_node, content));
                true
            } else {
                false
            };

            // For bare function pointers (fn(...) -> ...), insert synthetic "fn" marker
            // as primary type to avoid TypeOf edge pointing to first parameter
            if !has_trait {
                types.insert(0, "fn".to_string());
            }

            // Extract parameter types
            if let Some(params) = type_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    types.extend(extract_all_type_names_from_rust_type(param, content));
                }
            }

            // Extract return type
            if let Some(return_type) = type_node.child_by_field_name("return_type") {
                types.extend(extract_all_type_names_from_rust_type(return_type, content));
            }

            types
        }

        // Dynamic trait objects: dyn Display, dyn Trait + Send
        "dynamic_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Skip "dyn" keyword, extract trait bounds
                if child.kind() != "dyn" {
                    types.extend(extract_all_type_names_from_rust_type(child, content));
                }
            }
            types
        }

        // Abstract type (impl Trait): impl Display, impl Display + Clone
        // tree-sitter-rust 0.23.3 uses "abstract_type" for impl Trait
        "abstract_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Skip "impl" keyword, extract trait bounds
                types.extend(extract_all_type_names_from_rust_type(child, content));
            }
            types
        }


        // Qualified types / Associated types: <T as Iterator>::Item
        // Structure: qualified_type { type: <T as Iterator>, alias: Item }
        // The type field contains a bracketed_type with the trait bound
        "qualified_type" => {
            let mut types = Vec::new();

            // Extract base type with trait bound (e.g., <T as Iterator>)
            if let Some(type_node_inner) = type_node.child_by_field_name("type") {
                types.extend(extract_all_type_names_from_rust_type(type_node_inner, content));
            }

            // Extract associated type name (Item)
            // The alias field contains a scoped_type_identifier or simple identifier
            if let Some(alias_node) = type_node.child_by_field_name("alias") {
                types.extend(extract_all_type_names_from_rust_type(alias_node, content));
            }

            types
        }

        // Scoped type identifiers: std::Vec, crate::User, or <T as Iterator>::Item path
        // Extract the type name (last component) and optionally traverse path for bracketed types
        "scoped_type_identifier" => {
            let mut types = Vec::new();

            // Extract the name (last component)
            if let Some(name_node) = type_node.child_by_field_name("name")
                && let Ok(text) = name_node.utf8_text(content)
            {
                types.push(text.trim().to_string());
            }

            // Also traverse path if it contains bracketed_type (for associated types)
            // This handles cases like <T as Iterator>::Item where the path is <T as Iterator>
            if let Some(path_node) = type_node.child_by_field_name("path") {
                types.extend(extract_all_type_names_from_rust_type(path_node, content));
            }

            types
        }

        // Bracketed types: <T as Iterator>, used in qualified types
        // Recursively extract types from the bracketed content
        "bracketed_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_rust_type(child, content));
            }
            types
        }

        // Higher-ranked trait bounds: for<'a> Fn(&'a T)
        // Skip lifetime parameters, extract the bound types
        "higher_ranked_trait_bound" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Skip lifetime declarations, extract bound types
                if child.kind() != "lifetime" && child.kind() != "for_lifetimes" {
                    types.extend(extract_all_type_names_from_rust_type(child, content));
                }
            }
            types
        }

        // Skip types that don't reference other types
        "unit_type" |        // ()
        "never_type" |       // !
        "lifetime" |         // 'a
        "mutable_specifier" |  // mut keyword
        "self" => {          // self type
            Vec::new()
        }

        // Recursively handle any other wrapper nodes
        _ => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_rust_type(child, content));
            }
            types
        }
    }
}

/// Build `TypeOf` edges for a let declaration with type annotation.
///
/// Creates a Variable node and `TypeOf` edge to the primary type, plus Reference edges to all
/// extracted type names (including generic arguments, trait bounds, etc.).
///
/// # Example
///
/// ```rust,ignore
/// let users: Vec<User> = vec![];
/// ```
///
/// Creates:
/// - Variable node: "users"
/// - `TypeOf` edge: users → Vec (primary type)
/// - Reference edges: users → Vec, users → User
#[allow(clippy::unnecessary_wraps)]
fn build_let_declaration_typeof_edge(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract pattern (variable name)
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return Ok(());
    };

    // Extract type annotation
    let Some(type_node) = node.child_by_field_name("type") else {
        // No type annotation - type inference not in scope
        return Ok(());
    };

    // Get variable name from pattern
    let Some(var_name) = extract_identifier_from_pattern(pattern, content) else {
        return Ok(());
    };

    let span = span_from_node(pattern);

    // Create Variable node
    let var_id = helper.add_variable(var_name, Some(span));

    // Extract all type names from annotation
    let type_names = extract_all_type_names_from_rust_type(type_node, content);

    if type_names.is_empty() {
        return Ok(());
    }

    // Create TypeOf edge to primary (first) type
    // Example: let x: Vec<User> → TypeOf edge to Vec
    let primary_type = &type_names[0];
    let type_id = helper.add_type(primary_type.as_str(), Some(span));
    helper.add_typeof_edge(var_id, type_id);

    // Create Reference edges to all extracted types
    // Example: let x: Vec<User> → Reference edges to Vec and User
    for type_name in &type_names {
        let type_id = helper.add_type(type_name.as_str(), Some(span));
        helper.add_reference_edge(var_id, type_id);
    }

    Ok(())
}

/// Build `TypeOf` edges for function/method parameters with type annotations.
///
/// Creates Parameter nodes and `TypeOf` + Reference edges for each typed parameter.
/// Skips `self` parameters.
///
/// # Example
///
/// ```rust,ignore
/// fn process(user: &User, count: usize) { }
/// ```
///
/// Creates:
/// - Parameter nodes: "user", "count"
/// - `TypeOf` edges: user → User, count → usize
/// - Reference edges: user → User, count → usize
#[allow(clippy::unnecessary_wraps)]
fn build_parameter_typeof_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract pattern (parameter name)
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return Ok(());
    };

    // Extract type annotation
    let Some(type_node) = node.child_by_field_name("type") else {
        return Ok(());
    };

    // Get parameter name
    let Some(param_name) = extract_identifier_from_pattern(pattern, content) else {
        return Ok(());
    };

    // Skip 'self' parameters (self, &self, &mut self)
    if param_name == "self" {
        return Ok(());
    }

    let span = span_from_node(pattern);

    // Create Variable node for parameter
    let param_id = helper.add_variable(param_name, Some(span));

    // Extract all type names
    let type_names = extract_all_type_names_from_rust_type(type_node, content);

    if type_names.is_empty() {
        return Ok(());
    }

    // TypeOf edge to primary type
    let primary_type = &type_names[0];
    let type_id = helper.add_type(primary_type.as_str(), Some(span));
    helper.add_typeof_edge(param_id, type_id);

    // Reference edges to all types
    for type_name in &type_names {
        let type_id = helper.add_type(type_name.as_str(), Some(span));
        helper.add_reference_edge(param_id, type_id);
    }

    Ok(())
}

/// Resolve the qualified container name for a struct/enum-variant field.
///
/// Walks ancestors starting at the parent of `field_or_list_node` (which is
/// expected to be a `field_declaration_list` or `ordered_field_declaration_list`)
/// and returns the qualified name of the immediately enclosing container:
///
/// * named struct field — parent chain `field_declaration_list → struct_item`
///   yields `{module_path}::{Struct}` (per `qualify_item_name`).
/// * tuple struct field — parent chain `ordered_field_declaration_list →
///   struct_item` yields `{module_path}::{Struct}`.
/// * enum struct/tuple variant field — the immediate field-list parent is an
///   `enum_variant`; we walk one further to its `enum_variant_list → enum_item`
///   ancestor and return `{module_path}::{Enum}::{Variant}` (per design §4.6
///   AC-4 / AC-5; Rust `::` retained per §3.1.2).
///
/// Returns `None` for unanchored / unsupported AST shapes (defensive — should
/// not happen for grammar-valid Rust source).
fn resolve_field_container_qualified_name(
    field_or_list_node: Node<'_>,
    content: &[u8],
    file_module_path: Option<&str>,
) -> Option<String> {
    let parent = field_or_list_node.parent()?;

    match parent.kind() {
        // Named-struct case: parent is field_declaration_list,
        // grandparent is either struct_item or enum_variant.
        "field_declaration_list" => {
            let grand = parent.parent()?;
            qualified_name_for_container(grand, content, file_module_path)
        }
        // Tuple-struct / tuple-variant case: parent is the
        // ordered_field_declaration_list's parent — i.e., struct_item or
        // enum_variant directly.
        "struct_item" | "enum_variant" => {
            qualified_name_for_container(parent, content, file_module_path)
        }
        _ => None,
    }
}

fn qualified_name_for_container(
    node: Node<'_>,
    content: &[u8],
    file_module_path: Option<&str>,
) -> Option<String> {
    match node.kind() {
        // U11 follow-up: `union_item` is treated identically to `struct_item`
        // for field qualification — its `field_declaration_list` carries
        // ordinary `field_declaration` children whose qualified name must be
        // `{module_path}::{UnionName}::{field}` (Rust `::` retained per
        // design §3.1.2). Without this arm, union fields fell through to the
        // bare-name fallback and reintroduced the bare-name collision class
        // U11 was meant to remove (REQ:R0001..R0005, R0023).
        "struct_item" | "enum_item" | "union_item" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(content).ok()?.trim();
            if name.is_empty() {
                return None;
            }
            let qualified = qualify_item_name(node, name, content, file_module_path);
            if qualified.is_empty() {
                None
            } else {
                Some(qualified)
            }
        }
        "enum_variant" => {
            // Variant qualified name = {Enum}::{Variant} (with module prefix).
            let variant_name_node = node.child_by_field_name("name")?;
            let variant_name = variant_name_node.utf8_text(content).ok()?.trim();
            if variant_name.is_empty() {
                return None;
            }
            // Walk up: enum_variant → enum_variant_list → enum_item.
            let variant_list = node.parent()?;
            if variant_list.kind() != "enum_variant_list" {
                return None;
            }
            let enum_item = variant_list.parent()?;
            if enum_item.kind() != "enum_item" {
                return None;
            }
            let enum_name_node = enum_item.child_by_field_name("name")?;
            let enum_name = enum_name_node.utf8_text(content).ok()?.trim();
            if enum_name.is_empty() {
                return None;
            }
            let enum_qualified = qualify_item_name(enum_item, enum_name, content, file_module_path);
            if enum_qualified.is_empty() {
                None
            } else {
                // Rust `::` retained per design §3.1.2.
                Some(format!("{enum_qualified}::{variant_name}"))
            }
        }
        _ => None,
    }
}

/// Per-field visibility per design §3.3 row 6.
///
/// | Source              | Visibility string |
/// |---------------------|-------------------|
/// | `pub`               | `"public"`        |
/// | `pub(crate)`        | `"crate"`         |
/// | `pub(super)`        | `"super"`         |
/// | `pub(in <path>)`    | `"in:<path>"`     |
/// | absent (default)    | `"private"`       |
///
/// The `pub(in path)` form preserves the path so downstream consumers can
/// disambiguate; the return type carries an owned String for that case.
enum FieldVisibility {
    Static(&'static str),
    Owned(String),
}

impl FieldVisibility {
    fn as_str(&self) -> &str {
        match self {
            FieldVisibility::Static(s) => s,
            FieldVisibility::Owned(s) => s.as_str(),
        }
    }
}

fn classify_visibility_modifier_text(raw: &str) -> FieldVisibility {
    // Whitespace-collapse so `pub ( crate )` is recognized identically to
    // `pub(crate)`. The grammar typically emits the dense form, but parser
    // recovery occasionally inserts whitespace.
    let collapsed: String = raw.split_whitespace().collect();

    if collapsed == "pub" {
        FieldVisibility::Static("public")
    } else if collapsed == "pub(crate)" {
        FieldVisibility::Static("crate")
    } else if collapsed == "pub(super)" {
        FieldVisibility::Static("super")
    } else if let Some(rest) = collapsed.strip_prefix("pub(in") {
        let inner = rest.trim_end_matches(')').trim();
        if inner.is_empty() {
            FieldVisibility::Static("private")
        } else {
            FieldVisibility::Owned(format!("in:{inner}"))
        }
    } else {
        FieldVisibility::Static("private")
    }
}

fn extract_field_visibility(node: Node<'_>, content: &[u8]) -> FieldVisibility {
    let mut cursor = node.walk();
    let Some(vis_node) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier")
    else {
        return FieldVisibility::Static("private");
    };

    let Ok(vis_text_raw) = vis_node.utf8_text(content) else {
        return FieldVisibility::Static("private");
    };

    classify_visibility_modifier_text(vis_text_raw.trim())
}

/// Build the qualified Property node + `TypeOf{Field}` edge for a named
/// struct / enum-struct-variant field.
///
/// # Example
///
/// ```rust,ignore
/// pub struct Service {
///     pub repository: UserRepository,
///     cache: Arc<RwLock<HashMap<String, User>>>,
/// }
/// ```
///
/// Creates:
/// - Property nodes: `Service::repository` (visibility = `"public"`),
///   `Service::cache` (visibility = `"private"`).
/// - `TypeOf{Field, name="repository"}` edge: `Service::repository → UserRepository`.
/// - Reference edges to all extracted nested types.
#[allow(clippy::unnecessary_wraps)]
fn build_field_typeof_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    file_module_path: Option<&str>,
) -> GraphResult<()> {
    // Extract field name.
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(());
    };

    let Ok(field_name_raw) = name_node.utf8_text(content) else {
        return Ok(());
    };
    let field_name = field_name_raw.trim();
    if field_name.is_empty() {
        return Ok(());
    }

    let span = span_from_node(name_node);

    // Resolve enclosing struct / enum-variant qualified name (Rust `::`
    // retained per design §3.1.2). If the container can't be resolved
    // (defensive guard for malformed AST), fall back to bare-name emission so
    // we don't drop the field entirely.
    let qualified_field_name =
        resolve_field_container_qualified_name(node, content, file_module_path).map_or_else(
            || field_name.to_string(),
            |container| format!("{container}::{field_name}"),
        );

    // Visibility per design §3.3 row 6.
    let visibility = extract_field_visibility(node, content);

    // Property node: is_static = false (Rust struct fields are never
    // associated; design §3.4 row 6).
    let field_id = helper.add_property_with_static_and_visibility(
        &qualified_field_name,
        Some(span),
        false,
        Some(visibility.as_str()),
    );

    // Extract type annotation; absence is allowed (e.g. parse error recovery)
    // and means we still emit the Property node but no TypeOf/Reference edges.
    let Some(type_node) = node.child_by_field_name("type") else {
        return Ok(());
    };

    let type_names = extract_all_type_names_from_rust_type(type_node, content);
    if type_names.is_empty() {
        return Ok(());
    }

    // TypeOf{Field, name=BARE} edge to the primary (first) type. The edge
    // metadata uses the BARE field name; the node uses the qualified name
    // (REQ:R0003 + cross-language norm — see design §3.5).
    let primary_type = &type_names[0];
    let type_id = helper.add_type(primary_type.as_str(), Some(span));
    helper.add_typeof_edge_with_context(
        field_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(field_name),
    );

    // Reference edges to every extracted type (preserves prior behavior).
    for type_name in &type_names {
        let type_id = helper.add_type(type_name.as_str(), Some(span));
        helper.add_reference_edge(field_id, type_id);
    }

    Ok(())
}

/// Build qualified Property nodes + `TypeOf{Field}` edges for a tuple
/// struct's / enum tuple-variant's `ordered_field_declaration_list`.
///
/// Tuple-struct collision is resolved by anchoring each synthetic name on
/// the enclosing struct/variant qualified name (REQ:R0022): `Point(i32, i32)`
/// and `Vec(i32, i32)` produce four distinct `NodeIds` `Point::0`, `Point::1`,
/// `Vec::0`, `Vec::1` instead of the prior bare `0` / `1` collision.
///
/// AST structure:
/// ```text
/// ordered_field_declaration_list
///   visibility_modifier?  primitive_type (i32)
///   visibility_modifier?  primitive_type (i32)
/// ```
///
/// In tree-sitter-rust the `visibility_modifier` for an ordered field is a
/// preceding sibling at the same level — not a child of the type node — so
/// we walk all children and attach the most recently seen modifier to the
/// next type-bearing child.
#[allow(clippy::unnecessary_wraps)]
fn build_tuple_field_typeof_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    file_module_path: Option<&str>,
) -> GraphResult<()> {
    // Resolve enclosing struct / enum-variant qualified name. If unresolvable,
    // emit bare names so we don't drop the fields entirely (defensive).
    let container = resolve_field_container_qualified_name(node, content, file_module_path);

    let mut cursor = node.walk();
    let mut pending_visibility: Option<FieldVisibility> = None;
    let mut field_index: usize = 0;

    for child in node.named_children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            let raw = child.utf8_text(content).unwrap_or("").trim();
            pending_visibility = Some(classify_visibility_modifier_text(raw));
            continue;
        }

        // Skip attribute_item (and related non-type children); only treat the
        // remaining named children as type slots for tuple fields.
        if child.kind() == "attribute_item" {
            continue;
        }

        let span = span_from_node(child);
        let bare_name = field_index.to_string();

        let qualified_field_name = match &container {
            Some(c) => format!("{c}::{bare_name}"),
            None => bare_name.clone(),
        };

        let visibility = pending_visibility
            .take()
            .unwrap_or(FieldVisibility::Static("private"));

        let field_id = helper.add_property_with_static_and_visibility(
            &qualified_field_name,
            Some(span),
            false,
            Some(visibility.as_str()),
        );

        let type_names = extract_all_type_names_from_rust_type(child, content);
        if !type_names.is_empty() {
            // TypeOf{Field, name=BARE_INDEX} edge to the primary type.
            let primary_type = &type_names[0];
            let type_id = helper.add_type(primary_type.as_str(), Some(span));
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(&bare_name),
            );

            for type_name in &type_names {
                let type_id = helper.add_type(type_name.as_str(), Some(span));
                helper.add_reference_edge(field_id, type_id);
            }
        }

        field_index += 1;
    }

    Ok(())
}

/// Build Reference edges for type aliases to all referenced types.
///
/// Extracts all type names from the type alias RHS and creates Reference edges
/// from the alias to each referenced type.
///
/// Also extracts trait bounds from type parameters (e.g., `T: Trait`) and
/// creates Reference edges from the alias to the bound traits.
///
/// # Examples
///
/// ```rust,ignore
/// type MyResult<T> = Result<T, MyError>;
/// ```
///
/// Creates:
/// - Type node: `MyResult` (already created by existing `type_item` handler)
/// - Reference edges: `MyResult` → Result, `MyResult` → T, `MyResult` → `MyError`
///
/// ```rust,ignore
/// type Serializable<T: Serialize + Clone> = Result<T, Error>;
/// ```
///
/// Creates:
/// - Reference edges: Serializable → Result, Serializable → T, Serializable → Error
/// - Reference edges: Serializable → Serialize, Serializable → Clone (from bounds)
fn build_type_alias_reference_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    file_module_path: Option<&str>,
) -> GraphResult<()> {
    // Get type alias name
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(());
    };

    let Ok(alias_name) = name_node.utf8_text(content) else {
        return Ok(());
    };

    // Get the RHS type definition
    let Some(value_node) = node.child_by_field_name("type") else {
        return Ok(());
    };

    let span = span_from_node(name_node);

    // Qualify alias name same way as existing type_item handler
    let qualified_name = qualify_item_name(node, alias_name.trim(), content, file_module_path);

    if qualified_name.is_empty() {
        return Ok(());
    }

    // Get existing Type node for alias (created by type_item handler)
    let alias_id = helper.add_type(&qualified_name, Some(span));

    // Extract all referenced types from RHS
    let referenced_types = extract_all_type_names_from_rust_type(value_node, content);

    // Create Reference edges to all referenced types
    for type_name in referenced_types {
        let type_id = helper.add_type(&type_name, Some(span));
        helper.add_reference_edge(alias_id, type_id);
    }

    // Extract bounds from type_parameters (e.g., type Alias<T: Trait> = ...)
    // Per spec Phase 4: create Reference edges from alias to bound traits
    if let Some(type_params) = node.child_by_field_name("type_parameters") {
        let mut cursor = type_params.walk();
        for param_node in type_params.named_children(&mut cursor) {
            if param_node.kind() == "type_parameter" {
                // Look for bounded_type child which contains type bounds
                let mut param_cursor = param_node.walk();
                for child in param_node.named_children(&mut param_cursor) {
                    if child.kind() == "trait_bounds" {
                        // Extract all trait names from bounds
                        extract_trait_names_from_bounds(child, content, helper, alias_id, span)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build `TypeOf` edges for const/static items with type annotations.
///
/// Creates `TypeOf` edge to the primary type, plus Reference edges to all
/// extracted type names (including generic arguments, trait bounds, etc.).
///
/// # Example
///
/// ```rust,ignore
/// const MAX_SIZE: usize = 100;
/// static USERS: Vec<User> = Vec::new();
/// ```
///
/// Creates:
/// - `TypeOf` edge: `MAX_SIZE` → usize, USERS → Vec
/// - Reference edges: USERS → usize, USERS → Vec, USERS → User
#[allow(clippy::unnecessary_wraps)]
fn build_const_static_typeof_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    const_id: NodeId,
) -> GraphResult<()> {
    // Extract type annotation
    let Some(type_node) = node.child_by_field_name("type") else {
        // No type annotation - skip
        return Ok(());
    };

    let span = span_from_node(node);

    // Extract all type names from annotation
    let type_names = extract_all_type_names_from_rust_type(type_node, content);

    if type_names.is_empty() {
        return Ok(());
    }

    // Create TypeOf edge to primary (first) type
    // Example: const X: Vec<User> → TypeOf edge to Vec
    let primary_type = &type_names[0];
    let type_id = helper.add_type(primary_type.as_str(), Some(span));
    helper.add_typeof_edge(const_id, type_id);

    // Create Reference edges to all extracted types
    // Example: const X: Vec<User> → Reference edges to Vec and User
    for type_name in &type_names {
        let type_id = helper.add_type(type_name.as_str(), Some(span));
        helper.add_reference_edge(const_id, type_id);
    }

    Ok(())
}

/// Build Reference edges from a function/struct/impl to its trait bounds.
///
/// Extracts trait names from `type_parameters` and `where_clause`, then creates
/// Reference edges from the item to each referenced trait.
///
/// # Example
///
/// ```rust,ignore
/// fn compare<T: Display + Clone>(a: T, b: T) where T: Ord { }
/// ```
///
/// Creates Reference edges: compare → Display, compare → Clone, compare → Ord
fn build_trait_bound_reference_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    item_id: NodeId,
) -> GraphResult<()> {
    let span = span_from_node(node);

    // Extract trait bounds from type_parameters
    if let Some(type_params_node) = node.child_by_field_name("type_parameters") {
        let mut cursor = type_params_node.walk();
        for child in type_params_node.named_children(&mut cursor) {
            if child.kind() == "type_parameter" {
                // Look for trait_bounds within this type_parameter
                if let Some(bounds_node) = child.child_by_field_name("bound") {
                    extract_trait_names_from_bounds(bounds_node, content, helper, item_id, span)?;
                } else {
                    // Some grammars might not use "bound" field, traverse children
                    let mut param_cursor = child.walk();
                    for param_child in child.named_children(&mut param_cursor) {
                        if param_child.kind() == "trait_bounds" {
                            extract_trait_names_from_bounds(
                                param_child,
                                content,
                                helper,
                                item_id,
                                span,
                            )?;
                        }
                    }
                }
            }
        }
    }

    // Extract trait bounds from where_clause
    // Note: where_clause is not a field, it's a child node
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "where_clause" {
            let mut where_cursor = child.walk();
            for where_child in child.named_children(&mut where_cursor) {
                if where_child.kind() == "where_predicate" {
                    // Look for trait_bounds within this predicate
                    let mut pred_cursor = where_child.walk();
                    for pred_child in where_child.named_children(&mut pred_cursor) {
                        if pred_child.kind() == "trait_bounds" {
                            extract_trait_names_from_bounds(
                                pred_child, content, helper, item_id, span,
                            )?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Extract trait names from a `trait_bounds` node and create Reference edges.
#[allow(clippy::unnecessary_wraps)]
fn extract_trait_names_from_bounds(
    bounds_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    item_id: NodeId,
    span: Span,
) -> GraphResult<()> {
    // Use the existing extract_all_type_names_from_rust_type function
    // which already handles trait_bounds correctly
    let trait_names = extract_all_type_names_from_rust_type(bounds_node, content);

    for trait_name in trait_names {
        let trait_id = helper.add_type(&trait_name, Some(span));
        helper.add_reference_edge(item_id, trait_id);
    }

    Ok(())
}

// ============================================================================
// Generic Type Parameter Emission (REQ:R0029 / U20 — C2_GEN_TP_RUST)
// ============================================================================

/// Emit per-type-parameter `Type` nodes and `TypeOf{Constraint}` edges
/// for a Rust generic declaration (`function_item`, `trait_item`, or
/// `impl_item`).
///
/// Per design §3.1.2 the qualified-name form is
/// `<crate_path>::<DeclName>::<ParamName>` with `::` retained as Rust's
/// native path-segment separator (different from C# / Kotlin / Java
/// where the native source separator is `.` and gets canonicalised).
///
/// Tree-sitter-rust grammar shape (relevant fragments):
///
/// ```text
/// type_parameters:
///   '<' commaSep1(type_parameter | lifetime_parameter | const_parameter) '>'
/// type_parameter:
///   name: type_identifier
///   bound: trait_bounds                  // optional, inline bounds
///   default: ...                         // optional default
/// const_parameter:
///   'const' name: identifier ':' type: <type>
/// lifetime_parameter:
///   lifetime: lifetime
/// where_clause:
///   'where' commaSep1(where_predicate)
/// where_predicate:
///   left: type_identifier | scoped_type_identifier | lifetime | ...
///   bounds: trait_bounds
/// ```
///
/// Per AC-5 lifetime parameters are deliberately NOT emitted as `Type`
/// nodes — they remain on the `LifetimeConstraint` pipeline driven by
/// `lifetime_extractor`. Per AC-6 higher-ranked trait bounds
/// (`for<'a> Fn(&'a u32)`) are reference-only and out of scope here.
///
/// Per AC-4 const generic parameters (`const N: usize`) emit a base
/// `Type` node and one `Constraint` edge to the declared underlying
/// type (`usize`); the `const_generic` attribute itself is deferred
/// to a future `EdgeKind` extension.
fn process_type_parameter_declarations(
    decl_node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    let Some(params_node) = decl_node.child_by_field_name("type_parameters") else {
        return;
    };

    // Map parameter name -> NodeId so where-clause predicates can
    // target the right parameter Type node by identifier match.
    let mut param_ids: HashMap<String, NodeId> = HashMap::new();

    let mut params_cursor = params_node.walk();
    for param_node in params_node.named_children(&mut params_cursor) {
        match param_node.kind() {
            "type_parameter" => {
                emit_type_parameter_node(
                    param_node,
                    content,
                    parent_qualified_name,
                    helper,
                    &mut param_ids,
                );
            }
            "const_parameter" => {
                emit_const_parameter_node(
                    param_node,
                    content,
                    parent_qualified_name,
                    helper,
                    &mut param_ids,
                );
            }
            // AC-5: lifetime_parameter is NOT migrated — handled by
            // the existing LifetimeConstraint pipeline. Deliberately
            // skip without emitting a Type node.
            _ => {}
        }
    }

    // AC-3: walk where_clause siblings on the same declaration and
    // emit Constraint edges per where_predicate matching one of the
    // collected parameter names.
    let mut clause_cursor = decl_node.walk();
    for child in decl_node.named_children(&mut clause_cursor) {
        if child.kind() == "where_clause" {
            emit_where_clause_constraints(child, content, &param_ids, helper);
        }
    }
}

/// Emit one `type_parameter` node + inline-bound Constraint edges.
fn emit_type_parameter_node(
    param_node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    param_ids: &mut HashMap<String, NodeId>,
) {
    // Tree-sitter-rust exposes the parameter identifier as the first
    // named child of `type_parameter` (a `type_identifier` node). The
    // grammar does not provide a `name` field here.
    let mut cursor = param_node.walk();
    let mut name_node: Option<Node<'_>> = None;
    let mut bound_node: Option<Node<'_>> = None;
    for child in param_node.named_children(&mut cursor) {
        match child.kind() {
            "type_identifier" if name_node.is_none() => {
                name_node = Some(child);
            }
            "trait_bounds" => {
                bound_node = Some(child);
            }
            _ => {}
        }
    }
    let Some(name_node) = name_node else {
        return;
    };
    let Ok(param_name) = name_node.utf8_text(content) else {
        return;
    };
    let param_name = param_name.trim();
    if param_name.is_empty() {
        return;
    }

    // AC-2: qualified name uses `::` separator (Rust-native).
    let qualified = format!("{parent_qualified_name}::{param_name}");
    // Span anchored on the parameter identifier so LSP / MCP report a
    // concrete source location for "Find Definition" / hover.
    let span = span_from_node(name_node);
    let param_id = helper.add_type(&qualified, Some(span));
    param_ids.insert(param_name.to_string(), param_id);

    // AC-3: inline bounds (`T: Clone + Send`).
    if let Some(bounds) = bound_node {
        emit_constraint_edges_from_bounds(bounds, content, param_id, helper);
    }
}

/// Emit one `const_parameter` node + Constraint edge to its declared
/// underlying type. AC-4.
fn emit_const_parameter_node(
    param_node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    param_ids: &mut HashMap<String, NodeId>,
) {
    // Tree-sitter-rust grammar: const_parameter has `name` and `type`
    // fields.
    let Some(name_node) = param_node.child_by_field_name("name") else {
        return;
    };
    let Ok(param_name) = name_node.utf8_text(content) else {
        return;
    };
    let param_name = param_name.trim();
    if param_name.is_empty() {
        return;
    }

    let qualified = format!("{parent_qualified_name}::{param_name}");
    let span = span_from_node(name_node);
    let param_id = helper.add_type(&qualified, Some(span));
    param_ids.insert(param_name.to_string(), param_id);

    // AC-4: emit a Constraint edge to the declared underlying type.
    if let Some(type_node) = param_node.child_by_field_name("type") {
        let type_names = extract_all_type_names_from_rust_type(type_node, content);
        let type_span = span_from_node(type_node);
        for type_name in type_names {
            let type_id = helper.add_type(&type_name, Some(type_span));
            helper.add_typeof_edge_with_context(
                param_id,
                type_id,
                Some(TypeOfContext::Constraint),
                None,
                None,
            );
        }
    }
}

/// Walk a `where_clause` node and emit Constraint edges for each
/// `where_predicate` whose `left` field names one of the parameters
/// collected during type-parameter emission.
///
/// Lifetime predicates (`'a: 'b`) and predicates targeting unknown
/// names are skipped silently — the lifetime predicate flow stays on
/// the existing `LifetimeExtractor` pipeline (AC-5).
fn emit_where_clause_constraints(
    where_clause: Node<'_>,
    content: &[u8],
    param_ids: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = where_clause.walk();
    for predicate in where_clause.named_children(&mut cursor) {
        if predicate.kind() != "where_predicate" {
            continue;
        }
        let Some(left_node) = predicate.child_by_field_name("left") else {
            continue;
        };
        // Only emit Constraint edges when the predicate targets a
        // type identifier that matches one of our parameter names.
        // Scoped paths (`Self::Item`), associated types, and lifetime
        // predicates fall through.
        if left_node.kind() != "type_identifier" {
            continue;
        }
        let Ok(left_name) = left_node.utf8_text(content) else {
            continue;
        };
        let left_name = left_name.trim();
        let Some(&param_id) = param_ids.get(left_name) else {
            continue;
        };
        let Some(bounds_node) = predicate.child_by_field_name("bounds") else {
            continue;
        };
        emit_constraint_edges_from_bounds(bounds_node, content, param_id, helper);
    }
}

/// Emit one `TypeOf{Constraint}` edge per trait bound in a
/// `trait_bounds` node.
///
/// Reuses `extract_constraint_type_names_from_rust_type` for bound-name
/// extraction so generic trait bounds (`Iterator<Item = T>`,
/// `Fn(u32) -> u32`) decompose into their constituent type names. Unlike
/// the Reference-edge extractor, this variant skips
/// `higher_ranked_trait_bound` subtrees so HRTBs (e.g. `for<'a> Fn(&'a u32)`)
/// never leak Constraint edges (AC-6: HRTBs reference-only). The
/// existing Reference-edge pipeline still emits References for HRTB
/// constituents elsewhere.
fn emit_constraint_edges_from_bounds(
    bounds_node: Node<'_>,
    content: &[u8],
    param_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let span = span_from_node(bounds_node);
    let trait_names = extract_constraint_type_names_from_rust_type(bounds_node, content);
    for trait_name in trait_names {
        let trait_id = helper.add_type(&trait_name, Some(span));
        helper.add_typeof_edge_with_context(
            param_id,
            trait_id,
            Some(TypeOfContext::Constraint),
            None,
            None,
        );
    }
}

/// Constraint-pipeline variant of `extract_all_type_names_from_rust_type`
/// that excludes `higher_ranked_trait_bound` subtrees.
///
/// AC-6 mandates that HRTBs (`for<'a> Fn(&'a T)`) on type-parameter
/// bounds remain reference-only — a `T: for<'a> Fn(&'a u32)` bound
/// must NOT emit `TypeOf{Constraint}` edges from `T` to `Fn` or `u32`.
/// The Reference-edge pipeline (`extract_all_type_names_from_rust_type`)
/// still recurses into HRTB nodes, so References on HRTB constituents
/// are unaffected.
///
/// Other shapes — generic types, function types, `trait_bounds` (the
/// `+`-separated list itself), tuples, references, arrays, etc. — are
/// extracted exactly as the Reference variant does.
//
// Match arms for distinct AST node kinds (`tuple_type` / `bounded_type` /
// `trait_bounds` vs `dynamic_type` / `abstract_type`) are kept separate
// so the per-arm comments document each shape, even though the bodies
// happen to be identical.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
fn extract_constraint_type_names_from_rust_type(
    type_node: Node<'_>,
    content: &[u8],
) -> Vec<String> {
    // Skip HRTBs entirely — AC-6 keeps them reference-only.
    if type_node.kind() == "higher_ranked_trait_bound" {
        return Vec::new();
    }

    match type_node.kind() {
        // Base case: simple type identifier
        "type_identifier" | "primitive_type" => {
            if let Ok(text) = type_node.utf8_text(content) {
                vec![text.trim().to_string()]
            } else {
                Vec::new()
            }
        }

        // Generic types: Vec<T>, HashMap<K, V>, Iterator<Item = T>
        "generic_type" => {
            let mut types = Vec::new();
            if let Some(base_type) = type_node.child_by_field_name("type") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    base_type, content,
                ));
            }
            if let Some(type_args) = type_node.child_by_field_name("type_arguments") {
                let mut cursor = type_args.walk();
                for child in type_args.named_children(&mut cursor) {
                    types.extend(extract_constraint_type_names_from_rust_type(child, content));
                }
            }
            types
        }

        // Reference types: &T, &mut T
        "reference_type" => {
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() != "lifetime" && child.kind() != "mutable_specifier" {
                    return extract_constraint_type_names_from_rust_type(child, content);
                }
            }
            Vec::new()
        }

        // Pointer types: *const T, *mut T
        "pointer_type" => {
            if let Some(inner_type) = type_node.child_by_field_name("type") {
                extract_constraint_type_names_from_rust_type(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Array types: [T; N]
        "array_type" => {
            if let Some(element_type) = type_node.child_by_field_name("element") {
                extract_constraint_type_names_from_rust_type(element_type, content)
            } else {
                Vec::new()
            }
        }

        // Trait-bound list (`Display + Clone + Send`), tuples, bounded types.
        // Iteration applies the HRTB-skipping rule to each child individually,
        // so `T: Clone + for<'a> Fn(&'a u32)` keeps `Clone` while dropping the
        // HRTB subtree.
        "tuple_type" | "bounded_type" | "trait_bounds" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_constraint_type_names_from_rust_type(child, content));
            }
            types
        }

        // Function types: `Fn(T) -> U`, `FnMut(T) -> U`, `FnOnce(T) -> U`,
        // bare `fn(T) -> U`. Mirror the Reference extractor: emit the trait
        // name (or synthetic `fn` marker) and recurse into parameters /
        // return type, with HRTB-skipping carried through recursion.
        "function_type" => {
            let mut types = Vec::new();
            let has_trait = if let Some(trait_node) = type_node.child_by_field_name("trait") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    trait_node, content,
                ));
                true
            } else {
                false
            };
            if !has_trait {
                types.insert(0, "fn".to_string());
            }
            if let Some(parameters) = type_node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for param in parameters.named_children(&mut cursor) {
                    types.extend(extract_constraint_type_names_from_rust_type(param, content));
                }
            }
            if let Some(return_type) = type_node.child_by_field_name("return_type") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    return_type,
                    content,
                ));
            }
            types
        }

        // Dynamic/impl trait wrappers: `dyn Trait`, `impl Trait`.
        "dynamic_type" | "abstract_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_constraint_type_names_from_rust_type(child, content));
            }
            types
        }

        // Removed-trait wrapper: `?Sized`. Recurse to surface the inner
        // trait identifier.
        "removed_trait_bound" => {
            let mut types = Vec::new();
            if let Some(type_node_inner) = type_node.child_by_field_name("type") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    type_node_inner,
                    content,
                ));
            }
            types
        }

        // Type aliases inside bounds: extract the alias.
        "type_binding" => {
            let mut types = Vec::new();
            if let Some(alias_node) = type_node.child_by_field_name("alias") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    alias_node, content,
                ));
            }
            types
        }

        // Scoped type identifiers: `std::Vec`, `<T as Iterator>::Item`.
        "scoped_type_identifier" => {
            let mut types = Vec::new();
            if let Some(name_node) = type_node.child_by_field_name("name")
                && let Ok(text) = name_node.utf8_text(content)
            {
                types.push(text.trim().to_string());
            }
            if let Some(path_node) = type_node.child_by_field_name("path") {
                types.extend(extract_constraint_type_names_from_rust_type(
                    path_node, content,
                ));
            }
            types
        }

        // Bracketed types: `<T as Iterator>` qualified-path prefixes.
        "bracketed_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_constraint_type_names_from_rust_type(child, content));
            }
            types
        }

        // Skip non-type leaves.
        "unit_type" | "never_type" | "lifetime" | "mutable_specifier" | "self" => Vec::new(),

        // Fall through: recurse into named children, preserving HRTB skip.
        _ => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_constraint_type_names_from_rust_type(child, content));
            }
            types
        }
    }
}

/// Qualify a simple item name with surrounding module context.
///
/// Only qualifies simple identifiers. Already-qualified paths (containing ::,
/// or starting with `crate/self/super/::`) are returned as-is to preserve
/// their original semantics.
///
/// # Arguments
///
/// * `node` - AST node for the item being qualified
/// * `name` - Simple name of the item (e.g., "helper")
/// * `content` - Source file content for text extraction
/// * `file_module_path` - File-level module path (e.g., `Some("extra")` for `src/extra.rs`)
///
/// # Returns
///
/// Fully qualified name including inline module context and file module path.
/// For example, `helper` in `src/extra.rs` becomes `extra::helper`.
fn qualify_item_name(
    node: Node<'_>,
    name: &str,
    content: &[u8],
    file_module_path: Option<&str>,
) -> String {
    // Don't qualify already-qualified paths
    if is_already_qualified(name) {
        return name.to_string();
    }

    let mut parts = vec![name.to_string()];
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "mod_item"
            && let Some(mod_name) = parent.child_by_field_name("name")
            && let Ok(text) = mod_name.utf8_text(content)
        {
            let t = text.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
        current = parent;
    }

    // Add file module path as outermost prefix
    if let Some(file_mod) = file_module_path {
        parts.push(file_mod.to_string());
    }

    parts.reverse();
    parts.join("::")
}

/// File-level module name for exports/imports.
/// Distinct from `<toplevel>` to avoid node kind collision in `GraphBuildHelper` cache.
const FILE_MODULE_NAME: &str = "<file_module>";

fn export_from_file_module(
    helper: &mut GraphBuildHelper,
    exported: sqry_core::graph::unified::node::NodeId,
) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

// ========== ASTGraph: Pre-computed AST metadata ==========

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CallContext {
    pub name: Arc<str>,
    pub qualified_name: String,
    pub span: (usize, usize),
    pub is_async: bool,
    pub is_unsafe: bool,
    pub is_method: bool,
}

impl CallContext {
    #[must_use]
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
}

pub struct ASTGraph {
    /// Maps node ID to its enclosing callable node ID
    callable_map: HashMap<usize, usize>,
    /// Maps callable node ID to its context (name, scope, etc.)
    context_map: HashMap<usize, CallContext>,
}

impl ASTGraph {
    /// Build the graph structure from the AST in a single O(n) pass
    ///
    /// # Errors
    ///
    /// Returns an error when tree traversal fails due to invalid UTF-8 inside
    /// the inspected nodes (propagated from `utf8_text` calls).
    pub fn from_tree(tree: &Tree, content: &[u8], max_scope_depth: usize) -> Result<Self, String> {
        let mut builder = ASTGraphBuilder::new(content, max_scope_depth);

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        builder
            .visit(tree.root_node(), None, &mut guard)
            .map_err(|e| format!("Rust AST traversal hit recursion limit: {e}"))?;
        Ok(builder.build())
    }

    /// Get the enclosing callable context for a node (O(1) lookup)
    #[must_use]
    pub fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        let callable_id = self.callable_map.get(&node_id)?;
        self.context_map.get(callable_id)
    }

    /// Get all callable contexts
    pub fn contexts(&self) -> impl Iterator<Item = &CallContext> {
        self.context_map.values()
    }
}

#[allow(dead_code)]
struct ASTGraphBuilder<'a> {
    content: &'a [u8],
    max_scope_depth: usize,
    callable_map: HashMap<usize, usize>,
    context_map: HashMap<usize, CallContext>,
    current_callable: Option<usize>,
    current_scope: Vec<Arc<str>>,
    current_impl_type: Option<Arc<str>>,
}

impl<'a> ASTGraphBuilder<'a> {
    fn new(content: &'a [u8], max_scope_depth: usize) -> Self {
        Self {
            content,
            max_scope_depth,
            callable_map: HashMap::new(),
            context_map: HashMap::new(),
            current_callable: None,
            current_scope: Vec::new(),
            current_impl_type: None,
        }
    }

    fn build(self) -> ASTGraph {
        ASTGraph {
            callable_map: self.callable_map,
            context_map: self.context_map,
        }
    }

    /// # Errors
    ///
    /// Returns [`sqry_core::query::security::RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
    fn visit(
        &mut self,
        node: Node<'_>,
        parent_callable: Option<usize>,
        guard: &mut sqry_core::query::security::RecursionGuard,
    ) -> Result<(), sqry_core::query::security::RecursionError> {
        guard.enter()?;

        let node_id = node.id();

        // Track impl blocks to determine current type context
        if node.kind() == "impl_item"
            && let Some(type_node) = node.child_by_field_name("type")
            && let Ok(type_text) = type_node.utf8_text(self.content)
        {
            let old_impl_type = self.current_impl_type.clone();
            self.current_impl_type = Some(Arc::from(type_text.trim()));

            // Visit children with impl context
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.visit(child, parent_callable, guard)?;
            }

            // Restore previous impl context
            self.current_impl_type = old_impl_type;
            guard.exit();
            return Ok(());
        }

        // Check if this node is a callable (function, method, closure)
        let callable_name = callable_node_name(node, self.content);

        let new_callable = if let Some(name) = callable_name {
            let start = node.start_byte();
            let end = node.end_byte();
            let is_async = is_async_function(node, self.content);
            let is_unsafe = is_unsafe_function(node, self.content);
            let is_method = self.current_impl_type.is_some();

            // Build qualified name with impl type context and module scope
            let qualified_name = if let Some(impl_type) = &self.current_impl_type {
                // Method inside impl block: include module scope if present
                // e.g., foo::bar::Type::method instead of just Type::method
                if self.current_scope.is_empty() {
                    format!("{impl_type}::{name}")
                } else {
                    format!("{}::{}::{}", self.current_scope.join("::"), impl_type, name)
                }
            } else if self.current_scope.is_empty() {
                name.clone()
            } else if self.current_scope.len() <= self.max_scope_depth {
                format!("{}::{}", self.current_scope.join("::"), name)
            } else {
                // Truncate deep scopes
                let truncated = &self.current_scope[..self.max_scope_depth];
                format!("{}::{}", truncated.join("::"), name)
            };

            let context = CallContext {
                name: Arc::from(name),
                qualified_name,
                span: (start, end),
                is_async,
                is_unsafe,
                is_method,
            };

            self.context_map.insert(node_id, context);
            Some(node_id)
        } else {
            None
        };

        // Use new callable context if we entered one, otherwise inherit parent's
        let effective_callable = new_callable.or(parent_callable);

        // Map this node to its enclosing callable
        if let Some(callable_id) = effective_callable {
            self.callable_map.insert(node_id, callable_id);
        }

        // Handle scope tracking (structs, enums, modules, etc.)
        let scope_name = scope_node_name(node, self.content);
        let pushed_scope = if let Some(name) = scope_name {
            self.current_scope.push(Arc::from(name));
            true
        } else {
            false
        };

        // Recursively visit children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, effective_callable, guard)?;
        }

        // Pop scope if we pushed one
        if pushed_scope {
            self.current_scope.pop();
        }

        guard.exit();
        Ok(())
    }
}

/// Check if a node represents a callable (function, method, closure, etc.)
fn callable_node_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "function_item" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string())),
        "closure_expression" => Some(SyntheticNameBuilder::from_node(&node, content, "closure")),
        _ => None,
    }
}

fn scope_node_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "struct_item" | "enum_item" | "trait_item" | "type_item" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string())),
        "mod_item" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string())),
        _ => None,
    }
}

fn is_async_function(node: Node<'_>, _content: &[u8]) -> bool {
    // Check if function has async modifier
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| {
        child.kind() == "async" || {
            // async can be inside function_modifiers
            if child.kind() == "function_modifiers" {
                let mut mod_cursor = child.walk();
                child
                    .children(&mut mod_cursor)
                    .any(|modifier| modifier.kind() == "async")
            } else {
                false
            }
        }
    })
}

fn is_unsafe_function(node: Node<'_>, _content: &[u8]) -> bool {
    // Check if function has unsafe modifier
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| {
        child.kind() == "unsafe" || {
            // unsafe can be inside function_modifiers
            if child.kind() == "function_modifiers" {
                let mut mod_cursor = child.walk();
                child
                    .children(&mut mod_cursor)
                    .any(|modifier| modifier.kind() == "unsafe")
            } else {
                false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn parse_rust(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("Error loading Rust grammar");
        parser.parse(source, None).expect("Error parsing")
    }

    #[test]
    fn test_builder_language() {
        let builder = RustGraphBuilder::default();
        assert_eq!(builder.language(), Language::Rust);
    }

    #[test]
    fn test_pub_method_emits_export_edge() {
        fn find_function_item_named<'a>(
            node: Node<'a>,
            content: &[u8],
            name: &str,
        ) -> Option<Node<'a>> {
            if node.kind() == "function_item"
                && let Some(name_node) = node.child_by_field_name("name")
                && let Ok(text) = name_node.utf8_text(content)
                && text.trim() == name
            {
                return Some(node);
            }

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_function_item_named(child, content, name) {
                    return Some(found);
                }
            }
            None
        }

        let source = r"
pub struct User {
    name: String,
}

impl User {
    pub fn get_name(&self) -> &str {
        &self.name
    }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        let func_node = find_function_item_named(tree.root_node(), source.as_bytes(), "get_name")
            .expect("expected to find get_name function_item");
        assert!(
            is_unrestricted_pub(func_node, source.as_bytes()),
            "expected get_name to be unrestricted pub"
        );

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("graph build should succeed");

        let ops = staging.operations();

        let mut string_table: HashMap<sqry_core::graph::unified::string::StringId, String> =
            HashMap::new();
        for op in ops {
            if let sqry_core::graph::unified::build::staging::StagingOp::InternString {
                local_id,
                value,
            } = op
            {
                string_table
                    .entry(*local_id)
                    .or_insert_with(|| value.clone());
            }
        }

        let resolve_string = |id: sqry_core::graph::unified::string::StringId| -> Option<&str> {
            string_table.get(&id).map(String::as_str)
        };

        let mut node_names: HashMap<sqry_core::graph::unified::node::NodeId, String> =
            HashMap::new();
        for op in ops {
            if let sqry_core::graph::unified::build::staging::StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
            {
                let name = entry
                    .qualified_name
                    .and_then(&resolve_string)
                    .or_else(|| resolve_string(entry.name));
                if let Some(name_str) = name {
                    node_names.insert(*node_id, name_str.to_string());
                }
            }
        }

        let mut exported_targets = Vec::new();
        for op in ops {
            if let sqry_core::graph::unified::build::staging::StagingOp::AddEdge {
                target,
                kind,
                ..
            } = op
                && matches!(kind, sqry_core::graph::unified::EdgeKind::Exports { .. })
                && let Some(name) = node_names.get(target)
            {
                exported_targets.push(name.clone());
            }
        }

        assert!(
            exported_targets.iter().any(|name| name == "User::get_name"),
            "expected exported targets to include User::get_name, got: {exported_targets:?}"
        );
    }

    #[test]
    fn test_simple_function_call() {
        let source = r#"
fn main() {
    println!("hello");
    foo();
}

fn foo() {}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_method_call() {
        let source = r"
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new() -> Self {
        Point { x: 0, y: 0 }
    }

    fn distance(&self) -> f64 {
        self.magnitude()
    }

    fn magnitude(&self) -> f64 {
        0.0
    }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_macro_invocation() {
        let source = r#"
fn main() {
    println!("hello");
    vec![1, 2, 3];
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_use_declaration() {
        let source = r"
use std::collections::HashMap;
use std::io::{Read, Write};
use std::fs::*;
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_async_function() {
        let source = r"
async fn fetch_data() -> String {
    let result = read_file().await;
    result
}

async fn read_file() -> String {
    String::new()
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_extern_crate() {
        let source = r"
extern crate serde;
extern crate tokio as async_runtime;
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_ffi_extern_block() {
        let source = r#"
extern "C" {
    fn printf(fmt: *const i8, ...) -> i32;
    fn malloc(size: usize) -> *mut u8;
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    #[test]
    fn test_self_method_resolution() {
        let source = r"
impl Widget {
    fn process(&self) {
        self.render();
    }

    fn render(&self) {}
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verifies that graph building completes without error
    }

    // =========================================================================
    // OOP Edge Tests (impl Trait for Type)
    // =========================================================================

    use sqry_core::graph::unified::EdgeKind as UnifiedEdgeKind;
    use sqry_core::graph::unified::build::staging::StagingOp;

    /// Helper: Count Implements edges in staging operations
    fn count_implements_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: UnifiedEdgeKind::Implements,
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper: Count Export edges in staging operations
    fn count_export_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: UnifiedEdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper: Count `FfiCall` edges in staging operations
    fn count_ffi_call_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: UnifiedEdgeKind::FfiCall { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper: Check if a string with given pattern exists in interned strings
    fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::InternString { value, .. } = op {
                value.contains(pattern)
            } else {
                false
            }
        })
    }

    /// Test: Basic trait implementation creates Implements edge
    #[test]
    fn test_impl_trait_creates_implements_edge() {
        let source = r#"
trait Display {
    fn display(&self) -> String;
}

struct MyType {
    value: i32,
}

impl Display for MyType {
    fn display(&self) -> String {
        format!("{}", self.value)
    }
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert!(
            implements_count > 0,
            "Should have Implements edge for 'impl Display for MyType'"
        );

        // Verify both nodes exist (via interned strings)
        assert!(
            has_interned_string_containing(&staging, "MyType"),
            "Should have MyType node"
        );
        assert!(
            has_interned_string_containing(&staging, "Display"),
            "Should have Display node"
        );
    }

    /// Test: Multiple trait implementations create multiple Implements edges
    #[test]
    fn test_multiple_trait_impls_create_multiple_edges() {
        let source = r"
trait Display {
    fn display(&self) -> String;
}

trait Debug {
    fn debug(&self) -> String;
}

trait Clone {
    fn clone(&self) -> Self;
}

struct MyType;

impl Display for MyType {
    fn display(&self) -> String { String::new() }
}

impl Debug for MyType {
    fn debug(&self) -> String { String::new() }
}

impl Clone for MyType {
    fn clone(&self) -> Self { MyType }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edges
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 3,
            "Should have 3 Implements edges for Display, Debug, Clone"
        );
    }

    /// Test: Inherent impl (impl Type { ... }) does NOT create OOP edges
    #[test]
    fn test_inherent_impl_no_oop_edge() {
        let source = r"
struct MyType {
    value: i32,
}

impl MyType {
    fn new() -> Self {
        MyType { value: 0 }
    }

    fn get_value(&self) -> i32 {
        self.value
    }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edges - should be none
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 0,
            "Inherent impl should NOT create Implements edges, found {implements_count}"
        );
    }

    /// Test: Trait impl in nested module has qualified names
    #[test]
    fn test_trait_impl_in_module() {
        let source = r"
mod inner {
    trait Renderable {
        fn render(&self);
    }

    struct Widget;

    impl Renderable for Widget {
        fn render(&self) {}
    }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 1,
            "Should have 1 Implements edge in nested module"
        );

        // Check that nodes have qualified names with module prefix
        assert!(
            has_interned_string_containing(&staging, "inner::Widget"),
            "Widget should have module-qualified name"
        );
    }

    /// Test: Generic trait implementation
    #[test]
    fn test_generic_trait_impl() {
        let source = r"
trait From<T> {
    fn from(value: T) -> Self;
}

struct MyString(String);

impl From<&str> for MyString {
    fn from(value: &str) -> Self {
        MyString(value.to_string())
    }
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge - should handle generics
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 1,
            "Should have 1 Implements edge for generic trait impl"
        );
    }

    /// Test: impl for external/qualified trait preserves trait path
    #[test]
    fn test_impl_qualified_trait_preserves_path() {
        let source = r#"
struct MyType;

impl std::fmt::Display for MyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MyType")
    }
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 1,
            "Should have 1 Implements edge for std::fmt::Display"
        );

        // The trait name should preserve the full path, not prepend module names
        assert!(
            has_interned_string_containing(&staging, "std::fmt::Display"),
            "Should have std::fmt::Display trait node (not qualified with local module)"
        );
        assert!(
            has_interned_string_containing(&staging, "MyType"),
            "Should have MyType struct node"
        );
    }

    /// Test: impl `crate::Trait` preserves crate-relative path
    #[test]
    fn test_impl_crate_relative_trait_preserves_path() {
        let source = r"
mod inner {
    struct MyType;

    impl crate::MyTrait for MyType {}
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert_eq!(
            implements_count, 1,
            "Should have 1 Implements edge for crate::MyTrait"
        );

        // The trait path should be crate::MyTrait, NOT inner::crate::MyTrait
        assert!(
            has_interned_string_containing(&staging, "crate::MyTrait"),
            "Should preserve crate::MyTrait path without module prefix"
        );
    }

    /// Test: impl for qualified type preserves type path
    #[test]
    fn test_impl_qualified_type_preserves_path() {
        let source = r"
trait MyTrait {}

impl MyTrait for foo::bar::MyType {}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert_eq!(implements_count, 1, "Should have 1 Implements edge");

        // The type path should be preserved
        assert!(
            has_interned_string_containing(&staging, "foo::bar::MyType"),
            "Should preserve foo::bar::MyType path"
        );
    }

    // =========================================================================
    // FFI Edge Tests (extern "C" blocks)
    // =========================================================================

    /// Test: extern "C" block creates FFI function nodes
    #[test]
    fn test_extern_c_block_creates_ffi_functions() {
        let source = r#"
extern "C" {
    fn printf(fmt: *const i8, ...) -> i32;
    fn malloc(size: usize) -> *mut u8;
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for FFI function nodes
        assert!(
            has_interned_string_containing(&staging, "extern::C::printf"),
            "Should have printf FFI function"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::malloc"),
            "Should have malloc FFI function"
        );
    }

    /// Test: extern "C" block with static variables
    #[test]
    fn test_extern_c_block_static_variables() {
        let source = r#"
extern "C" {
    static errno: i32;
    static mut environ: *mut *mut i8;
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for FFI static nodes
        assert!(
            has_interned_string_containing(&staging, "extern::C::errno"),
            "Should have errno FFI static"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::environ"),
            "Should have environ FFI static"
        );
    }

    /// Test: extern block with different ABI
    #[test]
    fn test_extern_system_abi() {
        let source = r#"
extern "system" {
    fn GetLastError() -> u32;
    fn SetLastError(code: u32);
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for FFI function nodes with "system" ABI
        assert!(
            has_interned_string_containing(&staging, "extern::system::GetLastError"),
            "Should have GetLastError with system ABI"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::system::SetLastError"),
            "Should have SetLastError with system ABI"
        );
    }

    /// Test: Mixed FFI functions and statics in same block
    #[test]
    fn test_mixed_ffi_block() {
        let source = r#"
extern "C" {
    fn open(path: *const i8, flags: i32) -> i32;
    static STDIN_FILENO: i32;
    fn close(fd: i32) -> i32;
    static STDOUT_FILENO: i32;
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for both functions and statics
        assert!(
            has_interned_string_containing(&staging, "extern::C::open"),
            "Should have open FFI function"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::close"),
            "Should have close FFI function"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::STDIN_FILENO"),
            "Should have STDIN_FILENO FFI static"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::STDOUT_FILENO"),
            "Should have STDOUT_FILENO FFI static"
        );
    }

    /// Test: FFI functions are exported
    #[test]
    fn test_ffi_functions_are_exported() {
        let source = r#"
extern "C" {
    fn libc_function();
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Export edge
        let export_count = count_export_edges(&staging);
        assert!(export_count > 0, "FFI functions should be exported");
    }

    // =========================================================================
    // Combined OOP and FFI Tests
    // =========================================================================

    /// Test: Both OOP and FFI in same file
    #[test]
    fn test_combined_oop_and_ffi() {
        let source = r#"
trait NativeInterface {
    fn call_native(&self);
}

struct Wrapper {
    handle: *mut std::ffi::c_void,
}

impl NativeInterface for Wrapper {
    fn call_native(&self) {
        unsafe { native_call(self.handle); }
    }
}

extern "C" {
    fn native_call(handle: *mut std::ffi::c_void);
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge
        let implements_count = count_implements_edges(&staging);
        assert_eq!(implements_count, 1, "Should have 1 Implements edge");

        // Check for FFI function
        assert!(
            has_interned_string_containing(&staging, "native_call"),
            "Should have native_call FFI function"
        );
    }

    // =========================================================================
    // FFI Call Linking Tests
    // =========================================================================

    /// Test: Calling an extern function creates an `FfiCall` edge
    #[test]
    fn test_ffi_call_creates_ffi_call_edge() {
        let source = r#"
extern "C" {
    fn printf(format: *const i8, ...) -> i32;
}

fn main() {
    unsafe {
        printf(b"hello\0".as_ptr() as *const i8);
    }
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have the FFI function declared
        assert!(
            has_interned_string_containing(&staging, "extern::C::printf"),
            "Should have extern::C::printf FFI function"
        );

        // Should create an FfiCall edge from main to printf
        let ffi_call_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_call_count, 1,
            "Should have 1 FfiCall edge from main to printf"
        );
    }

    /// Test: Multiple FFI calls create multiple `FfiCall` edges
    #[test]
    fn test_multiple_ffi_calls() {
        let source = r#"
extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

fn allocate_and_free() {
    unsafe {
        let ptr = malloc(100);
        free(ptr);
    }
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have both FFI functions declared
        assert!(
            has_interned_string_containing(&staging, "extern::C::malloc"),
            "Should have extern::C::malloc"
        );
        assert!(
            has_interned_string_containing(&staging, "extern::C::free"),
            "Should have extern::C::free"
        );

        // Should create 2 FfiCall edges
        let ffi_call_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_call_count, 2,
            "Should have 2 FfiCall edges (malloc + free)"
        );
    }

    /// Test: extern "system" convention creates proper `FfiCall`
    #[test]
    fn test_ffi_system_convention() {
        let source = r#"
extern "system" {
    fn LoadLibraryA(name: *const i8) -> *mut u8;
}

fn load() {
    unsafe {
        LoadLibraryA(b"kernel32.dll\0".as_ptr() as *const i8);
    }
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have the FFI function with system ABI
        assert!(
            has_interned_string_containing(&staging, "extern::system::LoadLibraryA"),
            "Should have extern::system::LoadLibraryA FFI function"
        );

        // Should create an FfiCall edge
        let ffi_call_count = count_ffi_call_edges(&staging);
        assert_eq!(ffi_call_count, 1, "Should have 1 FfiCall edge");
    }

    /// Test: Non-FFI function calls don't create `FfiCall` edges
    #[test]
    fn test_regular_call_not_ffi_call() {
        let source = r"
fn helper() {}

fn main() {
    helper();
}
";
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should NOT create any FfiCall edges
        let ffi_call_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_call_count, 0,
            "Regular function calls should not create FfiCall edges"
        );
    }

    /// Test: Combined OOP and FFI calls with `FfiCall` edge verification
    #[test]
    fn test_combined_oop_and_ffi_call_linking() {
        let source = r#"
trait NativeInterface {
    fn call_native(&self);
}

struct Wrapper {
    handle: *mut std::ffi::c_void,
}

impl NativeInterface for Wrapper {
    fn call_native(&self) {
        unsafe { native_call(self.handle); }
    }
}

extern "C" {
    fn native_call(handle: *mut std::ffi::c_void);
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check for Implements edge (OOP)
        let implements_count = count_implements_edges(&staging);
        assert_eq!(implements_count, 1, "Should have 1 Implements edge");

        // Check for FfiCall edge (FFI call linking)
        let ffi_call_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_call_count, 1,
            "Should have 1 FfiCall edge from call_native to native_call"
        );
    }

    /// Test: Confidence metadata is stored in the staging graph
    #[test]
    fn test_confidence_metadata_stored() {
        let source = r#"
fn simple_function() {
    println!("Hello, world!");
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let builder = RustGraphBuilder::default();
        let file = PathBuf::from("test.rs");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("graph build should succeed");

        // Verify confidence metadata was stored
        let confidence = staging
            .confidence()
            .expect("confidence metadata should be set");

        // For AST-only analysis (default config without rust-analyzer),
        // we expect AstOnly or Partial confidence level
        assert!(
            matches!(
                confidence.level,
                sqry_core::confidence::ConfidenceLevel::AstOnly
                    | sqry_core::confidence::ConfidenceLevel::Partial
            ),
            "Expected AstOnly or Partial confidence, got {:?}",
            confidence.level
        );
    }

    #[test]
    fn test_extract_path_from_attr_text_standard() {
        let attr = r#"#[path = "custom.rs"]"#;
        let result = extract_path_from_attr_text(attr);
        assert_eq!(result, Some("custom.rs".to_string()));
    }

    #[test]
    fn test_extract_path_from_attr_text_no_spaces() {
        let attr = r#"#[path="custom.rs"]"#;
        let result = extract_path_from_attr_text(attr);
        assert_eq!(result, Some("custom.rs".to_string()));
    }

    #[test]
    fn test_extract_path_from_attr_text_subdirectory() {
        let attr = r#"#[path = "subdir/module.rs"]"#;
        let result = extract_path_from_attr_text(attr);
        assert_eq!(result, Some("subdir/module.rs".to_string()));
    }

    #[test]
    fn test_extract_path_from_attr_text_no_match() {
        let attr = r"#[derive(Debug)]";
        let result = extract_path_from_attr_text(attr);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_path_from_attr_text_multiline() {
        let attr = r#"
            #[cfg(test)]
            #[path = "test_impl.rs"]
        "#;
        let result = extract_path_from_attr_text(attr);
        assert_eq!(result, Some("test_impl.rs".to_string()));
    }

    // === Rust Analyzer Capability Metadata Tests ===

    #[test]
    fn test_build_context_marks_type_inference_unavailable_without_ra_limitation() {
        use tempfile::tempdir;

        let config = RustGraphConfig::new();
        let temp = tempdir().unwrap();
        let temp_path = temp.path().join("test.rs");
        let ctx = BuildContext::new(&config, &temp_path);
        let metadata = ctx.confidence.to_metadata();

        assert!(
            metadata
                .unavailable_features
                .contains(&"type_inference".to_string()),
            "type_inference should be marked unavailable until production RA inference exists"
        );
        assert!(
            !metadata
                .limitations
                .iter()
                .any(|limitation| limitation.contains("rust-analyzer")),
            "graph builder should not report rust-analyzer probing limitations"
        );
    }

    // === Visibility Extraction Tests ===

    #[test]
    fn test_extract_visibility_pub() {
        let source = "pub fn public_func() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let func = root.child(0).expect("should have function_item child");

        assert_eq!(func.kind(), "function_item");
        let vis = extract_visibility(func, source.as_bytes());
        assert_eq!(
            vis,
            Some("public"),
            "public function should have 'public' visibility"
        );
    }

    #[test]
    fn test_extract_visibility_private() {
        let source = "fn private_func() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let func = root.child(0).expect("should have function_item child");

        assert_eq!(func.kind(), "function_item");
        let vis = extract_visibility(func, source.as_bytes());
        assert_eq!(
            vis,
            Some("private"),
            "private function should have 'private' visibility"
        );
    }

    #[test]
    fn test_extract_visibility_pub_crate() {
        let source = "pub(crate) fn crate_func() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let func = root.child(0).expect("should have function_item child");

        assert_eq!(func.kind(), "function_item");
        let vis = extract_visibility(func, source.as_bytes());
        assert_eq!(
            vis,
            Some("public"),
            "pub(crate) function should have 'public' visibility"
        );
    }

    #[test]
    fn test_extract_visibility_pub_super() {
        let source = "pub(super) fn super_func() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let func = root.child(0).expect("should have function_item child");

        assert_eq!(func.kind(), "function_item");
        let vis = extract_visibility(func, source.as_bytes());
        assert_eq!(
            vis,
            Some("public"),
            "pub(super) function should have 'public' visibility"
        );
    }
}
