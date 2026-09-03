//! Cpp `GraphBuilder` implementation for code graph construction.
//!
//! Extracts Cpp-specific relationships:
//! - Class definitions (regular, template, sealed, objects, companion objects)
//! - Function definitions (regular, virtual, inline, extension functions)
//! - Call expressions (regular calls, method calls, extension calls)
//! - Inheritance (class/struct inheritance via Inherits edges)
//! - Interface implementation (Implements edges for classes implementing pure virtual interfaces)
//! - FFI declarations (extern "C" blocks via `FfiCall` edges)
//!
//! # Multi-Pass Strategy
//!
//! 1. **Pass 1**: Extract class/object definitions → Create Class nodes
//! 2. **Pass 2**: Extract function/property definitions → Create Function nodes
//! 3. **Pass 3**: Extract call expressions → Create Call edges
//! 4. **Pass 4**: Extract FFI declarations → Create FFI function nodes

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{FfiConvention, GraphBuildHelper, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};
use tree_sitter::{Node, Tree};

/// File-level module name for exports.
/// In C++, symbols at file/namespace scope with external linkage are exported.
const FILE_MODULE_NAME: &str = "<file_module>";

/// Type alias for mapping (qualifier, name) tuples to fully-qualified names
/// Used for both field types and type mappings in C++ AST analysis
type QualifiedNameMap = HashMap<(String, String), String>;

/// Registry of FFI declarations discovered during graph building.
///
/// Maps simple function names (e.g., `printf`) to their qualified FFI name
/// (e.g., `extern::C::printf`) and calling convention. This allows call edge
/// construction to detect when a call targets an FFI function and create
/// `FfiCall` edges instead of regular `Call` edges.
type FfiRegistry = HashMap<String, (String, FfiConvention)>;

/// Registry of pure virtual interfaces (abstract classes with only pure virtual methods).
///
/// Maps interface name to their qualified names for Implements edge creation.
type PureVirtualRegistry = HashSet<String>;

const DEFAULT_GRAPH_BUILD_TIMEOUT_MS: u64 = 10_000;
const MIN_GRAPH_BUILD_TIMEOUT_MS: u64 = 1_000;
const MAX_GRAPH_BUILD_TIMEOUT_MS: u64 = 60_000;
const BUDGET_CHECK_INTERVAL: u32 = 1024;

fn cpp_graph_build_timeout() -> Duration {
    let timeout_ms = std::env::var("SQRY_CPP_GRAPH_BUILD_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRAPH_BUILD_TIMEOUT_MS)
        .clamp(MIN_GRAPH_BUILD_TIMEOUT_MS, MAX_GRAPH_BUILD_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

struct BuildBudget {
    file: PathBuf,
    phase_timeout: Duration,
    started_at: Instant,
    checkpoints: u32,
}

impl BuildBudget {
    fn new(file: &Path) -> Self {
        Self {
            file: file.to_path_buf(),
            phase_timeout: cpp_graph_build_timeout(),
            started_at: Instant::now(),
            checkpoints: 0,
        }
    }

    #[cfg(test)]
    fn already_expired(file: &Path) -> Self {
        Self {
            file: file.to_path_buf(),
            phase_timeout: Duration::from_secs(1),
            started_at: Instant::now().checked_sub(Duration::from_secs(60)).unwrap(),
            checkpoints: BUDGET_CHECK_INTERVAL - 1,
        }
    }

    fn checkpoint(&mut self, phase: &'static str) -> GraphResult<()> {
        self.checkpoints = self.checkpoints.wrapping_add(1);
        if self.checkpoints.is_multiple_of(BUDGET_CHECK_INTERVAL)
            && self.started_at.elapsed() > self.phase_timeout
        {
            return Err(GraphBuilderError::BuildTimedOut {
                file: self.file.clone(),
                phase,
                #[allow(clippy::cast_possible_truncation)] // Graph storage: node/edge index counts fit in u32
                timeout_ms: self.phase_timeout.as_millis() as u64,
            });
        }
        Ok(())
    }
}

// ================================
// ASTGraph: In-memory function context index
// ================================

/// In-memory index of C++ function contexts for O(1) lookups during call edge extraction.
///
/// This structure is built in a first pass over the AST and provides:
/// - Fast lookup of the enclosing function for any byte position
/// - Qualified names for all functions/methods
/// - Field type resolution for member variable method calls
/// - Type name resolution via includes and using declarations
#[derive(Debug)]
struct ASTGraph {
    /// All function/method contexts with their qualified names and byte spans
    contexts: Vec<FunctionContext>,
    /// Maps function definition start byte to its context index.
    context_start_index: HashMap<usize, usize>,

    /// Maps (`class_fqn`, `field_name`) to field's FQN type.
    /// Example: ("`demo::Service`", "repo") -> "`demo::Repository`"
    /// This avoids collisions when multiple classes have fields with the same name.
    /// Consumed by `resolve_member_call` to resolve method calls on member
    /// variables (e.g., `repo.save()` inside `demo::Service` -> `demo::Repository::save`).
    field_types: QualifiedNameMap,

    /// Maps (`namespace_context`, `simple_type_name`) to FQN.
    /// Example: ("app", "Widget") -> "`lib::Widget`"
    /// Populated exclusively by using-declaration aliases (`using lib::Widget;`),
    /// so the same simple name resolves differently per using-declaration scope.
    /// Consumed by `resolve_static_call` to resolve `Widget::make()` through a
    /// using-declaration alias to `lib::Widget::make`.
    type_map: QualifiedNameMap,

    /// Maps byte ranges to namespace prefixes (e.g., range -> "`demo::`")
    /// Consumed by `resolve_static_call` (via `find_namespace_for_offset`) to
    /// determine the caller's namespace context when keying `type_map`.
    namespace_map: HashMap<std::ops::Range<usize>, String>,
}

impl ASTGraph {
    /// Build `ASTGraph` from tree-sitter AST
    fn from_tree(root: Node, content: &[u8], budget: &mut BuildBudget) -> GraphResult<Self> {
        // Extract namespace context
        let namespace_map = extract_namespace_map(root, content, budget)?;

        // Extract function contexts
        let mut contexts = extract_cpp_contexts(root, content, &namespace_map, budget)?;
        contexts.sort_by_key(|ctx| ctx.span.0);
        let context_start_index = contexts
            .iter()
            .enumerate()
            .map(|(idx, ctx)| (ctx.span.0, idx))
            .collect();

        // Extract field declarations and type mappings
        let (field_types, type_map) =
            extract_field_and_type_info(root, content, &namespace_map, budget)?;

        Ok(Self {
            contexts,
            context_start_index,
            field_types,
            type_map,
            namespace_map,
        })
    }

    /// Find the enclosing function context for a given byte position.
    ///
    /// C++ has no nested function definitions, so at most one function span can
    /// contain any byte offset. With contexts sorted by start byte we can use a
    /// binary search instead of scanning every function for every call site.
    fn find_enclosing(&self, byte_pos: usize) -> Option<&FunctionContext> {
        let insertion_point = self.contexts.partition_point(|ctx| ctx.span.0 <= byte_pos);
        if insertion_point == 0 {
            return None;
        }

        let candidate = &self.contexts[insertion_point - 1];
        (byte_pos < candidate.span.1).then_some(candidate)
    }

    fn context_for_start(&self, start_byte: usize) -> Option<&FunctionContext> {
        self.context_start_index
            .get(&start_byte)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

/// Represents a C++ function or method with its qualified name and metadata
#[derive(Debug, Clone)]
struct FunctionContext {
    /// Fully qualified name: "`demo::Service::process`" or "`demo::helper`"
    qualified_name: String,
    /// Byte span of the function body
    span: (usize, usize),
    /// Whether this is a static method
    /// Reserved for future method resolution enhancements
    is_static: bool,
    /// Whether this is a virtual method.
    /// Not wired by #466 (virtual-dispatch target-set expansion is out of scope);
    /// asserted by context-extraction tests only, so kept behind `dead_code`.
    #[allow(dead_code)]
    is_virtual: bool,
    /// Whether this is inline.
    /// Not wired by #466; asserted by context-extraction tests only, so kept
    /// behind `dead_code`.
    #[allow(dead_code)]
    is_inline: bool,
    /// Namespace stack for use in call resolution (e.g., [`demo`])
    namespace_stack: Vec<String>,
    /// Class stack for use in call resolution (e.g., [`Service`], or
    /// [`Outer`, `Nested`] for an in-class nested method). Reconstructed into the
    /// enclosing-class FQN by `enclosing_class_fqn` for `field_types` lookups.
    class_stack: Vec<String>,
    /// Return type of the function (e.g., `int`, `std::string`)
    return_type: Option<String>,
}

impl FunctionContext {
    #[allow(dead_code)] // Reserved for future context queries
    fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
}

/// Cpp-specific `GraphBuilder` implementation.
///
/// Performs multi-pass analysis:
/// 1. Extract class and object definitions
/// 2. Extract function and property definitions
/// 3. Extract call expressions
///
/// # Example
///
/// ```no_run
/// use sqry_lang_cpp::relations::CppGraphBuilder;
/// use sqry_core::graph::GraphBuilder;
/// use sqry_core::graph::unified::StagingGraph;
/// use tree_sitter::Parser;
///
/// let mut parser = Parser::new();
/// parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
/// let tree = parser.parse(b"class User { public: std::string getName() { return \"Alice\"; } };", None).unwrap();
/// let mut staging = StagingGraph::new();
/// let builder = CppGraphBuilder::new();
/// builder.build_graph(&tree, b"class User { public: std::string getName() { return \"Alice\"; } };",
///                      std::path::Path::new("test.cpp"), &mut staging).unwrap();
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct CppGraphBuilder;

impl CppGraphBuilder {
    /// Create a new Cpp `GraphBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::unused_self)] // Method uses self for API consistency
    #[allow(clippy::trivially_copy_pass_by_ref)] // Intentional
    fn build_graph_with_budget(
        #[allow(clippy::trivially_copy_pass_by_ref)] // API consistency with other methods
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
        budget: &mut BuildBudget,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Cpp);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree.root_node(), content, budget)?;

        // Track seen includes for deduplication
        let mut seen_includes: HashSet<String> = HashSet::new();

        // Track namespace and class context for qualified naming
        let mut namespace_stack: Vec<String> = Vec::new();
        let mut class_stack: Vec<String> = Vec::new();

        // Two-pass approach for FFI call linking:
        // Pass 1: Collect FFI declarations so calls can be resolved regardless of source order
        let mut ffi_registry = FfiRegistry::new();
        collect_ffi_declarations(tree.root_node(), content, &mut ffi_registry, budget)?;

        // Pass 1b: Collect pure virtual interfaces for Implements edge detection
        let mut pure_virtual_registry = PureVirtualRegistry::new();
        collect_pure_virtual_interfaces(
            tree.root_node(),
            content,
            &mut pure_virtual_registry,
            budget,
        )?;

        // Walk tree to find classes, functions, methods, and calls
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &mut seen_includes,
            &mut namespace_stack,
            &mut class_stack,
            &ffi_registry,
            &pure_virtual_registry,
            budget,
        )?;

        Ok(())
    }

    /// Extract class attributes from modifiers.
    #[allow(dead_code)] // Scaffolding for class attribute analysis
    fn extract_class_attributes(node: &tree_sitter::Node, content: &[u8]) -> Vec<String> {
        let mut attributes = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "modifiers" {
                let mut mod_cursor = child.walk();
                for modifier in child.children(&mut mod_cursor) {
                    if let Ok(mod_text) = modifier.utf8_text(content) {
                        match mod_text {
                            "template" => attributes.push("template".to_string()),
                            "sealed" => attributes.push("sealed".to_string()),
                            "abstract" => attributes.push("abstract".to_string()),
                            "open" => attributes.push("open".to_string()),
                            "final" => attributes.push("final".to_string()),
                            "inner" => attributes.push("inner".to_string()),
                            "value" => attributes.push("value".to_string()),
                            _ => {}
                        }
                    }
                }
            }
        }
        attributes
    }

    /// Check if a function is virtual (async).
    #[allow(dead_code)] // Scaffolding for virtual method detection
    fn extract_is_virtual(node: &tree_sitter::Node, content: &[u8]) -> bool {
        if let Some(spec) = node.child_by_field_name("declaration_specifiers")
            && let Ok(text) = spec.utf8_text(content)
            && text.contains("virtual")
        {
            return true;
        }

        if let Ok(text) = node.utf8_text(content)
            && text.contains("virtual")
        {
            return true;
        }

        if let Some(parent) = node.parent()
            && (parent.kind() == "field_declaration" || parent.kind() == "declaration")
            && let Ok(text) = parent.utf8_text(content)
            && text.contains("virtual")
        {
            return true;
        }

        false
    }

    /// Extract function attributes from modifiers.
    #[allow(dead_code)] // Scaffolding for function attribute analysis
    fn extract_function_attributes(node: &tree_sitter::Node, content: &[u8]) -> Vec<String> {
        let mut attributes = Vec::new();
        for node_ref in [
            node.child_by_field_name("declaration_specifiers"),
            node.parent(),
        ]
        .into_iter()
        .flatten()
        {
            if let Ok(text) = node_ref.utf8_text(content) {
                for keyword in [
                    "virtual",
                    "inline",
                    "constexpr",
                    "operator",
                    "override",
                    "static",
                ] {
                    if text.contains(keyword) && !attributes.contains(&keyword.to_string()) {
                        attributes.push(keyword.to_string());
                    }
                }
            }
        }

        if let Ok(text) = node.utf8_text(content) {
            for keyword in [
                "virtual",
                "inline",
                "constexpr",
                "operator",
                "override",
                "static",
            ] {
                if text.contains(keyword) && !attributes.contains(&keyword.to_string()) {
                    attributes.push(keyword.to_string());
                }
            }
        }

        attributes
    }
}

impl GraphBuilder for CppGraphBuilder {
    fn language(&self) -> Language {
        Language::Cpp
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(cpp_shape_mapping())
    }

    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut budget = BuildBudget::new(file);
        self.build_graph_with_budget(tree, content, file, staging, &mut budget)
    }
}

/// Per-language [`ShapeMapping`] for C++: the reference partner of Python for
/// AC-6 (a structurally equivalent C++ and Python function must produce
/// comparable descriptors under the one bucket schema).
///
/// Holds a precomputed `kind_id -> CfBucket` table built once from the
/// tree-sitter-cpp grammar and shared process-wide via [`cpp_shape_mapping`].
/// Everything except this mapping is the one shared `compute_shape_descriptor`
/// routine.
pub struct CppShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl CppShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-cpp grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let count = lang.node_kind_count();
        let mut cf_by_kind_id = vec![None; count];
        for (id, slot) in cf_by_kind_id.iter_mut().enumerate() {
            let Ok(kind_id) = u16::try_from(id) else {
                break;
            };
            if !lang.node_kind_is_named(kind_id) {
                continue;
            }
            if let Some(name) = lang.node_kind_for_id(kind_id) {
                *slot = cf_bucket_for_cpp_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for CppShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // A C++ function nests its parameter list inside the declarator
        // (`function_definition.declarator -> function_declarator -> parameter_list`),
        // so there is no direct `parameters` field to read.
        if let Some(params) = cpp_parameter_list(fn_node) {
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                match child.kind() {
                    "parameter_declaration" => {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                    }
                    // `int x = 0` default argument.
                    "optional_parameter_declaration" => {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                        shape.has_defaults = true;
                    }
                    // C-style `...` ellipsis or `Args... args` parameter pack.
                    "variadic_parameter_declaration" | "variadic_declarator" => {
                        shape.has_varargs = true;
                    }
                    _ => {}
                }
            }
        }
        // The function declares a return type whenever the node carries a `type`
        // field (`auto`/`int`/...); constructors and destructors do not.
        shape.has_return_annotation = fn_node.child_by_field_name("type").is_some();
        shape
    }
}

/// Descend a C++ function node to its `parameter_list`, threading through the
/// `declarator` field (`function_declarator` for a plain definition, possibly
/// wrapped in pointer/reference declarators).
fn cpp_parameter_list(fn_node: Node) -> Option<Node> {
    let mut declarator = fn_node.child_by_field_name("declarator")?;
    // Unwrap pointer/reference declarator layers until we reach the function
    // declarator that owns the parameter list.
    for _ in 0..8 {
        if declarator.kind() == "function_declarator" {
            return declarator.child_by_field_name("parameters");
        }
        match declarator.child_by_field_name("declarator") {
            Some(inner) => declarator = inner,
            None => break,
        }
    }
    None
}

/// Map one tree-sitter-cpp grammar node-kind name to its canonical control-flow
/// bucket. Additive-only; the bucket set is frozen.
fn cf_bucket_for_cpp_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "conditional_expression" => CfBucket::Branch,
        "for_statement" | "for_range_loop" | "while_statement" | "do_statement" => CfBucket::Loop,
        "switch_statement" | "case_statement" => CfBucket::Match,
        "try_statement" => CfBucket::Try,
        "catch_clause" => CfBucket::Catch,
        "throw_statement" | "throw_expression" => CfBucket::Throw,
        "return_statement" | "co_return_statement" => CfBucket::Return,
        "co_yield_expression" => CfBucket::Yield,
        "co_await_expression" => CfBucket::Await,
        "break_statement" | "continue_statement" | "goto_statement" => CfBucket::BreakContinue,
        "call_expression" => CfBucket::Call,
        "assignment_expression" | "init_declarator" | "declaration" => CfBucket::Assign,
        "lambda_expression" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide C++ shape mapping, built once on first use.
#[must_use]
pub fn cpp_shape_mapping() -> &'static CppShapeMapping {
    static MAPPING: OnceLock<CppShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(CppShapeMapping::build)
}

// ================================
// Context Extraction (Stub Implementations)
// ================================

/// Extract namespace declarations and build a map from byte ranges to namespace names.
///
/// This function recursively traverses the AST and builds a map from byte ranges to namespace
/// prefixes. For example, if a node is inside `namespace demo { ... }`, its byte range will
/// map to "`demo::`".
///
/// Returns: `HashMap`<Range<usize>, String> mapping byte ranges to namespace prefixes
fn extract_namespace_map(
    node: Node,
    content: &[u8],
    budget: &mut BuildBudget,
) -> GraphResult<HashMap<std::ops::Range<usize>, String>> {
    let mut map = HashMap::new();

    // Create recursion guard with configured limit
    let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
        .expect("Failed to load recursion limits");
    let file_ops_depth = recursion_limits
        .effective_file_ops_depth()
        .expect("Invalid file_ops_depth configuration");
    let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
        .expect("Failed to create recursion guard");

    extract_namespaces_recursive(node, content, "", &mut map, &mut guard, budget).map_err(|e| {
        match e {
            timeout @ GraphBuilderError::BuildTimedOut { .. } => timeout,
            other => GraphBuilderError::ParseError {
                span: span_from_node(node),
                reason: format!("C++ namespace extraction failed: {other}"),
            },
        }
    })?;

    Ok(map)
}

/// Recursive helper for namespace extraction
///
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_namespaces_recursive(
    node: Node,
    content: &[u8],
    current_ns: &str,
    map: &mut HashMap<std::ops::Range<usize>, String>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:extract_namespace_map")?;
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: span_from_node(node),
        reason: format!("C++ namespace extraction hit recursion limit: {e}"),
    })?;

    if node.kind() == "namespace_definition" {
        // Extract namespace name from the namespace_identifier or identifier child
        let ns_name = if let Some(name_node) = node.child_by_field_name("name") {
            extract_identifier(name_node, content)
        } else {
            // Anonymous namespace
            String::from("anonymous")
        };

        // Build new namespace prefix
        let new_ns = if current_ns.is_empty() {
            format!("{ns_name}::")
        } else {
            format!("{current_ns}{ns_name}::")
        };

        // Map the body's byte range to this namespace
        if let Some(body) = node.child_by_field_name("body") {
            let range = body.start_byte()..body.end_byte();
            map.insert(range, new_ns.clone());

            // Recurse into nested namespaces within the body
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                extract_namespaces_recursive(child, content, &new_ns, map, guard, budget)?;
            }
        }
    } else {
        // Recurse with current namespace
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            extract_namespaces_recursive(child, content, current_ns, map, guard, budget)?;
        }
    }

    guard.exit();
    Ok(())
}

/// Extract identifier from a node (handles simple identifiers and qualified names)
fn extract_identifier(node: Node, content: &[u8]) -> String {
    node.utf8_text(content).unwrap_or("").to_string()
}

/// Find the namespace prefix for a given byte offset
fn find_namespace_for_offset(
    byte_offset: usize,
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
) -> String {
    // Find all ranges that contain this offset
    let mut matching_ranges: Vec<_> = namespace_map
        .iter()
        .filter(|(range, _)| range.contains(&byte_offset))
        .collect();

    // Sort by range size (smaller ranges are more specific/nested)
    matching_ranges.sort_by_key(|(range, _)| range.end - range.start);

    // Return the most specific (smallest) range's namespace
    matching_ranges
        .first()
        .map_or("", |(_, ns)| ns.as_str())
        .to_string()
}

/// Extract all function/method contexts with their qualified names.
///
/// This function traverses the AST and builds a complete list of all functions/methods
/// with their fully qualified names (including namespace and class context).
///
/// Returns: Vec<FunctionContext> with all function/method contexts
fn extract_cpp_contexts(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    budget: &mut BuildBudget,
) -> GraphResult<Vec<FunctionContext>> {
    let mut contexts = Vec::new();
    let mut class_stack = Vec::new();

    // Create recursion guard with configured limit
    let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
        .expect("Failed to load recursion limits");
    let file_ops_depth = recursion_limits
        .effective_file_ops_depth()
        .expect("Invalid file_ops_depth configuration");
    let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
        .expect("Failed to create recursion guard");

    extract_contexts_recursive(
        node,
        content,
        namespace_map,
        &mut contexts,
        &mut class_stack,
        &mut guard,
        budget,
    )
    .map_err(|e| match e {
        timeout @ GraphBuilderError::BuildTimedOut { .. } => timeout,
        other => GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: format!("C++ context extraction failed: {other}"),
        },
    })?;

    Ok(contexts)
}

/// Recursive helper for function context extraction
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_contexts_recursive(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    contexts: &mut Vec<FunctionContext>,
    class_stack: &mut Vec<String>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:extract_contexts")?;
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: span_from_node(node),
        reason: format!("C++ context extraction hit recursion limit: {e}"),
    })?;

    match node.kind() {
        "class_specifier" | "struct_specifier" => {
            // Extract class/struct name
            if let Some(name_node) = node.child_by_field_name("name") {
                let class_name = extract_identifier(name_node, content);
                class_stack.push(class_name);

                // Recurse into class body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_contexts_recursive(
                            child,
                            content,
                            namespace_map,
                            contexts,
                            class_stack,
                            guard,
                            budget,
                        )?;
                    }
                }

                class_stack.pop();
            }
        }

        "function_definition" => {
            // Extract function name and build qualified name
            if let Some(declarator) = node.child_by_field_name("declarator") {
                let (func_name, class_prefix) =
                    extract_function_name_with_class(declarator, content);

                // Find enclosing namespace and convert to stack
                let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);
                let namespace_stack: Vec<String> = if namespace.is_empty() {
                    Vec::new()
                } else {
                    namespace
                        .trim_end_matches("::")
                        .split("::")
                        .map(String::from)
                        .collect()
                };

                // Build the effective class stack:
                // - If we're inside a class body, use that class stack
                // - If this is an out-of-class method (e.g., Service::process), use the class prefix
                let effective_class_stack: Vec<String> = if !class_stack.is_empty() {
                    class_stack.clone()
                } else if let Some(ref prefix) = class_prefix {
                    vec![prefix.clone()]
                } else {
                    Vec::new()
                };

                // Build qualified name
                let qualified_name =
                    build_qualified_name(&namespace_stack, &effective_class_stack, &func_name);

                // Extract metadata
                let is_static = is_static_function(node, content);
                let is_virtual = is_virtual_function(node, content);
                let is_inline = is_inline_function(node, content);

                // Extract return type from function definition
                let return_type = node
                    .child_by_field_name("type")
                    .and_then(|type_node| type_node.utf8_text(content).ok())
                    .map(std::string::ToString::to_string);

                // Get function definition's full span for matching during graph building
                let span = (node.start_byte(), node.end_byte());

                contexts.push(FunctionContext {
                    qualified_name,
                    span,
                    is_static,
                    is_virtual,
                    is_inline,
                    namespace_stack,
                    class_stack: effective_class_stack,
                    return_type,
                });
            }

            // Don't recurse into function body - C++ doesn't have nested functions
        }

        _ => {
            // Recurse into children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_contexts_recursive(
                    child,
                    content,
                    namespace_map,
                    contexts,
                    class_stack,
                    guard,
                    budget,
                )?;
            }
        }
    }

    guard.exit();
    Ok(())
}

/// Build a fully qualified name from namespace stack, class stack, and name.
///
/// This function combines namespace context, class hierarchy, and the final name
/// into a C++-style qualified name (e.g., `namespace::ClassName::methodName`).
/// What a tagged type specifier (`class` / `struct` / `union` / `enum`) at
/// this site actually is (issue #748).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaggedSpecifierRole {
    /// `struct Payload { int a; };`, `class Holder { ... };`. A declaration
    /// with a body: the extent is the type's own.
    Definition,
    /// `class Widget;`, `enum State : int;`, `template <typename T> class T;`.
    /// A declaration of identity with no body. Still a definition for
    /// `find_unused`, the items filter and centrality, but nothing to
    /// fingerprint.
    ForwardDeclaration,
    /// `struct Payload *slot;` as a member, `void render(enum State s)` as a
    /// parameter, `sizeof(struct Payload)`. A REFERENCE to a type declared
    /// elsewhere: the extent in hand belongs to the field or the parameter
    /// list, not to the type.
    Reference,
}

/// Classify a tagged type specifier.
///
/// Identical rule to the C plugin's, and probed the same way against
/// tree-sitter-cpp 0.23. A `body` field settles `Definition`. The other two
/// share a bodyless shape, so they are told apart by the PARENT: no
/// `declarator` on it AND a parent that is a place where declarations are
/// written.
///
/// | source | parent | `declarator` | role |
/// |---|---|---|---|
/// | `class Widget;` | `translation_unit` | no | forward |
/// | `namespace n { class W; }` | `declaration_list` | no | forward |
/// | `class InnerFwd;` in a class | `field_declaration` | no | forward |
/// | `template <class T> class W;` | `template_declaration` | no | forward |
/// | `struct P *slot;` as a member | `field_declaration` | yes | reference |
/// | `void f(enum S s)` | `parameter_declaration` | yes | reference |
/// | `void f(struct P)` | `parameter_declaration` | no | reference |
/// | `sizeof(struct P)` | `type_descriptor` | no | reference |
///
/// The parent list is an ALLOW list: an unlisted parent falls through to
/// `Reference`, which keeps the node out of the body plane. Mistaking a
/// reference for a declaration is the direction that fabricates definitions,
/// so that is the direction to fail away from.///
/// # Which arms are reachable
///
/// Each arm was deleted in turn from both plugins and the per-language gate
/// re-run. Eleven of the thirteen are killed by a fixture: `translation_unit`,
/// `declaration` (an attribute-prefixed forward), `declaration_list` (a
/// namespace body and an `extern "C"` block), `compound_statement`,
/// `field_declaration`, `template_declaration`, and all five preprocessor
/// conditionals.
///
/// `declaration` only became reachable once a MISSING declarator stopped
/// counting as a real one; before that, every specifier under it took the
/// declarator branch first and the arm was dead.
///
/// Two survive, because nothing reaches them:
///
/// - `field_declaration_list` never appears as a specifier's DIRECT parent; a
///   member declaration is wrapped in a `field_declaration`.
/// - `linkage_specification` likewise: an `extern "C"` block's body parses as
///   a `declaration_list`.
///
/// They are kept anyway. They can only ever move a shape from `Reference` to
/// `ForwardDeclaration`, which is the direction that preserves a real
/// declaration's `is_definition`, and a grammar update could make either
/// reachable. Unkillable-by-design, and recorded as such rather than left as
/// two unexplained surviving mutants.
fn classify_tagged_specifier(node: Node) -> TaggedSpecifierRole {
    if node.child_by_field_name("body").is_some() {
        return TaggedSpecifierRole::Definition;
    }
    let Some(parent) = node.parent() else {
        return TaggedSpecifierRole::Reference;
    };
    // A MISSING declarator is not a declarator. tree-sitter inserts one
    // wherever a `declaration` is expected to name something and does not,
    // which is exactly the shape an attribute-prefixed forward declaration
    // takes: `__attribute__((unused)) struct Config;` and
    // `[[maybe_unused]] class Widget;` both parse as a `declaration` whose
    // declarator is `(MISSING identifier)`. Treating that as a real declarator
    // sent every prefixed forward to the reference sink (issue #748).
    if let Some(declarator) = parent.child_by_field_name("declarator")
        && !declarator.is_missing()
    {
        return TaggedSpecifierRole::Reference;
    }
    if matches!(
        parent.kind(),
        "translation_unit"
            | "declaration"
            | "declaration_list"
            | "compound_statement"
            | "field_declaration"
            | "field_declaration_list"
            | "template_declaration"
            | "linkage_specification"
            // Preprocessor conditionals hold declarations directly, and an
            // include guard wraps essentially every real header, so leaving
            // these out made the whole forward-declaration case unreachable in
            // practice.
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
    ) {
        TaggedSpecifierRole::ForwardDeclaration
    } else {
        TaggedSpecifierRole::Reference
    }
}

fn build_qualified_name(namespace_stack: &[String], class_stack: &[String], name: &str) -> String {
    let mut parts = Vec::new();

    // Add namespace stack
    parts.extend(namespace_stack.iter().cloned());

    // Add class stack
    for class_name in class_stack {
        parts.push(class_name.clone());
    }

    // Add name
    parts.push(name.to_string());

    parts.join("::")
}

/// Extract function name and optional class prefix from a function declarator node.
/// Returns (`function_name`, `optional_class_prefix`).
/// For `Service::process`, returns ("process", Some("Service")).
/// For `process`, returns ("process", None).
fn extract_function_name_with_class(declarator: Node, content: &[u8]) -> (String, Option<String>) {
    // The declarator can be:
    // - function_declarator (simple function)
    // - qualified_identifier (Class::method)
    // - field_identifier (method)
    // - destructor_name (~Class)
    // - operator_name (operator+)

    match declarator.kind() {
        "function_declarator" => {
            // Recurse to find the actual name
            if let Some(declarator_inner) = declarator.child_by_field_name("declarator") {
                extract_function_name_with_class(declarator_inner, content)
            } else {
                (extract_identifier(declarator, content), None)
            }
        }
        "qualified_identifier" => {
            // For qualified names like Service::process, extract both parts
            let name = if let Some(name_node) = declarator.child_by_field_name("name") {
                extract_identifier(name_node, content)
            } else {
                extract_identifier(declarator, content)
            };

            // Extract the scope (class/namespace prefix)
            let class_prefix = declarator
                .child_by_field_name("scope")
                .map(|scope_node| extract_identifier(scope_node, content));

            (name, class_prefix)
        }
        "field_identifier" | "identifier" | "destructor_name" | "operator_name" => {
            (extract_identifier(declarator, content), None)
        }
        _ => {
            // For other cases, try to extract text directly
            (extract_identifier(declarator, content), None)
        }
    }
}

/// Extract function name from a function declarator node (convenience wrapper)
#[allow(dead_code)]
fn extract_function_name(declarator: Node, content: &[u8]) -> String {
    extract_function_name_with_class(declarator, content).0
}

/// Check if a function is static
fn is_static_function(node: Node, content: &[u8]) -> bool {
    has_specifier(node, "static", content)
}

/// Check if a function is virtual
fn is_virtual_function(node: Node, content: &[u8]) -> bool {
    has_specifier(node, "virtual", content)
}

/// Check if a function is inline
fn is_inline_function(node: Node, content: &[u8]) -> bool {
    has_specifier(node, "inline", content)
}

/// Check if a function has a specific specifier (static, virtual, inline, etc.)
fn has_specifier(node: Node, specifier: &str, content: &[u8]) -> bool {
    // Check declaration specifiers
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if (child.kind() == "storage_class_specifier"
            || child.kind() == "type_qualifier"
            || child.kind() == "virtual"
            || child.kind() == "inline")
            && let Ok(text) = child.utf8_text(content)
            && text == specifier
        {
            return true;
        }
    }
    false
}

/// Extract field declarations and type mappings.
///
/// This function traverses the AST and extracts:
/// 1. Field types: Maps (`class_fqn`, `field_name`) to field's FQN type
/// 2. Type map: Maps (`namespace_context`, `simple_type_name`) to FQN from using directives
///
/// Returns:
/// - `field_types`: Maps (`class_fqn`, `field_name`) to field's FQN type
/// - `type_map`: Maps (`namespace_context`, `simple_type_name`) to FQN
fn extract_field_and_type_info(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    budget: &mut BuildBudget,
) -> GraphResult<(QualifiedNameMap, QualifiedNameMap)> {
    let mut field_types = HashMap::new();
    let mut type_map = HashMap::new();
    let mut class_stack = Vec::new();

    // First pass: collect the FQN of EVERY class/struct declared in this
    // translation unit at the class-visit site, so the store-site scope walk in
    // `extract_field_declaration` can qualify a bare field type against declared
    // classes. This must be collected here, not from `field_types.keys()`: a
    // method-only class (no field declarations) is never a `field_types` key, so
    // sourcing the set from `field_types` would omit it, keep a field of that
    // type bare, and fail Phase 4c-prime unification (02_DESIGN Section 3.2.2).
    let mut declared_classes: HashSet<String> = HashSet::new();
    let mut collect_stack: Vec<String> = Vec::new();
    collect_declared_class_fqns(
        node,
        content,
        namespace_map,
        &mut declared_classes,
        &mut collect_stack,
        budget,
    )?;

    extract_fields_recursive(
        node,
        content,
        namespace_map,
        &declared_classes,
        &mut field_types,
        &mut type_map,
        &mut class_stack,
        budget,
    )?;

    Ok((field_types, type_map))
}

/// Compute a class/struct's FQN from its simple name, the enclosing namespace,
/// and the enclosing class stack. Shared by `collect_declared_class_fqns` and
/// `extract_fields_recursive` so both passes key classes identically:
/// - nested class: `parent_fqn::class_name` (the parent FQN is `class_stack.last()`);
/// - top-level class in a namespace: `namespace::class_name`;
/// - top-level class in the global namespace: `class_name`.
fn build_class_fqn(class_name: &str, namespace: &str, class_stack: &[String]) -> String {
    if let Some(parent_fqn) = class_stack.last() {
        format!("{parent_fqn}::{class_name}")
    } else if namespace.is_empty() {
        class_name.to_string()
    } else {
        format!("{}::{}", namespace.trim_end_matches("::"), class_name)
    }
}

/// First-pass collection of every declared class/struct FQN in the translation
/// unit, keyed identically to `field_types` (via `build_class_fqn`). Includes
/// method-only classes, which never appear as a `field_types` key.
fn collect_declared_class_fqns(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    declared: &mut HashSet<String>,
    class_stack: &mut Vec<String>,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:collect_declared_classes")?;
    match node.kind() {
        "class_specifier" | "struct_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let class_name = extract_identifier(name_node, content);
                let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);
                let class_fqn = build_class_fqn(&class_name, &namespace, class_stack);

                declared.insert(class_fqn.clone());
                class_stack.push(class_fqn);

                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    collect_declared_class_fqns(
                        child,
                        content,
                        namespace_map,
                        declared,
                        class_stack,
                        budget,
                    )?;
                }

                class_stack.pop();
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_declared_class_fqns(
                    child,
                    content,
                    namespace_map,
                    declared,
                    class_stack,
                    budget,
                )?;
            }
        }
    }
    Ok(())
}

/// C++ built-in / primitive type names. A field of a primitive type never
/// receives a member call, so `qualify_field_type` keeps it bare rather than
/// walking enclosing scopes.
fn is_cpp_primitive(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "void"
            | "bool"
            | "char"
            | "double"
            | "float"
            | "long"
            | "short"
            | "unsigned"
            | "signed"
            | "wchar_t"
            | "auto"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "intptr_t"
            | "uintptr_t"
            | "size_t"
            | "ssize_t"
            | "ptrdiff_t"
    )
}

/// Namespace-qualify a stored field-type value by a C++ unqualified-name-lookup
/// scope walk (innermost enclosing scope first), so a member call through the
/// field emits a qualified target that Phase 4c-prime can unify.
///
/// Keeps `::`-qualified values (using-declaration hits or explicit qualification)
/// and primitives bare. Otherwise walks enclosing scopes from innermost to
/// outermost and takes the FIRST that names a declared class: the enclosing
/// class scope (`{class_fqn}::{type}`), then each strict prefix of `class_fqn`,
/// then the namespace (`{namespace}::{type}`). If no enclosing scope names a
/// declared class, the value is kept BARE (an unresolvable stub), never a
/// namespace guess that could collide with a different real type. Class scope
/// winning over namespace scope is what keeps the nested-class case sound (a
/// field `Inner inner` inside `demo::Outer` binds to `demo::Outer::Inner`, not a
/// distinct top-level `demo::Inner`).
fn qualify_field_type(
    resolved_type: &str,
    class_fqn: &str,
    namespace: &str,
    declared_classes: &HashSet<String>,
) -> String {
    // Already qualified (using-decl hit or explicit ::), or a primitive: keep.
    if resolved_type.contains("::") || is_cpp_primitive(resolved_type) {
        return resolved_type.to_string();
    }

    // 1. Enclosing class scope.
    let candidate = format!("{class_fqn}::{resolved_type}");
    if declared_classes.contains(&candidate) {
        return candidate;
    }

    // 2. Each strict prefix of the enclosing class FQN, innermost first.
    let mut prefix = class_fqn;
    while let Some(idx) = prefix.rfind("::") {
        prefix = &prefix[..idx];
        let candidate = format!("{prefix}::{resolved_type}");
        if declared_classes.contains(&candidate) {
            return candidate;
        }
    }

    // 3. The enclosing namespace.
    let namespace_key = namespace.trim_end_matches("::");
    if !namespace_key.is_empty() {
        let candidate = format!("{namespace_key}::{resolved_type}");
        if declared_classes.contains(&candidate) {
            return candidate;
        }
    }

    // No enclosing scope names a declared class: keep the value bare. This is
    // either a declared global-scope class (bare FQN is correct) or an
    // unresolvable stub; never a false edge to a different real node.
    resolved_type.to_string()
}

/// Recursive helper for field and type extraction
fn extract_fields_recursive(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    declared_classes: &HashSet<String>,
    field_types: &mut HashMap<(String, String), String>,
    type_map: &mut HashMap<(String, String), String>,
    class_stack: &mut Vec<String>,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:extract_fields")?;
    match node.kind() {
        "class_specifier" | "struct_specifier" => {
            // Extract class name and build FQN
            if let Some(name_node) = node.child_by_field_name("name") {
                let class_name = extract_identifier(name_node, content);
                let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);

                // Build FQN including parent classes from class_stack
                let class_fqn = build_class_fqn(&class_name, &namespace, class_stack);

                class_stack.push(class_fqn.clone());

                // Process all children to find field_declaration_list or direct field_declaration
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_fields_recursive(
                        child,
                        content,
                        namespace_map,
                        declared_classes,
                        field_types,
                        type_map,
                        class_stack,
                        budget,
                    )?;
                }

                class_stack.pop();
            }
        }

        "field_declaration" => {
            // Extract field declaration if we're inside a class
            if let Some(class_fqn) = class_stack.last() {
                extract_field_declaration(
                    node,
                    content,
                    class_fqn,
                    namespace_map,
                    declared_classes,
                    field_types,
                    type_map,
                );
            }

            // A nested class/struct declaration appears inside a
            // `field_declaration` in tree-sitter-cpp. Recurse so fields on that
            // nested class are added to `field_types` and can resolve calls from
            // inline nested methods (issue #466 T7).
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_fields_recursive(
                    child,
                    content,
                    namespace_map,
                    declared_classes,
                    field_types,
                    type_map,
                    class_stack,
                    budget,
                )?;
            }
        }

        "using_directive" => {
            // Extract using directive: using namespace std;
            extract_using_directive(node, content, namespace_map, type_map);
        }

        "using_declaration" => {
            // Extract using declaration: using std::vector;
            extract_using_declaration(node, content, namespace_map, type_map);
        }

        _ => {
            // Recurse into children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_fields_recursive(
                    child,
                    content,
                    namespace_map,
                    declared_classes,
                    field_types,
                    type_map,
                    class_stack,
                    budget,
                )?;
            }
        }
    }

    Ok(())
}

/// Extract a field declaration and store its type
fn extract_field_declaration(
    node: Node,
    content: &[u8],
    class_fqn: &str,
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    declared_classes: &HashSet<String>,
    field_types: &mut HashMap<(String, String), String>,
    type_map: &HashMap<(String, String), String>,
) {
    // In tree-sitter-cpp, field_declaration children are:
    // type_identifier, field_identifier, ;
    // OR for multiple declarators: type_identifier, declarator1, ',', declarator2, ;

    let mut field_type = None;
    let mut field_names = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "primitive_type" | "qualified_identifier" | "template_type" => {
                field_type = Some(extract_type_name(child, content));
            }
            "field_identifier" => {
                // Direct field identifier (simple case: Type name;)
                field_names.push(extract_identifier(child, content));
            }
            "field_declarator"
            | "init_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator" => {
                // Declarator (with modifiers: Type* name; or Type name = init;)
                if let Some(name) = extract_field_name(child, content) {
                    field_names.push(name);
                }
            }
            _ => {}
        }
    }

    // Resolve field type to FQN using namespace/type_map, then scope-qualify a
    // still-bare class type through C++ unqualified-name lookup so member calls
    // through the field emit a unifiable target (02_DESIGN Section 3.2.2).
    if let Some(ftype) = field_type {
        let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);
        let resolved = resolve_type_to_fqn(&ftype, &namespace, type_map);
        let field_type_fqn = qualify_field_type(&resolved, class_fqn, &namespace, declared_classes);

        // Store each field name with the same type
        for fname in field_names {
            field_types.insert((class_fqn.to_string(), fname), field_type_fqn.clone());
        }
    }
}

/// Extract type name from a type node
fn extract_type_name(type_node: Node, content: &[u8]) -> String {
    match type_node.kind() {
        "type_identifier" | "primitive_type" => extract_identifier(type_node, content),
        "qualified_identifier" => {
            // For qualified types like std::vector, we want the full name
            extract_identifier(type_node, content)
        }
        "template_type" => {
            // For template types like vector<int>, extract the base type
            if let Some(name) = type_node.child_by_field_name("name") {
                extract_identifier(name, content)
            } else {
                extract_identifier(type_node, content)
            }
        }
        _ => {
            // For other cases, try to extract text directly
            extract_identifier(type_node, content)
        }
    }
}

/// Extract field name from a declarator
fn extract_field_name(declarator: Node, content: &[u8]) -> Option<String> {
    match declarator.kind() {
        "field_declarator" => {
            // Recurse to find the actual name
            if let Some(declarator_inner) = declarator.child_by_field_name("declarator") {
                extract_field_name(declarator_inner, content)
            } else {
                Some(extract_identifier(declarator, content))
            }
        }
        "field_identifier" | "identifier" => Some(extract_identifier(declarator, content)),
        "pointer_declarator" | "reference_declarator" | "array_declarator" => {
            // For pointer/reference/array types, recurse to find the name
            if let Some(declarator_inner) = declarator.child_by_field_name("declarator") {
                extract_field_name(declarator_inner, content)
            } else {
                None
            }
        }
        "init_declarator" => {
            // For initialized fields, extract the declarator
            if let Some(declarator_inner) = declarator.child_by_field_name("declarator") {
                extract_field_name(declarator_inner, content)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve a simple type name to its FQN using namespace context and `type_map`
fn resolve_type_to_fqn(
    type_name: &str,
    namespace: &str,
    type_map: &HashMap<(String, String), String>,
) -> String {
    // If already qualified (contains ::), return as-is
    if type_name.contains("::") {
        return type_name.to_string();
    }

    // Try to resolve using type_map with current namespace
    let namespace_key = namespace.trim_end_matches("::").to_string();
    if let Some(fqn) = type_map.get(&(namespace_key.clone(), type_name.to_string())) {
        return fqn.clone();
    }

    // Try global namespace
    if let Some(fqn) = type_map.get(&(String::new(), type_name.to_string())) {
        return fqn.clone();
    }

    // If no mapping found, return as-is
    type_name.to_string()
}

/// Extract using directive (using namespace X;)
fn extract_using_directive(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    _type_map: &mut HashMap<(String, String), String>,
) {
    // For now, we don't store using directives in type_map
    // because they affect all types in a namespace, not just specific ones
    // This is a simplification - full implementation would track these
    let _namespace = find_namespace_for_offset(node.start_byte(), namespace_map);

    // Extract the namespace being used
    if let Some(name_node) = node.child_by_field_name("name") {
        let _using_ns = extract_identifier(name_node, content);
        // Using directives (`using namespace std;`) import all names from a namespace,
        // requiring scoped directive tracking to resolve unqualified types. Using
        // declarations (`using std::vector;`) are handled by extract_using_declaration().
    }
}

/// Extract using declaration (using `X::Y`;)
///
/// Maps simple names to their fully qualified names for type resolution.
/// Example: `using std::vector;` stores `("", "vector") -> "std::vector"`.
fn extract_using_declaration(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
    type_map: &mut HashMap<(String, String), String>,
) {
    let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);
    let namespace_key = namespace.trim_end_matches("::").to_string();

    // Find the qualified_identifier child (tree-sitter-cpp doesn't expose a "name" field)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "qualified_identifier" || child.kind() == "identifier" {
            let fqn = extract_identifier(child, content);

            // Extract the simple name (last part after ::)
            if let Some(simple_name) = fqn.split("::").last() {
                // Store: (namespace_context, simple_name) -> fqn
                type_map.insert((namespace_key, simple_name.to_string()), fqn);
            }
            break;
        }
    }
}

// ================================
// Call Resolution
// ================================

/// Resolve a callee name to its fully qualified name using `ASTGraph` context.
///
/// Branches on the callee expression's AST node kind (authoritative), not on
/// text parsing:
/// - `field_expression` (`repo.save()` / `p->frobnicate()`): resolve the
///   receiver's static type through `field_types` and emit `Type::method`
///   (`resolve_member_call`).
/// - qualified name (`Widget::make()`): resolve the qualifier through a
///   using-declaration alias in `type_map` (`resolve_static_call`).
/// - simple names and every non-hit path: fall back to the exact
///   namespace-prefix string the pre-#466 resolver produced
///   (`resolve_callee_name_namespace_prefixed`), so no existing edge and no
///   `is_unqualified`/FFI-routing bit regresses (02_DESIGN Section 3.4).
///
/// The fallback is byte-identical to the old behavior on every miss, ambiguity,
/// missing class context, or unhandled node kind, so a map that cannot type a
/// receiver never emits a wrong target.
fn resolve_callee_name(
    function_node: Node<'_>,
    callee_name: &str,
    caller_ctx: &FunctionContext,
    ast_graph: &ASTGraph,
    content: &[u8],
) -> String {
    // Member call through a field of the enclosing class.
    if function_node.kind() == "field_expression" {
        if let Some(fqn) = resolve_member_call(function_node, caller_ctx, ast_graph, content) {
            return fqn;
        }
        return resolve_callee_name_namespace_prefixed(callee_name, caller_ctx);
    }

    // Qualified call (`Scope::method`): try a using-declaration alias, else fall
    // back to the namespace-prefix behavior (the specified path for
    // same-namespace static calls, 02_DESIGN Section 3.3).
    if !callee_name.starts_with("::")
        && callee_name.contains("::")
        && let Some(fqn) = resolve_static_call(function_node, callee_name, ast_graph)
    {
        return fqn;
    }

    resolve_callee_name_namespace_prefixed(callee_name, caller_ctx)
}

/// The pre-#466 namespace-prefix resolution, extracted verbatim as a pure text
/// helper (no `ASTGraph` dependency). Every non-hit path in `resolve_callee_name`
/// returns exactly this string, guaranteeing no regression of existing edges or
/// of the `is_unqualified`/FFI-routing bit read at the call-emission site.
fn resolve_callee_name_namespace_prefixed(
    callee_name: &str,
    caller_ctx: &FunctionContext,
) -> String {
    // If already fully qualified (starts with ::), return as-is
    if callee_name.starts_with("::") {
        return callee_name.trim_start_matches("::").to_string();
    }

    // If contains ::, it might be partially qualified (e.g., "Service::process")
    if callee_name.contains("::") {
        // Add namespace prefix if not already qualified
        if !caller_ctx.namespace_stack.is_empty() {
            let namespace_prefix = caller_ctx.namespace_stack.join("::");
            return format!("{namespace_prefix}::{callee_name}");
        }
        return callee_name.to_string();
    }

    // Simple name: build FQN from caller's namespace and class context
    let mut parts = Vec::new();

    // Add namespace
    if !caller_ctx.namespace_stack.is_empty() {
        parts.extend(caller_ctx.namespace_stack.iter().cloned());
    }

    // For simple names within a class, don't add class context automatically
    // (the call might be to a free function or static method from another class)

    // Add function name
    parts.push(callee_name.to_string());

    parts.join("::")
}

/// Reconstruct the enclosing class's fully namespace-qualified, fully nested FQN
/// from a `FunctionContext`, matching the `field_types` key format built at
/// `extract_fields_recursive`. `class_stack` holds bare simple class names for
/// in-class methods (or a single combined prefix for out-of-class definitions),
/// so the key is rebuilt by `::`-joining `namespace_stack ++ class_stack` rather
/// than reading `class_stack.last()` alone (which is a bare name and misses the
/// namespace-qualified key). Returns `None` for a free function (no enclosing
/// class), where member lookup is skipped.
fn enclosing_class_fqn(caller_ctx: &FunctionContext) -> Option<String> {
    if caller_ctx.class_stack.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    parts.extend(caller_ctx.namespace_stack.iter().map(String::as_str));
    parts.extend(caller_ctx.class_stack.iter().map(String::as_str));
    Some(parts.join("::"))
}

/// Resolve a member call (`receiver.method()` / `receiver->method()`) through the
/// enclosing class's fields. Conservative: only a bare-identifier receiver that
/// names a field of the enclosing class is typed; `this`, chained expressions,
/// locals, parameters, and smart pointers miss `field_types` and force the
/// caller to fall back (no wrong edge). Returns `Some("Type::method")` on a
/// single unambiguous hit, where `Type` is already a fully namespace-qualified
/// FQN because the stored field-type value is scope-qualified at the store site
/// (`qualify_field_type`).
fn resolve_member_call(
    function_node: Node<'_>,
    caller_ctx: &FunctionContext,
    ast_graph: &ASTGraph,
    content: &[u8],
) -> Option<String> {
    let receiver_node = function_node.child_by_field_name("argument")?;
    let method_node = function_node.child_by_field_name("field")?;

    // Only trust a single-identifier receiver: anything else (this->, nested
    // field/call expressions) is not a plain field access we can type.
    if !matches!(receiver_node.kind(), "identifier" | "field_identifier") {
        return None;
    }

    let receiver_text = receiver_node.utf8_text(content).ok()?.trim();
    let method_text = method_node.utf8_text(content).ok()?.trim();
    if receiver_text.is_empty() || method_text.is_empty() {
        return None;
    }

    let class_fqn = enclosing_class_fqn(caller_ctx)?;
    let field_type = ast_graph
        .field_types
        .get(&(class_fqn, receiver_text.to_string()))?;

    Some(format!("{field_type}::{method_text}"))
}

/// Resolve a qualified static call (`Qualifier::method()`) through a
/// using-declaration alias in `type_map`, keyed by the caller's namespace
/// context (looked up from the stored `namespace_map`). Only a single-segment
/// qualifier is resolved (multi-segment qualifiers fall back); a miss returns
/// `None` so the caller keeps the specified namespace-prefix behavior.
fn resolve_static_call(
    function_node: Node<'_>,
    callee_name: &str,
    ast_graph: &ASTGraph,
) -> Option<String> {
    let (qualifier, method) = callee_name.rsplit_once("::")?;
    // Only a bare, single-segment qualifier is a using-declaration alias key.
    if qualifier.is_empty() || qualifier.contains("::") || method.is_empty() {
        return None;
    }

    let namespace = find_namespace_for_offset(function_node.start_byte(), &ast_graph.namespace_map);
    let namespace_key = namespace.trim_end_matches("::").to_string();

    let resolved_qualifier = ast_graph
        .type_map
        .get(&(namespace_key, qualifier.to_string()))
        .or_else(|| {
            ast_graph
                .type_map
                .get(&(String::new(), qualifier.to_string()))
        })?;

    Some(format!("{resolved_qualifier}::{method}"))
}

/// Strip type qualifiers (const, volatile, *, &) to extract the base type name.
/// Examples:
/// - "const int*" -> "int"
/// - "int const*" -> "int"  (postfix const)
/// - "`std::string`&" -> "string"
/// - "vector<int>" -> "vector"
fn strip_type_qualifiers(type_text: &str) -> String {
    let mut result = type_text.trim().to_string();

    // Remove prefix qualifiers (with trailing space)
    result = result.replace("const ", "");
    result = result.replace("volatile ", "");
    result = result.replace("mutable ", "");
    result = result.replace("constexpr ", "");

    // Remove postfix qualifiers (with leading space)
    result = result.replace(" const", "");
    result = result.replace(" volatile", "");
    result = result.replace(" mutable", "");
    result = result.replace(" constexpr", "");

    // Remove pointer and reference markers
    result = result.replace(['*', '&'], "");

    // Trim any extra whitespace
    result = result.trim().to_string();

    // Extract the simple name from qualified names (std::string -> string)
    if let Some(last_part) = result.split("::").last() {
        result = last_part.to_string();
    }

    // Extract base type from templates (vector<int> -> vector)
    if let Some(open_bracket) = result.find('<') {
        result = result[..open_bracket].to_string();
    }

    result.trim().to_string()
}

/// Process a field declaration inside a class/struct, creating `Property` /
/// `Constant` nodes plus `TypeOf` (with `TypeOfContext::Field` + bare-name)
/// and `Reference` edges.
///
/// Per cross-language-field-emission/02_DESIGN §3.1.1 + §4.1:
/// - Qualified-name format: `Class.field`. Only the LAST separator is `.`;
///   the class chain itself keeps `::` (e.g. `demo::Outer::Inner.field`).
/// - `const` and `constexpr` declarations emit `NodeKind::Constant`; everything
///   else emits `NodeKind::Property`.
/// - The `static` keyword sets `is_static = true`. Per design §3.4 only the
///   `static` keyword controls this — bare `constexpr` does NOT imply static.
/// - Visibility flows in from `walk_class_body` (defaults: `class` →
///   `"private"`, `struct` → `"public"`).
/// - The `TypeOf` edge uses `TypeOfContext::Field` and stores the **bare**
///   field name in its `name` metadata (not the qualified form).
#[allow(clippy::unnecessary_wraps, clippy::too_many_lines)]
fn process_field_declaration(
    node: Node,
    content: &[u8],
    class_qualified_name: &str,
    visibility: &str,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract type and field names from the field_declaration
    let mut field_type_text = None;
    let mut field_names = Vec::new();
    // Modifiers on the declaration itself — used to pick Property vs Constant
    // and to compute is_static.
    let mut is_static_kw = false;
    let mut is_const = false;
    let mut is_constexpr = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "primitive_type" => {
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "qualified_identifier" => {
                // Handle qualified types like std::string
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "template_type" => {
                // Handle template types like std::vector<int>
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "sized_type_specifier" => {
                // Handle sized types like unsigned long, long long
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "type_qualifier" => {
                // Type qualifiers carry semantic information (`const`,
                // `volatile`, ...) and — for older tree-sitter-cpp grammars —
                // also `constexpr`. We always inspect the text so the
                // const/constexpr classification is accurate, and we only
                // promote it to `field_type_text` as a fallback when no
                // explicit type child was seen.
                if let Ok(text) = child.utf8_text(content) {
                    let trimmed = text.trim();
                    if trimmed == "const" {
                        is_const = true;
                    } else if trimmed == "constexpr" {
                        is_constexpr = true;
                    }
                    if field_type_text.is_none() {
                        field_type_text = Some(text.to_string());
                    }
                }
            }
            "storage_class_specifier" => {
                // `static`, `extern`, `register`, `mutable`, `thread_local`,
                // and (in newer grammars) `constexpr`.
                if let Ok(text) = child.utf8_text(content) {
                    let trimmed = text.trim();
                    if trimmed == "static" {
                        is_static_kw = true;
                    } else if trimmed == "constexpr" {
                        is_constexpr = true;
                    }
                }
            }
            "auto" => {
                // Handle auto type deduction
                field_type_text = Some("auto".to_string());
            }
            "decltype" => {
                // Handle decltype(expr)
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "struct_specifier" | "class_specifier" | "enum_specifier" | "union_specifier" => {
                // Handle inline struct/class/enum/union declarations
                if let Ok(text) = child.utf8_text(content) {
                    field_type_text = Some(text.to_string());
                }
            }
            "field_identifier" => {
                if let Ok(name) = child.utf8_text(content) {
                    field_names.push(name.trim().to_string());
                }
            }
            "field_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "init_declarator" => {
                // Recursively extract field name from declarators
                if let Some(name) = extract_field_name(child, content) {
                    field_names.push(name);
                }
            }
            _ => {}
        }
    }

    // If we found a type and at least one field name, create the nodes and edges
    if let Some(type_text) = field_type_text {
        let base_type = strip_type_qualifiers(&type_text);
        let is_constant = is_const || is_constexpr;

        for field_name in field_names {
            // Per design §3.1.1: only the LAST separator migrates to `.`;
            // the class chain (`namespace::Outer::Inner`) keeps `::`.
            let field_qualified = format!("{class_qualified_name}.{field_name}");
            let span = span_from_node(node);

            // AC-2 + AC-3 + AC-4: pick the right node kind, propagate
            // is_static from the `static` keyword, and forward visibility
            // from the enclosing access specifier.
            let field_id = if is_constant {
                helper.add_constant_with_name_static_and_visibility(
                    &field_name,
                    &field_qualified,
                    Some(span),
                    is_static_kw,
                    Some(visibility),
                )
            } else {
                helper.add_property_with_name_static_and_visibility(
                    &field_name,
                    &field_qualified,
                    Some(span),
                    is_static_kw,
                    Some(visibility),
                )
            };

            // Create a Type node for the base type (if not primitive)
            let type_id = helper.add_type(&base_type, None);

            // AC-5: TypeOf edge with Field context + bare field name.
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(sqry_core::graph::unified::edge::kind::TypeOfContext::Field),
                None,
                Some(&field_name),
            );

            // Reference edge preserved for backward-compatible "uses type"
            // queries.
            helper.add_reference_edge(field_id, type_id);
        }
    }

    Ok(())
}

/// Process file-level variable declarations (global variables)
#[allow(clippy::unnecessary_wraps)]
fn process_global_variable_declaration(
    node: Node,
    content: &[u8],
    namespace_stack: &[String],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Check if this is a declaration node (not a field_declaration, which is class-specific)
    if node.kind() != "declaration" {
        return Ok(());
    }

    // Skip function declarations (they have function_declarator children)
    // These are handled separately via function_definition nodes
    let mut cursor_check = node.walk();
    for child in node.children(&mut cursor_check) {
        if child.kind() == "function_declarator" {
            return Ok(());
        }
    }

    // Extract type and variable names
    let mut type_text = None;
    let mut var_names = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "primitive_type" | "qualified_identifier" | "template_type" => {
                if let Ok(text) = child.utf8_text(content) {
                    type_text = Some(text.to_string());
                }
            }
            "init_declarator" => {
                // Extract variable name from init_declarator
                if let Some(declarator) = child.child_by_field_name("declarator")
                    && let Some(name) = extract_declarator_name(declarator, content)
                {
                    var_names.push(name);
                }
            }
            "pointer_declarator" | "reference_declarator" => {
                if let Some(name) = extract_declarator_name(child, content) {
                    var_names.push(name);
                }
            }
            "identifier" => {
                // Direct identifier for simple declarations
                if let Ok(name) = child.utf8_text(content) {
                    var_names.push(name.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(type_text) = type_text {
        let base_type = strip_type_qualifiers(&type_text);

        for var_name in var_names {
            // Build qualified name with namespace
            let qualified = if namespace_stack.is_empty() {
                var_name.clone()
            } else {
                format!("{}::{}", namespace_stack.join("::"), var_name)
            };

            let span = span_from_node(node);

            // Create variable node (global variables are public by default)
            let var_id = helper.add_node_with_visibility(
                &qualified,
                Some(span),
                sqry_core::graph::unified::node::NodeKind::Variable,
                Some("public"),
            );
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(var_id);

            // Create Type node
            let type_id = helper.add_type(&base_type, None);

            // Add TypeOf and Reference edges
            helper.add_typeof_edge(var_id, type_id);
            helper.add_reference_edge(var_id, type_id);
        }
    }

    Ok(())
}

/// Extract variable/parameter name from a declarator node
fn extract_declarator_name(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => {
            if let Ok(name) = node.utf8_text(content) {
                Some(name.to_string())
            } else {
                None
            }
        }
        "pointer_declarator" | "reference_declarator" | "array_declarator" => {
            // Recurse to find the actual name
            if let Some(inner) = node.child_by_field_name("declarator") {
                extract_declarator_name(inner, content)
            } else {
                // Try looking for identifier child directly
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "identifier"
                        && let Ok(name) = child.utf8_text(content)
                    {
                        return Some(name.to_string());
                    }
                }
                None
            }
        }
        "init_declarator" => {
            // Extract from the declarator field
            if let Some(inner) = node.child_by_field_name("declarator") {
                extract_declarator_name(inner, content)
            } else {
                None
            }
        }
        "field_declarator" => {
            // Recurse to find the actual name
            if let Some(inner) = node.child_by_field_name("declarator") {
                extract_declarator_name(inner, content)
            } else {
                // Try to extract directly
                if let Ok(name) = node.utf8_text(content) {
                    Some(name.to_string())
                } else {
                    None
                }
            }
        }
        _ => None,
    }
}

/// Resolve an elaborated type reference the way C++ name lookup does:
/// innermost enclosing scope first, then outward.
///
/// `class Outer { class Inner; class Inner *slot; };` declares `Outer::Inner`
/// and then names it. Qualifying the reference with the namespace stack alone
/// minted a SECOND, namespace-level `Inner`, so the declaration and its use
/// were two unrelated nodes. Qualifying it with the class stack was the
/// original defect in the other direction: a member naming a type declared
/// elsewhere got the fabricated name `Outer::Payload`.
///
/// So neither: try each enclosing scope, innermost first, and take the first
/// one the helper has already minted a compatible node for. Falling back to
/// the namespace-qualified name keeps the previous behaviour for a type this
/// file has not declared, which is the common case.
///
/// The lookup only sees nodes minted EARLIER in this file, which is exactly
/// C++'s own rule for an elaborated reference to a nested type: the
/// declaration has to precede the use.
fn resolve_elaborated_reference(
    helper: &GraphBuildHelper,
    namespace_stack: &[String],
    class_stack: &[String],
    inner_name: &str,
    inner_kind: NodeKind,
) -> String {
    // `struct X` may name a type declared as `class X` and vice versa, so try
    // the site's own kind first and then its siblings.
    let mut kinds = vec![inner_kind];
    for candidate in [NodeKind::Class, NodeKind::Struct, NodeKind::Enum] {
        if candidate != inner_kind {
            kinds.push(candidate);
        }
    }

    for depth in (0..=class_stack.len()).rev() {
        let qualified = build_qualified_name(namespace_stack, &class_stack[..depth], inner_name);
        if kinds
            .iter()
            .any(|kind| helper.lookup_node(&qualified, *kind).is_some())
        {
            return qualified;
        }
    }

    build_qualified_name(namespace_stack, &[], inner_name)
}

/// The class body's members, with preprocessor conditionals flattened.
///
/// `#ifdef` and friends wrap their members in a `preproc_*` node, so iterating
/// the body's DIRECT children skips everything inside a conditional. Those
/// members then fell through to the generic file walker with the class stack
/// still pushed, which is how `Holder::Payload` came back for a guarded member
/// (issue #748).
///
/// Every arm of a conditional is yielded, `#ifdef` and `#else` alike. That is
/// deliberate and matches how the rest of this plugin treats preprocessor
/// branches: the graph describes what the source says, not what one particular
/// set of `-D` flags selects. A member declared in two arms resolves to one
/// node through the helper's cache either way.
fn class_body_members<'tree>(body_node: Node<'tree>) -> Vec<Node<'tree>> {
    fn push_members<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>, depth: usize) {
        // Conditionals nest, but not deeply in practice. The cap is a guard
        // against a pathological file, not a real limit.
        if depth > 16 {
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind().starts_with("preproc_") {
                push_members(child, out, depth + 1);
            } else {
                out.push(child);
            }
        }
    }

    let mut out = Vec::new();
    push_members(body_node, &mut out, 0);
    out
}

/// Walk a class/struct body, processing field declarations and methods with visibility tracking.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn walk_class_body(
    body_node: Node,
    content: &[u8],
    class_qualified_name: &str,
    is_struct: bool,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    seen_includes: &mut HashSet<String>,
    namespace_stack: &mut Vec<String>,
    class_stack: &mut Vec<String>,
    ffi_registry: &FfiRegistry,
    pure_virtual_registry: &PureVirtualRegistry,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    // Default visibility: struct = public, class = private
    let mut current_visibility = if is_struct { "public" } else { "private" };

    for child in class_body_members(body_node) {
        budget.checkpoint("cpp:walk_class_body")?;
        match child.kind() {
            "access_specifier" => {
                // Update current visibility (public:, private:, protected:)
                if let Ok(text) = child.utf8_text(content) {
                    let spec = text.trim().trim_end_matches(':').trim();
                    current_visibility = spec;
                }
            }
            "field_declaration" => {
                // First, look for nested type declarations (class/struct/union)
                // as direct children of the field_declaration. tree-sitter-cpp
                // wraps `class Inner { ... };` and `union { int a; };` shapes
                // declared inside a class body in a `field_declaration` parent.
                //
                // - NAMED nested class/struct (e.g. `class Inner { int x; };`)
                //   must recurse with the extended class chain so the inner
                //   field qualifies as `Outer::Inner.x`. Default visibility
                //   resets to the nested type's own default (struct = public,
                //   class = private), independent of the OUTER access state.
                // - ANONYMOUS union/struct/class (e.g. `union { int a; };`)
                //   injects its members into the enclosing class per C++
                //   semantics; recurse with the OUTER `class_qualified_name`
                //   so members emit as `Outer.a` / `Outer.b`. Visibility for
                //   injected members inherits the OUTER `current_visibility`.
                let mut handled_nested = false;
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    let kind = inner.kind();
                    if !matches!(
                        kind,
                        "class_specifier"
                            | "struct_specifier"
                            | "union_specifier"
                            | "enum_specifier"
                    ) {
                        continue;
                    }

                    let is_struct_or_union = matches!(kind, "struct_specifier" | "union_specifier");

                    if let Some(name_node) = inner.child_by_field_name("name") {
                        // NAMED nested type: emit the type node itself (so it is
                        // discoverable via `kind:class` / `kind:struct` /
                        // `kind:enum`), wire its inheritance/implements edges, then
                        // walk its body with the extended chain so members qualify
                        // as `Outer::Inner.field`.
                        //
                        // Visibility = the enclosing access state (`current_visibility`),
                        // matching C++ member-access rules for nested types. Nested
                        // types are NEVER exported at file scope, so no Export edge is
                        // added here (contrast with the top-level class/struct arm in
                        // `walk_tree_for_graph`).
                        if let Ok(inner_name) = name_node.utf8_text(content) {
                            let inner_name = inner_name.trim();
                            let nested_qualified = format!("{class_qualified_name}::{inner_name}");
                            let nested_span = span_from_node(inner);

                            let inner_kind = if kind == "enum_specifier" {
                                NodeKind::Enum
                            } else if is_struct_or_union {
                                NodeKind::Struct
                            } else {
                                NodeKind::Class
                            };
                            let role = classify_tagged_specifier(inner);

                            // A member declared with an ELABORATED TYPE REFERENCE
                            // (`struct Payload *slot;`) parses as a specifier with
                            // a name and no body. It is not a nested type at all:
                            // the type is declared elsewhere, the extent belongs to
                            // the member declaration, and `Outer::Payload` is a name
                            // nothing in the source has (issue #748).
                            //
                            // C++ lookup for the elaborated name starts at the
                            // INNERMOST enclosing scope and works outward, so
                            // resolve it that way rather than fabricating
                            // `Outer::Payload` for a type declared elsewhere. No
                            // visibility either: the access specifier governs the
                            // member, not the type it names. And do not walk a body
                            // it has not got.
                            if role == TaggedSpecifierRole::Reference {
                                let referenced_qualified = resolve_elaborated_reference(
                                    helper,
                                    namespace_stack,
                                    class_stack,
                                    inner_name,
                                    inner_kind,
                                );
                                helper.add_call_site_node(
                                    &referenced_qualified,
                                    nested_span,
                                    inner_kind,
                                );
                                continue;
                            }

                            // `class InnerFwd;` inside a class body IS a nested
                            // type declaration: `Outer::InnerFwd` is a real name
                            // and the node is a definition. It just has no body
                            // to fingerprint or to walk.
                            if role == TaggedSpecifierRole::ForwardDeclaration {
                                helper.add_bodyless_declaration_node(
                                    &nested_qualified,
                                    nested_span,
                                    inner_kind,
                                    Some(current_visibility),
                                );
                                continue;
                            }

                            // NodeKind: enum → Enum; struct/union → Struct; class → Class.
                            // (NodeKind has no dedicated Union variant; unions map to
                            // Struct, consistent with the nested-member walk below.)
                            if kind == "enum_specifier" {
                                // Nested enums carry the enclosing access
                                // visibility, identical to the nested
                                // class/struct path below.
                                helper.add_enum_with_visibility(
                                    &nested_qualified,
                                    Some(nested_span),
                                    Some(current_visibility),
                                );
                            } else {
                                let nested_id = if is_struct_or_union {
                                    helper.add_struct_with_visibility(
                                        &nested_qualified,
                                        Some(nested_span),
                                        Some(current_visibility),
                                    )
                                } else {
                                    helper.add_class_with_visibility(
                                        &nested_qualified,
                                        Some(nested_span),
                                        Some(current_visibility),
                                    )
                                };
                                build_inheritance_and_implements_edges(
                                    inner,
                                    content,
                                    &nested_qualified,
                                    nested_id,
                                    helper,
                                    namespace_stack,
                                    pure_virtual_registry,
                                )?;
                            }

                            // Recurse into the body for members. Enums carry no
                            // field members we model, so only class/struct/union
                            // bodies are walked. Emitting the node above already
                            // marks the declaration handled.
                            if matches!(
                                kind,
                                "class_specifier" | "struct_specifier" | "union_specifier"
                            ) && let Some(body) = inner.child_by_field_name("body")
                            {
                                walk_class_body(
                                    body,
                                    content,
                                    &nested_qualified,
                                    is_struct_or_union,
                                    ast_graph,
                                    helper,
                                    seen_includes,
                                    namespace_stack,
                                    class_stack,
                                    ffi_registry,
                                    pure_virtual_registry,
                                    budget,
                                )?;
                            }
                            handled_nested = true;
                        }
                    } else if let Some(body) = inner.child_by_field_name("body") {
                        // ANONYMOUS nested type: inject members into enclosing
                        // class. Process direct field_declaration children
                        // with OUTER qualifier + OUTER visibility so members
                        // surface as `Outer.member`.
                        let mut anon_cursor = body.walk();
                        for anon_child in body.children(&mut anon_cursor) {
                            if anon_child.kind() == "field_declaration" {
                                process_field_declaration(
                                    anon_child,
                                    content,
                                    class_qualified_name,
                                    current_visibility,
                                    helper,
                                )?;
                            }
                        }
                        handled_nested = true;
                    }
                }

                // Process the field_declaration itself unless we exclusively
                // handled it as a pure nested type with no instance declarator
                // (e.g. `class Inner { ... };` has a class_specifier but no
                // field_identifier). `process_field_declaration` is harmless
                // when no `field_identifier` / declarator child exists — it
                // collects an empty `field_names` list and falls through.
                // We still call it so cases that mix a nested type with an
                // instance declarator (`class Inner { } member;`) keep
                // emitting the `Outer.member` Property too. When
                // `handled_nested` is true and the type child is absent of
                // declarator children, the function is effectively a no-op
                // (no field name → no node).
                let _ = handled_nested;
                process_field_declaration(
                    child,
                    content,
                    class_qualified_name,
                    current_visibility,
                    helper,
                )?;
            }
            "function_definition" => {
                // Process method with current visibility
                // Extract function context from AST graph by matching start position
                if let Some(context) = ast_graph.context_for_start(child.start_byte()) {
                    let span = span_from_node(child);
                    helper.add_method_with_signature(
                        &context.qualified_name,
                        Some(span),
                        false, // C++ doesn't have async
                        context.is_static,
                        Some(current_visibility),
                        context.return_type.as_deref(),
                    );
                }
                // Recurse into function body to process call expressions
                walk_tree_for_graph(
                    child,
                    content,
                    ast_graph,
                    helper,
                    seen_includes,
                    namespace_stack,
                    class_stack,
                    ffi_registry,
                    pure_virtual_registry,
                    budget,
                )?;
            }
            _ => {
                // Recurse into other nodes (nested classes, etc.)
                walk_tree_for_graph(
                    child,
                    content,
                    ast_graph,
                    helper,
                    seen_includes,
                    namespace_stack,
                    class_stack,
                    ffi_registry,
                    pure_virtual_registry,
                    budget,
                )?;
            }
        }
    }

    Ok(())
}

/// Walk the tree and populate the staging graph.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)] // Central traversal; refactor after C++ AST stabilizes.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    seen_includes: &mut HashSet<String>,
    namespace_stack: &mut Vec<String>,
    class_stack: &mut Vec<String>,
    ffi_registry: &FfiRegistry,
    pure_virtual_registry: &PureVirtualRegistry,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:walk_tree_for_graph")?;
    match node.kind() {
        "preproc_include" => {
            // Handle #include directives - create Import edges
            build_import_edge(node, content, helper, seen_includes)?;
        }
        "linkage_specification" => {
            // Handle extern "C" blocks - create FFI function nodes
            build_ffi_block_for_staging(node, content, helper, namespace_stack);
        }
        "namespace_definition" => {
            // Extract namespace name and track context
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(ns_name) = name_node.utf8_text(content)
            {
                namespace_stack.push(ns_name.trim().to_string());

                // Recurse into namespace body
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_tree_for_graph(
                        child,
                        content,
                        ast_graph,
                        helper,
                        seen_includes,
                        namespace_stack,
                        class_stack,
                        ffi_registry,
                        pure_virtual_registry,
                        budget,
                    )?;
                }

                namespace_stack.pop();
                return Ok(());
            }
        }
        "class_specifier" | "struct_specifier" | "union_specifier" => {
            // Extract class/struct/union name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let class_name = class_name.trim();
                let span = span_from_node(node);
                // Unions have no dedicated NodeKind variant; they map to Struct,
                // matching the nested-type handling in `walk_class_body`.
                let is_struct = matches!(node.kind(), "struct_specifier" | "union_specifier");

                // Build qualified class name
                let qualified_class =
                    build_qualified_name(namespace_stack, class_stack, class_name);

                // Issue #748. `struct payload *p` in a parameter list parses
                // as a `struct_specifier` with a name and no members, exactly
                // like the forward declaration `struct payload;`. The first is
                // a reference to a type declared elsewhere, holding the
                // parameter list's extent; the second is a real declaration of
                // identity. Both are outside the body plane, and only the
                // second is a definition.
                let role = classify_tagged_specifier(node);
                let node_kind = if is_struct {
                    NodeKind::Struct
                } else {
                    NodeKind::Class
                };
                let class_id = match role {
                    TaggedSpecifierRole::Reference => {
                        helper.add_call_site_node(&qualified_class, span, node_kind)
                    }
                    TaggedSpecifierRole::ForwardDeclaration => helper
                        .add_bodyless_declaration_node(
                            &qualified_class,
                            span,
                            node_kind,
                            Some("public"),
                        ),
                    TaggedSpecifierRole::Definition => {
                        if is_struct {
                            // Add class/struct node with qualified name
                            helper.add_struct_with_visibility(
                                &qualified_class,
                                Some(span),
                                Some("public"),
                            )
                        } else {
                            helper.add_class_with_visibility(
                                &qualified_class,
                                Some(span),
                                Some("public"),
                            )
                        }
                    }
                };

                // Handle inheritance with qualified name
                // Also check for Implements edges (inheriting from pure virtual interfaces)
                build_inheritance_and_implements_edges(
                    node,
                    content,
                    &qualified_class,
                    class_id,
                    helper,
                    namespace_stack,
                    pure_virtual_registry,
                )?;

                // Export classes/structs at file/namespace scope (not nested classes)
                // Nested classes have internal linkage unless explicitly exported.
                //
                // Only a DEFINITION exports (issue #748). A file that merely
                // names a type does not provide it, and neither does one that
                // only forward-declares it.
                if class_stack.is_empty() && role == TaggedSpecifierRole::Definition {
                    let module_id = helper.add_module(FILE_MODULE_NAME, None);
                    helper.add_export_edge(module_id, class_id);
                }

                // Track class context for nested classes
                class_stack.push(class_name.to_string());

                // Process class body with visibility tracking
                // Default visibility: struct = public, class = private
                if let Some(body) = node.child_by_field_name("body") {
                    walk_class_body(
                        body,
                        content,
                        &qualified_class,
                        is_struct,
                        ast_graph,
                        helper,
                        seen_includes,
                        namespace_stack,
                        class_stack,
                        ffi_registry,
                        pure_virtual_registry,
                        budget,
                    )?;
                }

                class_stack.pop();
                return Ok(());
            }
        }
        "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(enum_name) = name_node.utf8_text(content)
            {
                let enum_name = enum_name.trim();
                let span = span_from_node(node);
                let qualified_enum = build_qualified_name(namespace_stack, class_stack, enum_name);

                // Same classification as the class/struct/union arm above
                // (issue #748). A bodyless `enum Color` in a parameter list is
                // a REFERENCE holding the parameter list's extent; two files
                // whose only shared text is such a prototype line otherwise
                // hash identically and are reported as duplicate bodies.
                // `enum State : int;` at file scope is a real forward
                // declaration and keeps its definition bit.
                match classify_tagged_specifier(node) {
                    TaggedSpecifierRole::Reference => {
                        helper.add_call_site_node(&qualified_enum, span, NodeKind::Enum);
                    }
                    TaggedSpecifierRole::ForwardDeclaration => {
                        helper.add_bodyless_declaration_node(
                            &qualified_enum,
                            span,
                            NodeKind::Enum,
                            None,
                        );
                    }
                    TaggedSpecifierRole::Definition => {
                        let enum_id = helper.add_enum(&qualified_enum, Some(span));

                        // Only a definition exports. Neither a reference nor a
                        // forward declaration makes the file provide the type.
                        if class_stack.is_empty() {
                            let module_id = helper.add_module(FILE_MODULE_NAME, None);
                            helper.add_export_edge(module_id, enum_id);
                        }
                    }
                }
            }
        }
        "function_definition" => {
            // Skip if we're inside a class body - methods are handled by walk_class_body
            // to ensure correct visibility tracking. This check prevents double-adding
            // methods with incorrect visibility.
            if !class_stack.is_empty() {
                // Don't process the function definition as a node here, but do recurse
                // into its body to find call expressions
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_tree_for_graph(
                        child,
                        content,
                        ast_graph,
                        helper,
                        seen_includes,
                        namespace_stack,
                        class_stack,
                        ffi_registry,
                        pure_virtual_registry,
                        budget,
                    )?;
                }
                return Ok(());
            }

            // Extract function context from AST graph by matching start position
            if let Some(context) = ast_graph.context_for_start(node.start_byte()) {
                let span = span_from_node(node);

                // Determine if this is a method or free function based on context
                if context.class_stack.is_empty() {
                    // This is a free function
                    // Visibility: static = private (internal linkage), non-static = public (external linkage)
                    let visibility = if context.is_static {
                        "private"
                    } else {
                        "public"
                    };
                    let fn_id = helper.add_function_with_signature(
                        &context.qualified_name,
                        Some(span),
                        false, // C++ doesn't have async
                        false, // C++ doesn't use unsafe keyword
                        Some(visibility),
                        context.return_type.as_deref(),
                    );

                    // Export non-static free functions (static functions have internal linkage)
                    if !context.is_static {
                        let module_id = helper.add_module(FILE_MODULE_NAME, None);
                        helper.add_export_edge(module_id, fn_id);
                    }
                } else {
                    // This is an out-of-class method definition (e.g., Resource::Resource())
                    // These are public by default in C++ (they must be declared in the class first)
                    // Note: We can't determine actual visibility here as that requires
                    // correlating with the in-class declaration
                    helper.add_method_with_signature(
                        &context.qualified_name,
                        Some(span),
                        false, // C++ doesn't have async
                        context.is_static,
                        Some("public"), // Default for out-of-class definitions
                        context.return_type.as_deref(),
                    );
                }
            }
        }
        "call_expression" => {
            // Build call edge
            if let Ok(Some((caller_qname, callee_qname, argument_count, span))) =
                build_call_for_staging(ast_graph, node, content)
            {
                // Ensure caller node exists
                let caller_function_id =
                    helper.ensure_callee(&caller_qname, span, CalleeKindHint::Function);
                let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);

                // Check if the callee is a known FFI function
                // Only do FFI lookup for unqualified calls (no ::)
                let is_unqualified = !callee_qname.contains("::");
                if is_unqualified {
                    if let Some((ffi_qualified, ffi_convention)) = ffi_registry.get(&callee_qname) {
                        // This is a call to an FFI function - create FfiCall edge
                        let ffi_target_id =
                            helper.ensure_callee(ffi_qualified, span, CalleeKindHint::Function);
                        helper.add_ffi_edge(caller_function_id, ffi_target_id, *ffi_convention);
                    } else {
                        // Regular call - create normal Call edge
                        let target_function_id =
                            helper.ensure_callee(&callee_qname, span, CalleeKindHint::Function);
                        helper.add_call_edge_full_with_span(
                            caller_function_id,
                            target_function_id,
                            argument_count,
                            false,
                            vec![span],
                        );
                    }
                } else {
                    // Qualified call - create normal Call edge
                    let target_function_id =
                        helper.ensure_callee(&callee_qname, span, CalleeKindHint::Function);
                    helper.add_call_edge_full_with_span(
                        caller_function_id,
                        target_function_id,
                        argument_count,
                        false,
                        vec![span],
                    );
                }
            }
        }
        "declaration" => {
            // Handle global/file-level variable declarations (not inside classes)
            // Only process if we're not inside a class (class members are handled in walk_class_body)
            if class_stack.is_empty() {
                process_global_variable_declaration(node, content, namespace_stack, helper)?;
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(
            child,
            content,
            ast_graph,
            helper,
            seen_includes,
            namespace_stack,
            class_stack,
            ffi_registry,
            pure_virtual_registry,
            budget,
        )?;
    }

    Ok(())
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<(String, String, usize, Span)>> {
    // Find the enclosing function context
    let call_context = ast_graph.find_enclosing(call_node.start_byte());
    let caller_qualified_name = if let Some(ctx) = call_context {
        ctx.qualified_name.clone()
    } else {
        // Top-level call (e.g., global initializer)
        return Ok(None);
    };

    let Some(function_node) = call_node.child_by_field_name("function") else {
        return Ok(None);
    };

    let callee_text = function_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim();

    if callee_text.is_empty() {
        return Ok(None);
    }

    // Resolve callee name using context
    let target_qualified_name = if let Some(ctx) = call_context {
        resolve_callee_name(function_node, callee_text, ctx, ast_graph, content)
    } else {
        callee_text.to_string()
    };

    let span = span_from_node(call_node);
    let argument_count = count_arguments(call_node);

    Ok(Some((
        caller_qualified_name,
        target_qualified_name,
        argument_count,
        span,
    )))
}

/// Build import edge for `#include` directives.
///
/// Handles both system includes (`<header>`) and local includes (`"header"`).
/// Per the implementation plan, include type (system/local) is tracked via
/// node metadata, not the edge's alias field (alias is for import renaming only).
/// Duplicate includes are deduplicated using the `seen_includes` set.
fn build_import_edge(
    include_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    seen_includes: &mut HashSet<String>,
) -> GraphResult<()> {
    // Look for path child (system_lib_string or string_literal)
    let path_node = include_node.child_by_field_name("path").or_else(|| {
        // Fallback: find first child that looks like a path
        let mut cursor = include_node.walk();
        include_node.children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "system_lib_string" | "string_literal" | "string_content"
            )
        })
    });

    let Some(path_node) = path_node else {
        return Ok(());
    };

    let include_path = path_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(include_node),
            reason: "failed to read include path".to_string(),
        })?
        .trim();

    if include_path.is_empty() {
        return Ok(());
    }

    // Determine include type and clean up path
    let is_system_include = include_path.starts_with('<') && include_path.ends_with('>');
    let cleaned_path = if is_system_include {
        // System include: <iostream> -> iostream
        include_path.trim_start_matches('<').trim_end_matches('>')
    } else {
        // Local include: "myheader.hpp" -> myheader.hpp
        include_path.trim_start_matches('"').trim_end_matches('"')
    };

    if cleaned_path.is_empty() {
        return Ok(());
    }

    // Deduplicate includes - only add if not seen before
    if !seen_includes.insert(cleaned_path.to_string()) {
        return Ok(()); // Already seen this include
    }

    // Create module node for the file being compiled (importer)
    let file_module_id = helper.add_module("<file>", None);

    // Create import node for the included header
    let span = span_from_node(include_node);
    let import_id = helper.add_import(cleaned_path, Some(span));

    // Add import edge - no alias for #include (alias is for renaming, which C++ doesn't support)
    // is_wildcard is false since #include brings in the whole header (but it's not a wildcard import)
    helper.add_import_edge(file_module_id, import_id);

    Ok(())
}

// ================================
// FFI Support Functions
// ================================

/// Collect FFI declarations from extern "C" blocks (Pass 1).
///
/// This function walks the entire AST to find all `extern "C" { ... }` blocks
/// and populates the FFI registry with function name → (qualified name, convention)
/// mappings. This must be done before processing calls so that FFI calls can be
/// properly linked regardless of source code order.
fn collect_ffi_declarations(
    node: Node<'_>,
    content: &[u8],
    ffi_registry: &mut FfiRegistry,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:collect_ffi_declarations")?;
    if node.kind() == "linkage_specification" {
        // Get the ABI string (e.g., "C")
        let abi = extract_ffi_abi(node, content);
        let convention = abi_to_convention(&abi);

        // Find the body child (declaration_list or single declaration)
        if let Some(body_node) = node.child_by_field_name("body") {
            collect_ffi_from_body(body_node, content, &abi, convention, ffi_registry);
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ffi_declarations(child, content, ffi_registry, budget)?;
    }

    Ok(())
}

/// Collect FFI declarations from a linkage specification body.
fn collect_ffi_from_body(
    body_node: Node<'_>,
    content: &[u8],
    abi: &str,
    convention: FfiConvention,
    ffi_registry: &mut FfiRegistry,
) {
    match body_node.kind() {
        "declaration_list" => {
            // Multiple declarations in the block
            let mut cursor = body_node.walk();
            for decl in body_node.children(&mut cursor) {
                if decl.kind() == "declaration"
                    && let Some(fn_name) = extract_ffi_function_name(decl, content)
                {
                    let qualified = format!("extern::{abi}::{fn_name}");
                    ffi_registry.insert(fn_name, (qualified, convention));
                }
            }
        }
        "declaration" => {
            // Single declaration (e.g., extern "C" void foo();)
            if let Some(fn_name) = extract_ffi_function_name(body_node, content) {
                let qualified = format!("extern::{abi}::{fn_name}");
                ffi_registry.insert(fn_name, (qualified, convention));
            }
        }
        _ => {}
    }
}

/// Extract function name from an FFI declaration.
fn extract_ffi_function_name(decl_node: Node<'_>, content: &[u8]) -> Option<String> {
    // Look for declarator field which contains the function declarator
    if let Some(declarator_node) = decl_node.child_by_field_name("declarator") {
        return extract_function_name_from_declarator(declarator_node, content);
    }
    None
}

/// Recursively extract function name from a declarator node.
fn extract_function_name_from_declarator(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "function_declarator" => {
            // Function declarator has a nested declarator with the name
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_function_name_from_declarator(inner, content);
            }
        }
        "identifier" => {
            // Found the name
            if let Ok(name) = node.utf8_text(content) {
                let name = name.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
        "pointer_declarator" | "reference_declarator" => {
            // Handle pointer/reference declarators (e.g., int* (*foo)())
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_function_name_from_declarator(inner, content);
            }
        }
        "parenthesized_declarator" => {
            // Handle parenthesized declarators
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = extract_function_name_from_declarator(child, content) {
                    return Some(name);
                }
            }
        }
        _ => {}
    }
    None
}

/// Extract the ABI string from an extern "X" block.
///
/// Returns the ABI string (e.g., "C") or "C" as default.
fn extract_ffi_abi(node: Node<'_>, content: &[u8]) -> String {
    // Look for the "value" field which contains the string literal
    if let Some(value_node) = node.child_by_field_name("value")
        && value_node.kind() == "string_literal"
    {
        // Look for string_content child
        let mut cursor = value_node.walk();
        for child in value_node.children(&mut cursor) {
            if child.kind() == "string_content"
                && let Ok(text) = child.utf8_text(content)
            {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    // Default to "C" if no ABI specified
    "C".to_string()
}

/// Convert an ABI string to an FFI calling convention.
fn abi_to_convention(abi: &str) -> FfiConvention {
    match abi.to_lowercase().as_str() {
        "system" => FfiConvention::System,
        "stdcall" => FfiConvention::Stdcall,
        "fastcall" => FfiConvention::Fastcall,
        "cdecl" => FfiConvention::Cdecl,
        _ => FfiConvention::C, // Default to C
    }
}

/// Build FFI function declarations from extern "C" blocks.
///
/// Creates Function nodes for FFI declarations with unsafe=true.
fn build_ffi_block_for_staging(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    namespace_stack: &[String],
) {
    // Get the ABI string
    let abi = extract_ffi_abi(node, content);

    // Find the body child
    if let Some(body_node) = node.child_by_field_name("body") {
        build_ffi_from_body(body_node, content, &abi, helper, namespace_stack);
    }
}

/// Build FFI function nodes from a linkage specification body.
fn build_ffi_from_body(
    body_node: Node<'_>,
    content: &[u8],
    abi: &str,
    helper: &mut GraphBuildHelper,
    namespace_stack: &[String],
) {
    match body_node.kind() {
        "declaration_list" => {
            // Multiple declarations in the block
            let mut cursor = body_node.walk();
            for decl in body_node.children(&mut cursor) {
                if decl.kind() == "declaration"
                    && let Some(fn_name) = extract_ffi_function_name(decl, content)
                {
                    let span = span_from_node(decl);
                    // Build qualified name with namespace context
                    let qualified = if namespace_stack.is_empty() {
                        format!("extern::{abi}::{fn_name}")
                    } else {
                        format!("{}::extern::{abi}::{fn_name}", namespace_stack.join("::"))
                    };
                    // Add as unsafe function (FFI functions are inherently unsafe)
                    helper.add_function(
                        &qualified,
                        Some(span),
                        false, // not async
                        true,  // unsafe (FFI)
                    );
                }
            }
        }
        "declaration" => {
            // Single declaration
            if let Some(fn_name) = extract_ffi_function_name(body_node, content) {
                let span = span_from_node(body_node);
                let qualified = if namespace_stack.is_empty() {
                    format!("extern::{abi}::{fn_name}")
                } else {
                    format!("{}::extern::{abi}::{fn_name}", namespace_stack.join("::"))
                };
                helper.add_function(&qualified, Some(span), false, true);
            }
        }
        _ => {}
    }
}

// ================================
// Pure Virtual Interface Support
// ================================

/// Collect pure virtual interfaces (abstract classes with pure virtual methods).
///
/// A class is considered a "pure virtual interface" if it contains at least one
/// pure virtual method (declared with `= 0`). Classes that inherit from such
/// interfaces will get Implements edges instead of just Inherits edges.
fn collect_pure_virtual_interfaces(
    node: Node<'_>,
    content: &[u8],
    registry: &mut PureVirtualRegistry,
    budget: &mut BuildBudget,
) -> GraphResult<()> {
    budget.checkpoint("cpp:collect_pure_virtual_interfaces")?;
    if matches!(node.kind(), "class_specifier" | "struct_specifier")
        && let Some(name_node) = node.child_by_field_name("name")
        && let Ok(class_name) = name_node.utf8_text(content)
    {
        let class_name = class_name.trim();
        if !class_name.is_empty() && has_pure_virtual_methods(node, content) {
            registry.insert(class_name.to_string());
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_pure_virtual_interfaces(child, content, registry, budget)?;
    }

    Ok(())
}

/// Check if a class/struct has any pure virtual methods.
///
/// Pure virtual methods are declared as `virtual ReturnType name() = 0;`
fn has_pure_virtual_methods(class_node: Node<'_>, content: &[u8]) -> bool {
    if let Some(body) = class_node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            // Look for field_declaration with virtual and = 0
            if child.kind() == "field_declaration" && is_pure_virtual_declaration(child, content) {
                return true;
            }
        }
    }
    false
}

/// Check if a field declaration is a pure virtual method (has `virtual` and `= 0`).
fn is_pure_virtual_declaration(decl_node: Node<'_>, content: &[u8]) -> bool {
    let mut has_virtual = false;
    let mut has_pure_specifier = false;

    // Check children for virtual keyword and default_value of 0
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        match child.kind() {
            "virtual" => {
                has_virtual = true;
            }
            "number_literal" => {
                // Check if this is the pure virtual specifier (= 0)
                // The number_literal with value "0" after "=" indicates a pure virtual method
                if let Ok(text) = child.utf8_text(content)
                    && text.trim() == "0"
                {
                    has_pure_specifier = true;
                }
            }
            _ => {}
        }
    }

    has_virtual && has_pure_specifier
}

/// Build inheritance and implements edges for a class/struct.
///
/// For each base class:
/// - If the base class is a pure virtual interface, create an Implements edge
/// - Otherwise, create an Inherits edge
fn build_inheritance_and_implements_edges(
    class_node: Node<'_>,
    content: &[u8],
    _qualified_class_name: &str,
    child_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
    namespace_stack: &[String],
    pure_virtual_registry: &PureVirtualRegistry,
) -> GraphResult<()> {
    // Look for base_class_clause child
    let mut cursor = class_node.walk();
    let base_clause = class_node
        .children(&mut cursor)
        .find(|child| child.kind() == "base_class_clause");

    let Some(base_clause) = base_clause else {
        return Ok(()); // No inheritance
    };

    // Parse all base classes from the base_class_clause
    let mut clause_cursor = base_clause.walk();
    for child in base_clause.children(&mut clause_cursor) {
        match child.kind() {
            "type_identifier" => {
                let base_name = child
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(child),
                        reason: "failed to read base class name".to_string(),
                    })?
                    .trim();

                if !base_name.is_empty() {
                    // Qualify with namespace if present
                    let qualified_base = if namespace_stack.is_empty() {
                        base_name.to_string()
                    } else {
                        format!("{}::{}", namespace_stack.join("::"), base_name)
                    };

                    // Check if base is a pure virtual interface
                    if pure_virtual_registry.contains(base_name) {
                        // Create interface node and Implements edge
                        let interface_id = helper.add_interface(&qualified_base, None);
                        helper.add_implements_edge(child_id, interface_id);
                    } else {
                        // Regular inheritance - create Inherits edge
                        let parent_id = helper.add_class(&qualified_base, None);
                        helper.add_inherits_edge(child_id, parent_id);
                    }
                }
            }
            "qualified_identifier" => {
                // Already qualified - use as-is
                let base_name = child
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(child),
                        reason: "failed to read base class name".to_string(),
                    })?
                    .trim();

                if !base_name.is_empty() {
                    // Extract simple name for registry lookup
                    let simple_name = base_name.rsplit("::").next().unwrap_or(base_name);

                    if pure_virtual_registry.contains(simple_name) {
                        let interface_id = helper.add_interface(base_name, None);
                        helper.add_implements_edge(child_id, interface_id);
                    } else {
                        let parent_id = helper.add_class(base_name, None);
                        helper.add_inherits_edge(child_id, parent_id);
                    }
                }
            }
            "template_type" => {
                // Template base class: Base<T>
                if let Some(template_name_node) = child.child_by_field_name("name")
                    && let Ok(base_name) = template_name_node.utf8_text(content)
                {
                    let base_name = base_name.trim();
                    if !base_name.is_empty() {
                        let qualified_base =
                            if base_name.contains("::") || namespace_stack.is_empty() {
                                base_name.to_string()
                            } else {
                                format!("{}::{}", namespace_stack.join("::"), base_name)
                            };

                        // Template bases are typically not pure virtual interfaces
                        // but check anyway
                        if pure_virtual_registry.contains(base_name) {
                            let interface_id = helper.add_interface(&qualified_base, None);
                            helper.add_implements_edge(child_id, interface_id);
                        } else {
                            let parent_id = helper.add_class(&qualified_base, None);
                            helper.add_inherits_edge(child_id, parent_id);
                        }
                    }
                }
            }
            _ => {
                // Skip access specifiers, colons, commas, and other non-base nodes.
            }
        }
    }

    Ok(())
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

fn count_arguments(node: Node<'_>) -> usize {
    node.child_by_field_name("arguments").map_or(0, |args| {
        let mut count = 0;
        let mut cursor = args.walk();
        for child in args.children(&mut cursor) {
            if !matches!(child.kind(), "(" | ")" | ",") {
                count += 1;
            }
        }
        count
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_ffi_call_edge, assert_has_node, assert_has_node_with_kind,
        assert_has_node_with_kind_exact, collect_call_edges,
    };
    use sqry_core::graph::unified::node::NodeKind;
    use tree_sitter::Parser;

    fn parse_cpp(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("Failed to set Cpp language");
        parser
            .parse(source.as_bytes(), None)
            .expect("Failed to parse Cpp source")
    }

    fn test_budget() -> BuildBudget {
        BuildBudget::new(Path::new("test.cpp"))
    }

    fn extract_namespace_map_for_test(
        tree: &Tree,
        source: &str,
    ) -> HashMap<std::ops::Range<usize>, String> {
        let mut budget = test_budget();
        extract_namespace_map(tree.root_node(), source.as_bytes(), &mut budget)
            .expect("namespace extraction should succeed in tests")
    }

    fn extract_cpp_contexts_for_test(
        tree: &Tree,
        source: &str,
        namespace_map: &HashMap<std::ops::Range<usize>, String>,
    ) -> Vec<FunctionContext> {
        let mut budget = test_budget();
        extract_cpp_contexts(
            tree.root_node(),
            source.as_bytes(),
            namespace_map,
            &mut budget,
        )
        .expect("context extraction should succeed in tests")
    }

    fn extract_field_and_type_info_for_test(
        tree: &Tree,
        source: &str,
        namespace_map: &HashMap<std::ops::Range<usize>, String>,
    ) -> (QualifiedNameMap, QualifiedNameMap) {
        let mut budget = test_budget();
        extract_field_and_type_info(
            tree.root_node(),
            source.as_bytes(),
            namespace_map,
            &mut budget,
        )
        .expect("field/type extraction should succeed in tests")
    }

    #[test]
    fn test_build_graph_times_out_with_expired_budget() {
        let source = r"
            namespace demo {
                class Service {
                public:
                    void process() {}
                };
            }
        ";
        let tree = parse_cpp(source);
        let builder = CppGraphBuilder::new();
        let mut staging = StagingGraph::new();
        let mut budget = BuildBudget::already_expired(Path::new("timeout.cpp"));

        let err = builder
            .build_graph_with_budget(
                &tree,
                source.as_bytes(),
                Path::new("timeout.cpp"),
                &mut staging,
                &mut budget,
            )
            .expect_err("expired budget should force timeout");

        match err {
            GraphBuilderError::BuildTimedOut {
                file,
                phase,
                timeout_ms,
            } => {
                assert_eq!(file, PathBuf::from("timeout.cpp"));
                assert_eq!(phase, "cpp:extract_namespace_map");
                assert_eq!(timeout_ms, 1_000);
            }
            other => panic!("expected BuildTimedOut, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_class() {
        let source = "class User { }";
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node_with_kind(&staging, "User", NodeKind::Class);
    }

    #[test]
    fn test_extract_template_class() {
        let source = r"
            template <typename T>
            class Person {
            public:
                T name;
                T age;
            };
        ";
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node_with_kind(&staging, "Person", NodeKind::Class);
    }

    #[test]
    fn test_nested_named_types_emit_nodes() {
        // Regression: nested class/struct/union/enum declared inside a class body
        // must each emit their OWN type node (previously only their members were
        // staged, so `kind:class` / `kind:struct` / `kind:enum` could not see
        // them). Covers doubly-nested chains and namespace-nested chains.
        let source = r"
            class Outer {
            public:
                class Inner { int z; };
                struct InnerS { int w; };
                union InnerU { int i; float f; };
                enum class InnerE { A, B };
                class L1 { public: class L2 { int q; }; };
            };
            namespace ns {
                class NsOuter { public: class NsInner { int n; }; };
            }
        ";
        let staging = build_cpp(source);

        // Each nested type emits a node with the `Outer::Inner` qualified shape.
        assert_has_node_with_kind_exact(&staging, "Outer::Inner", NodeKind::Class);
        assert_has_node_with_kind_exact(&staging, "Outer::InnerS", NodeKind::Struct);
        // Unions map to NodeKind::Struct (no dedicated Union variant).
        assert_has_node_with_kind_exact(&staging, "Outer::InnerU", NodeKind::Struct);
        assert_has_node_with_kind_exact(&staging, "Outer::InnerE", NodeKind::Enum);
        // Doubly nested: `Outer::L1` and `Outer::L1::L2`.
        assert_has_node_with_kind_exact(&staging, "Outer::L1", NodeKind::Class);
        assert_has_node_with_kind_exact(&staging, "Outer::L1::L2", NodeKind::Class);
        // Nested inside a namespaced class.
        assert_has_node_with_kind_exact(&staging, "ns::NsOuter", NodeKind::Class);
        assert_has_node_with_kind_exact(&staging, "ns::NsOuter::NsInner", NodeKind::Class);

        // Members still qualify under the nested chain (regression guard: the
        // member-walk behaviour that already worked must be preserved).
        assert_has_node_with_kind_exact(&staging, "Outer::Inner.z", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Outer::L1::L2.q", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "ns::NsOuter::NsInner.n", NodeKind::Property);
    }

    #[test]
    fn test_nested_enum_carries_enclosing_visibility() {
        // Nested enums must carry the enclosing access visibility, identical to
        // the nested class/struct path — not an absent visibility. A nested enum
        // under `private:` is `private`; under `public:` is `public`.
        let source = r"
            class Outer {
            private:
                enum class Secret { A, B };
            public:
                enum class Pub { X, Y };
            };
        ";
        let staging = build_cpp(source);

        let secret = cpp_find_added_node(&staging, "Outer::Secret")
            .expect("nested enum Outer::Secret must be staged");
        assert_eq!(secret.kind, NodeKind::Enum, "Secret must be an Enum node");
        let secret_vis = staging.resolve_local_string(
            secret
                .visibility
                .expect("nested enum must carry a visibility id"),
        );
        assert_eq!(
            secret_vis,
            Some("private"),
            "nested enum under `private:` must be private"
        );

        let pub_enum = cpp_find_added_node(&staging, "Outer::Pub")
            .expect("nested enum Outer::Pub must be staged");
        let pub_vis = staging.resolve_local_string(
            pub_enum
                .visibility
                .expect("nested enum must carry a visibility id"),
        );
        assert_eq!(
            pub_vis,
            Some("public"),
            "nested enum under `public:` must be public"
        );
    }

    #[test]
    fn test_nested_class_emits_inheritance_edge() {
        // Regression: a nested class with a base clause must emit an `Inherits`
        // edge anchored on the nested class node (previously the nested type was
        // never registered, so its lineage edge was lost entirely).
        let source = r"
            struct Base { virtual ~Base(); };
            class Outer {
            public:
                class Derived : public Base {};
            };
        ";
        let staging = build_cpp(source);

        let derived_id = cpp_find_added_node_id(&staging, "Outer::Derived", NodeKind::Class)
            .expect("nested Derived class node must be staged");

        let has_inherits = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    source: src,
                    kind: EdgeKind::Inherits,
                    ..
                } if *src == derived_id
            )
        });
        assert!(
            has_inherits,
            "nested Derived must emit an Inherits edge to its base"
        );
    }

    #[test]
    fn test_top_level_union_emits_struct_node() {
        // Regression: top-level `union` declarations previously produced no node
        // (only `class_specifier` / `struct_specifier` were matched). Unions map
        // to NodeKind::Struct.
        let source = "union Value { int i; float f; };";
        let staging = build_cpp(source);
        assert_has_node_with_kind_exact(&staging, "Value", NodeKind::Struct);
    }

    #[test]
    fn test_extract_function() {
        let source = r#"
            #include <cstdio>
            void hello() {
                std::printf("Hello");
            }
        "#;
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node_with_kind(&staging, "hello", NodeKind::Function);
    }

    #[test]
    fn test_extract_virtual_function() {
        let source = r"
            class Service {
            public:
                virtual void fetchData() {}
            };
        ";
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node(&staging, "fetchData");
    }

    #[test]
    fn test_extract_call_edge() {
        let source = r"
            void greet() {}

            int main() {
                greet();
                return 0;
            }
        ";
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node(&staging, "main");
        assert_has_node(&staging, "greet");
        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty());
    }

    #[test]
    fn test_extract_member_call_edge() {
        let source = r"
            class Service {
            public:
                void helper() {}
            };

            int main() {
                Service svc;
                svc.helper();
                return 0;
            }
        ";
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("member.cpp"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_has_node(&staging, "main");
        assert_has_node(&staging, "helper");
        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty());
    }

    #[test]
    fn test_extract_namespace_map_simple() {
        let source = r"
            namespace demo {
                void func() {}
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);

        // Should have one entry mapping the namespace body to "demo::"
        assert_eq!(namespace_map.len(), 1);

        // Find any namespace entry (we only have one)
        let (_, ns_prefix) = namespace_map.iter().next().unwrap();
        assert_eq!(ns_prefix, "demo::");
    }

    #[test]
    fn test_extract_namespace_map_nested() {
        let source = r"
            namespace outer {
                namespace inner {
                    void func() {}
                }
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);

        // Should have entries for both outer and inner namespaces
        assert!(namespace_map.len() >= 2);

        // Check that we have the expected namespace prefixes
        let ns_values: Vec<&String> = namespace_map.values().collect();
        assert!(ns_values.iter().any(|v| v.as_str() == "outer::"));
        assert!(ns_values.iter().any(|v| v.as_str() == "outer::inner::"));
    }

    #[test]
    fn test_extract_namespace_map_multiple() {
        let source = r"
            namespace first {
                void func1() {}
            }
            namespace second {
                void func2() {}
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);

        // Should have entries for both namespaces
        assert_eq!(namespace_map.len(), 2);

        let ns_values: Vec<&String> = namespace_map.values().collect();
        assert!(ns_values.iter().any(|v| v.as_str() == "first::"));
        assert!(ns_values.iter().any(|v| v.as_str() == "second::"));
    }

    #[test]
    fn test_find_namespace_for_offset() {
        let source = r"
            namespace demo {
                void func() {}
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);

        // Find the byte offset of "func" (should be inside demo namespace)
        let func_offset = source.find("func").unwrap();
        let ns = find_namespace_for_offset(func_offset, &namespace_map);
        assert_eq!(ns, "demo::");

        // Byte offset before namespace should return empty string
        let ns = find_namespace_for_offset(0, &namespace_map);
        assert_eq!(ns, "");
    }

    #[test]
    fn test_extract_cpp_contexts_free_function() {
        let source = r"
            void helper() {}
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "helper");
        assert!(!contexts[0].is_static);
        assert!(!contexts[0].is_virtual);
    }

    #[test]
    fn test_extract_cpp_contexts_namespace_function() {
        let source = r"
            namespace demo {
                void helper() {}
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "demo::helper");
        assert_eq!(contexts[0].namespace_stack, vec!["demo"]);
    }

    #[test]
    fn test_extract_cpp_contexts_class_method() {
        let source = r"
            class Service {
            public:
                void process() {}
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "Service::process");
        assert_eq!(contexts[0].class_stack, vec!["Service"]);
    }

    #[test]
    fn test_extract_cpp_contexts_namespace_and_class() {
        let source = r"
            namespace demo {
                class Service {
                public:
                    void process() {}
                };
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "demo::Service::process");
        assert_eq!(contexts[0].namespace_stack, vec!["demo"]);
        assert_eq!(contexts[0].class_stack, vec!["Service"]);
    }

    #[test]
    fn test_extract_cpp_contexts_static_method() {
        let source = r"
            class Repository {
            public:
                static void save() {}
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "Repository::save");
        assert!(contexts[0].is_static);
    }

    #[test]
    fn test_extract_cpp_contexts_virtual_method() {
        let source = r"
            class Base {
            public:
                virtual void render() {}
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "Base::render");
        assert!(contexts[0].is_virtual);
    }

    #[test]
    fn test_extract_cpp_contexts_inline_function() {
        let source = r"
            inline void helper() {}
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "helper");
        assert!(contexts[0].is_inline);
    }

    #[test]
    fn test_extract_cpp_contexts_out_of_line_definition() {
        let source = r"
            namespace demo {
                class Service {
                public:
                    int process(int v);
                };

                inline int Service::process(int v) {
                    return v;
                }
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let contexts = extract_cpp_contexts_for_test(&tree, source, &namespace_map);

        // Only the definition should be captured (not the declaration)
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].qualified_name, "demo::Service::process");
        assert!(contexts[0].is_inline);
    }

    #[test]
    fn test_extract_field_types_simple() {
        let source = r"
            class Service {
            public:
                Repository repo;
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should have one field: Service.repo -> Repository
        assert_eq!(field_types.len(), 1);
        assert_eq!(
            field_types.get(&("Service".to_string(), "repo".to_string())),
            Some(&"Repository".to_string())
        );
    }

    #[test]
    fn test_extract_field_types_namespace() {
        let source = r"
            namespace demo {
                class Service {
                public:
                    Repository repo;
                };
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should have one field with namespace-qualified class
        assert_eq!(field_types.len(), 1);
        assert_eq!(
            field_types.get(&("demo::Service".to_string(), "repo".to_string())),
            Some(&"Repository".to_string())
        );
    }

    #[test]
    fn test_extract_field_types_no_collision() {
        let source = r"
            class ServiceA {
            public:
                Repository repo;
            };

            class ServiceB {
            public:
                Repository repo;
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should have two distinct fields with no collision
        assert_eq!(field_types.len(), 2);
        assert_eq!(
            field_types.get(&("ServiceA".to_string(), "repo".to_string())),
            Some(&"Repository".to_string())
        );
        assert_eq!(
            field_types.get(&("ServiceB".to_string(), "repo".to_string())),
            Some(&"Repository".to_string())
        );
    }

    #[test]
    fn test_extract_using_declaration() {
        let source = r"
            using std::vector;

            class Service {
            public:
                vector data;
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Verify field extraction resolves type via using declaration
        assert_eq!(field_types.len(), 1);
        assert_eq!(
            field_types.get(&("Service".to_string(), "data".to_string())),
            Some(&"std::vector".to_string()),
            "Field type should resolve 'vector' to 'std::vector' via using declaration"
        );

        // Verify that using declaration populated type_map
        assert_eq!(
            type_map.get(&(String::new(), "vector".to_string())),
            Some(&"std::vector".to_string()),
            "Using declaration should map 'vector' to 'std::vector' in type_map"
        );
    }

    #[test]
    fn test_extract_field_types_pointer() {
        let source = r"
            class Service {
            public:
                Repository* repo;
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should extract field even for pointer types
        assert_eq!(field_types.len(), 1);
        assert_eq!(
            field_types.get(&("Service".to_string(), "repo".to_string())),
            Some(&"Repository".to_string())
        );
    }

    #[test]
    fn test_extract_field_types_multiple_declarators() {
        let source = r"
            class Service {
            public:
                Repository repo_a, repo_b, repo_c;
            };
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should extract all three fields
        assert_eq!(field_types.len(), 3);
        assert_eq!(
            field_types.get(&("Service".to_string(), "repo_a".to_string())),
            Some(&"Repository".to_string())
        );
        assert_eq!(
            field_types.get(&("Service".to_string(), "repo_b".to_string())),
            Some(&"Repository".to_string())
        );
        assert_eq!(
            field_types.get(&("Service".to_string(), "repo_c".to_string())),
            Some(&"Repository".to_string())
        );
    }

    #[test]
    fn test_extract_field_types_nested_struct_with_parent_field() {
        // Regression test for nested class FQN building
        // Verifies that Inner gets "demo::Outer::Inner" not "demo::Inner"
        let source = r"
            namespace demo {
                struct Outer {
                    int outer_field;
                    struct Inner {
                        int inner_field;
                    };
                    Inner nested_instance;
                };
            }
        ";
        let tree = parse_cpp(source);
        let namespace_map = extract_namespace_map_for_test(&tree, source);
        let (field_types, _type_map) =
            extract_field_and_type_info_for_test(&tree, source, &namespace_map);

        // Should have fields from both Outer and Inner with properly qualified class FQNs
        // The critical assertion: Inner's field must use "demo::Outer::Inner", not "demo::Inner"
        assert!(
            field_types.len() >= 2,
            "Expected at least outer_field and nested_instance"
        );

        // Outer's field
        assert_eq!(
            field_types.get(&("demo::Outer".to_string(), "outer_field".to_string())),
            Some(&"int".to_string())
        );

        // Outer's nested instance field. Its bare type `Inner` is scope-qualified
        // at the store site (issue #466, 02_DESIGN Section 3.2.2): the innermost
        // enclosing scope that names a declared class is `demo::Outer::Inner`, so
        // the stored value is the nested FQN, not the bare `Inner`. This is what
        // lets a member call through `nested_instance` unify with the real node.
        assert_eq!(
            field_types.get(&("demo::Outer".to_string(), "nested_instance".to_string())),
            Some(&"demo::Outer::Inner".to_string())
        );

        // If Inner's field is extracted, verify it uses the correct parent-qualified FQN
        if field_types.contains_key(&("demo::Outer::Inner".to_string(), "inner_field".to_string()))
        {
            // Great! The nested class field was extracted with correct FQN
            assert_eq!(
                field_types.get(&("demo::Outer::Inner".to_string(), "inner_field".to_string())),
                Some(&"int".to_string()),
                "Inner class fields must use parent-qualified FQN 'demo::Outer::Inner'"
            );
        }
    }

    // ========================================================================
    // C2_OTHER_CPP — Property/Constant emission for class/struct fields
    // REQ:R0001, R0002, R0003, R0004, R0005, R0020, R0023
    // ========================================================================
    //
    // These tests assert the post-fix shape of `process_field_declaration`:
    //   - field qualified names use `Class.field` (last separator migrated to `.`
    //     per design §3.1.1; class qualifier still uses `::`)
    //   - non-`const`/`constexpr` fields → NodeKind::Property
    //   - `const` and `constexpr` fields → NodeKind::Constant
    //   - `static` keyword → is_static = true
    //   - visibility from enclosing access specifier; default `"private"`
    //     for class, `"public"` for struct
    //   - TypeOf edge emits TypeOfContext::Field with the bare field name
    //   - legacy `Class::field` qualified-name lookup returns 0 hits

    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};

    /// Locate the staged `AddNode` entry by exact canonical (semantic) name.
    fn cpp_find_added_node<'a>(
        staging: &'a StagingGraph,
        canonical_name: &str,
    ) -> Option<&'a sqry_core::graph::unified::storage::arena::NodeEntry> {
        staging.operations().iter().find_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && staging.resolve_node_canonical_name(entry) == Some(canonical_name)
            {
                Some(entry)
            } else {
                None
            }
        })
    }

    /// Locate the staged `AddNode` `NodeId` for a node by exact canonical name + kind.
    fn cpp_find_added_node_id(
        staging: &StagingGraph,
        canonical_name: &str,
        kind: NodeKind,
    ) -> Option<sqry_core::graph::unified::NodeId> {
        staging.operations().iter().find_map(|op| match op {
            StagingOp::AddNode {
                entry,
                expected_id: Some(id),
            } if entry.kind == kind
                && staging.resolve_node_canonical_name(entry) == Some(canonical_name) =>
            {
                Some(*id)
            }
            _ => None,
        })
    }

    /// Build the unified graph for a C++ source snippet and return the staged graph.
    fn build_cpp(source: &str) -> StagingGraph {
        let tree = parse_cpp(source);
        let mut staging = StagingGraph::new();
        let builder = CppGraphBuilder::new();
        builder
            .build_graph(
                &tree,
                source.as_bytes(),
                Path::new("test.cpp"),
                &mut staging,
            )
            .expect("build_graph must succeed for the test fixture");
        staging
    }

    fn staged_node_name_by_id(
        staging: &StagingGraph,
        id: sqry_core::graph::unified::NodeId,
    ) -> Option<&str> {
        staging.nodes().find_map(|node| {
            if node.expected_id == Some(id) {
                staging.resolve_node_canonical_name(node.entry)
            } else {
                None
            }
        })
    }

    fn call_edge_pairs(staging: &StagingGraph) -> Vec<(String, String)> {
        staging
            .edges()
            .filter_map(|edge| {
                if matches!(edge.kind, EdgeKind::Calls { .. }) {
                    let source = staged_node_name_by_id(staging, edge.source)?.to_string();
                    let target = staged_node_name_by_id(staging, edge.target)?.to_string();
                    Some((source, target))
                } else {
                    None
                }
            })
            .collect()
    }

    fn assert_has_call_edge(staging: &StagingGraph, caller: &str, callee: &str) {
        let calls = call_edge_pairs(staging);
        assert!(
            calls
                .iter()
                .any(|(source, target)| source == caller && target == callee),
            "expected Calls edge {caller} -> {callee}; staged Calls edges: {calls:?}"
        );
    }

    fn assert_no_call_target(staging: &StagingGraph, forbidden_target: &str) {
        let calls = call_edge_pairs(staging);
        assert!(
            !calls.iter().any(|(_, target)| target == forbidden_target),
            "unexpected Calls edge target {forbidden_target}; staged Calls edges: {calls:?}"
        );
    }

    fn assert_no_call_target_suffix(staging: &StagingGraph, forbidden_suffix: &str) {
        let calls = call_edge_pairs(staging);
        assert!(
            !calls
                .iter()
                .any(|(_, target)| target.ends_with(forbidden_suffix)),
            "unexpected Calls edge target ending with {forbidden_suffix}; staged Calls edges: {calls:?}"
        );
    }

    #[test]
    fn test_issue_466_t1_member_call_through_field_resolves_to_method_fqn() {
        let source = r"
namespace demo {
    struct Repository { void save(); };
    struct Service { Repository repo; void run() { repo.save(); } };
}
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "demo::Service::run", "demo::Repository::save");
        assert_no_call_target(&staging, "demo::repo.save");
        assert_no_call_target(&staging, "Repository::save");
    }

    #[test]
    fn test_issue_466_t2_same_namespace_static_call_uses_fallback_prefix() {
        let source = r"
namespace demo {
    struct Repository { static void save(); };
    void use() { Repository::save(); }
}
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "demo::use", "demo::Repository::save");
    }

    #[test]
    fn test_issue_466_t3_using_declaration_alias_resolves_static_call() {
        let source = r"
namespace lib { struct Widget { static void make(); }; }
namespace app { using lib::Widget; void run() { Widget::make(); } }
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "app::run", "lib::Widget::make");
        assert_no_call_target(&staging, "app::Widget::make");
    }

    #[test]
    fn test_issue_466_t4_unknown_receiver_does_not_invent_member_target() {
        let source = r"
namespace demo {
    struct Service { void run(int* p) { p->frobnicate(); } };
}
";
        let staging = build_cpp(source);

        assert_no_call_target_suffix(&staging, "::frobnicate");
        assert_has_call_edge(&staging, "demo::Service::run", "demo::p->frobnicate");
    }

    #[test]
    fn test_issue_466_t5_qualified_and_ffi_fallback_behavior_is_unchanged() {
        let source = r#"
extern "C" { int printf(const char*); }
namespace demo { void helper() {} }
void run() {
    demo::helper();
    printf("x");
}
"#;
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "run", "demo::helper");
        assert_has_ffi_call_edge(&staging, "run", "extern::C::printf");
    }

    #[test]
    fn test_issue_466_t6_same_class_name_collision_does_not_cross_namespace() {
        let source = r"
namespace a { struct Repository { void save(); }; }
namespace b {
    struct Repository { void wipe(); };
    struct Service { Repository repo; void run() { repo.save(); } };
}
";
        let staging = build_cpp(source);

        assert_no_call_target(&staging, "a::Repository::save");
        assert_has_call_edge(&staging, "b::Service::run", "b::Repository::save");
    }

    #[test]
    fn test_issue_466_t7_nested_class_member_access_resolves() {
        let source = r"
namespace demo {
    struct Inner { void tick(); };
    struct Outer { struct Nested { Inner inner; void go() { inner.tick(); } }; };
}
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "demo::Outer::Nested::go", "demo::Inner::tick");
    }

    #[test]
    fn test_issue_466_t8_out_of_class_method_definition_resolves_member_field() {
        let source = r"
namespace demo {
    struct Repository { void save(); };
    struct Service { Repository repo; void run(); };
    void Service::run() { repo.save(); }
}
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "demo::Service::run", "demo::Repository::save");
    }

    #[test]
    fn test_issue_466_t9_same_named_fields_bind_to_enclosing_class() {
        let source = r"
namespace demo {
    struct Base { struct Handle { void base_op(); }; Handle h; };
    struct Repository { void save(); };
    struct Service { Repository h; void run() { h.save(); } };
}
";
        let staging = build_cpp(source);

        assert_has_call_edge(&staging, "demo::Service::run", "demo::Repository::save");
        assert_no_call_target(&staging, "demo::Base::Handle::base_op");
    }

    /// AC-1 + AC-2 + AC-4 (struct default visibility) + AC-5:
    /// instance struct fields emit Property nodes with `Class.field`
    /// qualified-name shape, `is_static = false`, visibility = `"public"`
    /// (struct default), and a `TypeOf` edge using `TypeOfContext::Field` +
    /// the bare field name.
    #[test]
    fn test_struct_field_emits_property_with_field_context() {
        let source = "struct Point { int x; int y; };";
        let staging = build_cpp(source);

        // AC-1: dotted Class.field qualified name.
        assert_has_node_with_kind_exact(&staging, "Point.x", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Point.y", NodeKind::Property);

        let entry =
            cpp_find_added_node(&staging, "Point.x").expect("Point.x should be staged as a node");
        assert_eq!(entry.kind, NodeKind::Property, "x must be Property");
        assert!(!entry.is_static, "instance field is_static must be false");
        let vis = staging.resolve_local_string(entry.visibility.expect("visibility id"));
        assert_eq!(
            vis,
            Some("public"),
            "struct default visibility must be 'public'"
        );
        // `span_from_node` packs row/column into `Span::Position`; the helper
        // then stores them into `start_line`/`start_column`/`end_line`/
        // `end_column` on the entry (start_byte/end_byte are intentionally
        // not populated by `add_node_internal`). Assert the packed
        // line/column range is non-empty so we catch zero-width spans.
        assert!(entry.end_line > 0, "field end_line must be set (got 0)");
        assert!(
            entry.end_line > entry.start_line
                || (entry.end_line == entry.start_line && entry.end_column > entry.start_column),
            "field span must be non-empty: [{}:{}..{}:{}]",
            entry.start_line,
            entry.start_column,
            entry.end_line,
            entry.end_column,
        );

        // AC-5: TypeOf edge with Field context + bare name "x".
        let x_id = cpp_find_added_node_id(&staging, "Point.x", NodeKind::Property)
            .expect("Point.x Property NodeId");
        let edge = staging.operations().iter().find_map(|op| {
            if let StagingOp::AddEdge {
                source: src,
                kind: EdgeKind::TypeOf { context, name, .. },
                ..
            } = op
                && *src == x_id
            {
                Some((*context, *name))
            } else {
                None
            }
        });
        let (ctx, name) = edge.expect("TypeOf edge from Point.x should be staged");
        assert_eq!(
            ctx,
            Some(TypeOfContext::Field),
            "TypeOf edge context must be Field"
        );
        let resolved_name = name.and_then(|sid| staging.resolve_local_string(sid));
        assert_eq!(
            resolved_name,
            Some("x"),
            "TypeOf edge name must be the bare field name 'x'"
        );

        // AC-1 (negative): old NodeKind::Variable for these names must NOT appear.
        let stale_variable = staging.nodes().any(|n| {
            n.entry.kind == NodeKind::Variable
                && matches!(
                    staging.resolve_node_name(n.entry),
                    Some("Point.x" | "Point.y" | "Point::x" | "Point::y")
                )
        });
        assert!(
            !stale_variable,
            "Point fields must not be emitted as NodeKind::Variable"
        );
    }

    /// AC-4: class default visibility is `"private"`.
    #[test]
    fn test_class_field_default_visibility_is_private() {
        let source = "class Foo { int hidden; };";
        let staging = build_cpp(source);

        let entry = cpp_find_added_node(&staging, "Foo.hidden")
            .expect("Foo.hidden should be staged as a node");
        assert_eq!(entry.kind, NodeKind::Property);
        let vis = staging.resolve_local_string(entry.visibility.expect("visibility id"));
        assert_eq!(
            vis,
            Some("private"),
            "class default visibility must be 'private'"
        );
    }

    /// AC-4: explicit access specifier overrides the default.
    #[test]
    fn test_class_field_respects_explicit_access_specifier() {
        let source = "class Foo { public: int public_field; protected: int prot_field; };";
        let staging = build_cpp(source);

        let pub_entry = cpp_find_added_node(&staging, "Foo.public_field")
            .expect("Foo.public_field should be staged");
        assert_eq!(
            staging.resolve_local_string(pub_entry.visibility.expect("vis")),
            Some("public")
        );

        let prot_entry = cpp_find_added_node(&staging, "Foo.prot_field")
            .expect("Foo.prot_field should be staged");
        assert_eq!(
            staging.resolve_local_string(prot_entry.visibility.expect("vis")),
            Some("protected")
        );
    }

    /// AC-2 + AC-3: `const` field → Constant; instance const has
    /// `is_static = false` (no `static` keyword present).
    #[test]
    fn test_const_field_emits_constant() {
        let source = "class Foo { const int kMax = 0; };";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "Foo.kMax", NodeKind::Constant);
        let entry = cpp_find_added_node(&staging, "Foo.kMax").expect("Foo.kMax");
        assert_eq!(entry.kind, NodeKind::Constant);
        assert!(
            !entry.is_static,
            "const (non-static) field is_static must be false; only `static` keyword sets is_static"
        );
    }

    /// AC-2 + AC-3: `constexpr` field → Constant. The `static` flag is
    /// driven strictly by the `static` keyword (per design §3.4); a bare
    /// `constexpr` member without `static` must keep `is_static = false`.
    #[test]
    fn test_constexpr_field_emits_constant() {
        let source = "class Foo { constexpr static int kAnswer = 42; };";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "Foo.kAnswer", NodeKind::Constant);
        let entry = cpp_find_added_node(&staging, "Foo.kAnswer").expect("Foo.kAnswer");
        assert_eq!(entry.kind, NodeKind::Constant);
        assert!(
            entry.is_static,
            "static constexpr member must have is_static = true"
        );
    }

    /// AC-3: `static` keyword sets `is_static = true` on a Property
    /// (non-const non-constexpr).
    #[test]
    fn test_static_field_sets_is_static_true() {
        let source = "class Foo { static int counter; };";
        let staging = build_cpp(source);

        let entry = cpp_find_added_node(&staging, "Foo.counter").expect("Foo.counter");
        assert_eq!(entry.kind, NodeKind::Property);
        assert!(entry.is_static, "static keyword must set is_static = true");
    }

    /// AC-6: bit-fields (e.g., `int flags : 4;`) emit Property nodes with the
    /// usual `Class.field` form.
    #[test]
    fn test_bitfield_emits_property() {
        let source = "struct Flags { unsigned int low : 4; unsigned int high : 4; };";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "Flags.low", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Flags.high", NodeKind::Property);
    }

    /// AC-6: anonymous union — true anonymous unions (no instance name) inject
    /// their members into the enclosing class per C++ semantics. Members must
    /// emit as Property nodes under the OUTER class qualifier
    /// (`Variant.as_int`, `Variant.as_float`), NOT under any synthetic inner
    /// qualifier — there is no name to qualify by.
    #[test]
    fn test_anonymous_union_member_fields_emit_property() {
        let source = r"
class Variant {
public:
    int tag;
    union {
        int as_int;
        float as_float;
    };
};
";
        let staging = build_cpp(source);

        // Outer named field is present with the dotted form.
        assert_has_node_with_kind_exact(&staging, "Variant.tag", NodeKind::Property);

        // Anonymous-union members are injected into the enclosing class and
        // appear under `Variant.<member>` per C++ semantics (design AC-6).
        assert_has_node_with_kind_exact(&staging, "Variant.as_int", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Variant.as_float", NodeKind::Property);

        // Visibility for injected members inherits the OUTER access state
        // (`public:` here).
        let as_int = cpp_find_added_node(&staging, "Variant.as_int")
            .expect("Variant.as_int should be staged");
        let vis = staging.resolve_local_string(as_int.visibility.expect("visibility id"));
        assert_eq!(
            vis,
            Some("public"),
            "anonymous-union members must inherit OUTER access (`public:` here)"
        );

        // Negative: there must be no synthetic anonymous-union qualifier
        // such as `Variant::.as_int` or members under a bogus inner name.
        let bogus = staging.nodes().any(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("::.") || name.starts_with("Variant::."))
        });
        assert!(
            !bogus,
            "anonymous union must not produce a synthetic qualifier"
        );

        // No stale Variable emission for any of these names.
        let stale_variable = staging.nodes().any(|n| {
            n.entry.kind == NodeKind::Variable
                && matches!(
                    staging.resolve_node_name(n.entry),
                    Some("Variant.tag" | "Variant.as_int" | "Variant.as_float")
                )
        });
        assert!(
            !stale_variable,
            "anonymous-union members + outer fields must not stay as Variable"
        );
    }

    /// AC-6: templated class — `template<class T> struct Box { T value; };`
    /// emits the field under the bare class name (template-args part is
    /// stripped for the qualified name; design §4.1 edge cases).
    #[test]
    fn test_templated_class_field_emits_property() {
        let source = r"
template<class T>
struct Box {
    T value;
};
";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "Box.value", NodeKind::Property);
        let entry = cpp_find_added_node(&staging, "Box.value").expect("Box.value");
        assert_eq!(entry.kind, NodeKind::Property);
        assert!(!entry.is_static);
    }

    /// AC-6: nested class — both the OUTER field (`Outer.outer_value`) and the
    /// INNER nested-class fields (`Outer::Inner.x`) must emit as Property
    /// nodes. `walk_class_body` recurses into a nested
    /// `field_declaration > class_specifier` and extends the qualifier chain
    /// with the inner-class name (design AC-6 + §4.1).
    #[test]
    fn test_outer_class_field_with_nested_class_present() {
        let source = r"
class Outer {
public:
    int outer_value;
    class Inner {
    public:
        int x;
    };
};
";
        let staging = build_cpp(source);

        // AC-6: outer field is emitted under the dotted form.
        assert_has_node_with_kind_exact(&staging, "Outer.outer_value", NodeKind::Property);

        // AC-6: nested-class field emits under the parent-qualified dotted
        // form `Outer::Inner.x` (class chain stays `::`, last separator
        // migrates to `.` per design §3.1.1).
        assert_has_node_with_kind_exact(&staging, "Outer::Inner.x", NodeKind::Property);

        // Negative legacy lookup: the legacy `Outer::outer_value` form must
        // not appear (AC-7 + design §3.1.1).
        let legacy_hits: Vec<_> = staging
            .nodes()
            .filter(|n| staging.resolve_node_name(n.entry) == Some("Outer::outer_value"))
            .collect();
        assert!(
            legacy_hits.is_empty(),
            "legacy `Outer::outer_value` lookup must return 0 hits"
        );

        // Negative: nested field must not appear under bare `Inner.x` (lost
        // outer chain) or legacy `Outer::Inner::x` (last separator missed
        // migration).
        for legacy in ["Inner.x", "Outer::Inner::x", "Outer.Inner.x"] {
            let hits: Vec<_> = staging
                .nodes()
                .filter(|n| staging.resolve_node_name(n.entry) == Some(legacy))
                .collect();
            assert!(
                hits.is_empty(),
                "nested-class field `{legacy}` must not appear; expected only `Outer::Inner.x`"
            );
        }
    }

    /// AC-6: nested struct inside a class — nested struct fields qualify as
    /// `Outer::Inner.y`. Default struct visibility is `public`, regardless
    /// of the OUTER access state.
    #[test]
    fn test_outer_class_with_nested_struct_emits_inner_field() {
        let source = r"
class Outer {
private:
    struct Inner {
        int y;
    };
};
";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "Outer::Inner.y", NodeKind::Property);

        let entry = cpp_find_added_node(&staging, "Outer::Inner.y")
            .expect("Outer::Inner.y should be staged");
        let vis = staging.resolve_local_string(entry.visibility.expect("visibility id"));
        assert_eq!(
            vis,
            Some("public"),
            "nested struct field default visibility must be 'public' \
             regardless of OUTER access state"
        );
    }

    /// Staging-level smoke: post-fix, no staged node for a class field uses
    /// the legacy `Class::field` qualified-name shape. This is a
    /// fast-feedback companion to the AC-7 contract test — the authoritative
    /// AC-7 assertion runs against a finalized `GraphSnapshot` via
    /// `find_nodes_by_name` in
    /// `tests/integration_tests.rs::test_legacy_double_colon_field_lookup_returns_zero_via_snapshot`
    /// (design §4.1).
    #[test]
    fn test_legacy_double_colon_field_lookup_returns_zero() {
        let source = r"
class Foo {
public:
    int bar;
    static int baz;
    const int qux = 0;
};
struct Quux {
    int corge;
};
";
        let staging = build_cpp(source);

        // Positive: dotted form must be present for every field.
        assert_has_node_with_kind_exact(&staging, "Foo.bar", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Foo.baz", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Foo.qux", NodeKind::Constant);
        assert_has_node_with_kind_exact(&staging, "Quux.corge", NodeKind::Property);

        // Negative: legacy `Class::field` qualified name must not appear for
        // any of the fields in the fixture.
        for legacy in ["Foo::bar", "Foo::baz", "Foo::qux", "Quux::corge"] {
            let hits: Vec<_> = staging
                .nodes()
                .filter(|n| staging.resolve_node_name(n.entry) == Some(legacy))
                .collect();
            assert!(
                hits.is_empty(),
                "legacy lookup for {legacy:?} must return 0 hits, got {} node(s) ({:?})",
                hits.len(),
                hits.iter()
                    .map(|n| (n.entry.kind, staging.resolve_node_name(n.entry)))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Field inside a class that lives in a namespace must keep the namespace
    /// chain joined by `::` and only flip the LAST separator to `.`.
    #[test]
    fn test_namespaced_class_field_qualified_name() {
        let source = r"
namespace demo {
    class Service {
    public:
        int counter;
    };
}
";
        let staging = build_cpp(source);

        assert_has_node_with_kind_exact(&staging, "demo::Service.counter", NodeKind::Property);
    }
}

#[cfg(test)]
mod shape_tests {
    use super::{cf_bucket_for_cpp_kind, cpp_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/reference/sample.cpp"
    ));

    fn parse(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let mut p = tree_sitter::Parser::new();
        p.set_language(&lang).expect("load cpp grammar");
        p.parse(src, None).expect("parse")
    }

    /// Resolve the function_definition whose declarator names the given function.
    fn function_named<'t>(tree: &'t tree_sitter::Tree, name: &str) -> tree_sitter::Node<'t> {
        let root = tree.root_node();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_definition"
                && function_def_name(node).as_deref() == Some(name)
            {
                return node;
            }
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no function_definition named {name}");
    }

    /// Pull the declared identifier out of a C++ function_definition declarator.
    fn function_def_name(node: tree_sitter::Node) -> Option<String> {
        let mut decl = node.child_by_field_name("declarator")?;
        for _ in 0..8 {
            if decl.kind() == "function_declarator" {
                let inner = decl.child_by_field_name("declarator")?;
                return inner.utf8_text(SAMPLE.as_bytes()).ok().map(str::to_owned);
            }
            decl = decl.child_by_field_name("declarator")?;
        }
        None
    }

    #[test]
    fn cf_table_is_non_empty() {
        let mapping = cpp_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let mut covered = 0;
        for id in 0..lang.node_kind_count() {
            if mapping.cf_bucket(id as u16).is_some() {
                covered += 1;
            }
        }
        assert!(
            covered >= 10,
            "expected many C++ CF kinds mapped, got {covered}"
        );
    }

    #[test]
    fn histogram_covers_real_control_flow() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            cpp_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(!d.is_unhashable());
        for bucket in [
            CfBucket::Branch,
            CfBucket::Loop,
            CfBucket::Match,
            CfBucket::Try,
            CfBucket::Catch,
            CfBucket::Throw,
            CfBucket::Return,
            CfBucket::BreakContinue,
            CfBucket::Call,
            CfBucket::Assign,
        ] {
            assert!(
                d.cf_histogram[bucket.index()] >= 1,
                "classify must exercise {bucket:?}"
            );
        }
    }

    #[test]
    fn lambda_body_covers_closure() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "adder");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            cpp_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(
            d.cf_histogram[CfBucket::Closure.index()] >= 1,
            "lambda closure"
        );
    }

    #[test]
    fn signature_shape_reads_arity_defaults_return() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let mapping = cpp_shape_mapping();
        let shape = mapping.signature_shape(func, SAMPLE.as_bytes());
        // int classify(const std::vector<int> &values, int threshold = 0)
        assert_eq!(shape.arity_positional, 2);
        assert!(shape.has_defaults, "threshold = 0");
        assert!(shape.has_return_annotation, "int return type");
    }

    /// AC-6 anchor: structurally equivalent classify() in C++ and Python share a
    /// comparable cf histogram shape under the one bucket schema.
    #[test]
    fn ac6_cpp_classify_histogram_well_formed() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            cpp_shape_mapping(),
            &ShapeBudget::default(),
        );
        // The branch/loop/return/call core is the cross-language comparison axis.
        assert_eq!(d.cf_histogram[CfBucket::Branch.index()], 2, "two if levels");
        assert!(d.cf_histogram[CfBucket::Loop.index()] >= 2, "for + while");
        assert!(d.cf_histogram[CfBucket::Return.index()] >= 2, "two returns");
    }

    #[test]
    fn unknown_kind_maps_to_none() {
        assert!(cf_bucket_for_cpp_kind("translation_unit").is_none());
        assert!(cf_bucket_for_cpp_kind("identifier").is_none());
    }
}
