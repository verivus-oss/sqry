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

use sqry_core::graph::unified::{FfiConvention, GraphBuildHelper, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
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

// Helper extension trait for Span creation
#[allow(dead_code)] // Reserved for future span-based analysis
trait SpanExt {
    fn from_node(node: &tree_sitter::Node) -> Self;
}

impl SpanExt for Span {
    fn from_node(node: &tree_sitter::Node) -> Self {
        Span::new(
            Position::new(node.start_position().row, node.start_position().column),
            Position::new(node.end_position().row, node.end_position().column),
        )
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
    /// Used to resolve method calls on member variables (e.g., repo.save -> `demo::Repository::save`)
    /// Reserved for future call resolution enhancements
    #[allow(dead_code)]
    field_types: QualifiedNameMap,

    /// Maps (`namespace_context`, `simple_type_name`) to FQN.
    /// Example: ("demo", "Repository") -> "`demo::Repository`"
    /// This handles the fact that the same simple type name can resolve differently
    /// in different namespaces and using-directive scopes.
    /// Used to resolve static method calls (e.g., `Repository::save` -> `demo::Repository::save`)
    /// Reserved for future call resolution enhancements
    #[allow(dead_code)]
    type_map: QualifiedNameMap,

    /// Maps byte ranges to namespace prefixes (e.g., range -> "`demo::`")
    /// Used to determine which namespace context a symbol is defined in
    /// Reserved for future namespace-aware resolution
    #[allow(dead_code)]
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
    /// Whether this is a virtual method
    /// Reserved for future polymorphic call analysis
    #[allow(dead_code)]
    is_virtual: bool,
    /// Whether this is inline
    /// Reserved for future optimization hints
    #[allow(dead_code)]
    is_inline: bool,
    /// Namespace stack for use in call resolution (e.g., [`demo`])
    namespace_stack: Vec<String>,
    /// Class stack for use in call resolution (e.g., [`Service`])
    /// Reserved for future method resolution enhancements
    #[allow(dead_code)] // Used in tests and reserved for future call resolution
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

    extract_fields_recursive(
        node,
        content,
        namespace_map,
        &mut field_types,
        &mut type_map,
        &mut class_stack,
        budget,
    )?;

    Ok((field_types, type_map))
}

/// Recursive helper for field and type extraction
fn extract_fields_recursive(
    node: Node,
    content: &[u8],
    namespace_map: &HashMap<std::ops::Range<usize>, String>,
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
                let class_fqn = if class_stack.is_empty() {
                    // Top-level class: just namespace + class name
                    if namespace.is_empty() {
                        class_name.clone()
                    } else {
                        format!("{}::{}", namespace.trim_end_matches("::"), class_name)
                    }
                } else {
                    // Nested class: parent_fqn + class name
                    format!("{}::{}", class_stack.last().unwrap(), class_name)
                };

                class_stack.push(class_fqn.clone());

                // Process all children to find field_declaration_list or direct field_declaration
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_fields_recursive(
                        child,
                        content,
                        namespace_map,
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
                    field_types,
                    type_map,
                );
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

    // Resolve field type to FQN using namespace/type_map
    if let Some(ftype) = field_type {
        let namespace = find_namespace_for_offset(node.start_byte(), namespace_map);
        let field_type_fqn = resolve_type_to_fqn(&ftype, &namespace, type_map);

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
/// This function handles:
/// - Simple names: "helper" -> "`demo::helper`" (using namespace context)
/// - Qualified names: "`Service::process`" -> "`demo::Service::process`" (adding namespace)
/// - Static method calls: "`Repository::save`" -> "`demo::Repository::save`"
/// - Member calls would require parsing the AST node further (future work)
fn resolve_callee_name(
    callee_name: &str,
    caller_ctx: &FunctionContext,
    _ast_graph: &ASTGraph,
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
    // Future work: Parse the call expression to determine if it's a member call

    // Add function name
    parts.push(callee_name.to_string());

    parts.join("::")
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

/// Process a field declaration inside a class/struct, creating Variable node with `TypeOf` and Reference edges.
#[allow(clippy::unnecessary_wraps)]
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
                // Handle type qualifiers (const, volatile) - may need to combine with following type
                if field_type_text.is_none()
                    && let Ok(text) = child.utf8_text(content)
                {
                    field_type_text = Some(text.to_string());
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

        for field_name in field_names {
            let field_qualified = format!("{class_qualified_name}::{field_name}");
            let span = span_from_node(node);

            // Create variable node with visibility
            let var_id = helper.add_node_with_visibility(
                &field_qualified,
                Some(span),
                sqry_core::graph::unified::node::NodeKind::Variable,
                Some(visibility),
            );

            // Create a Type node for the base type (if not primitive)
            let type_id = helper.add_type(&base_type, None);

            // Add TypeOf edge: variable -> type
            helper.add_typeof_edge(var_id, type_id);

            // Add Reference edge: variable -> type
            helper.add_reference_edge(var_id, type_id);
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

/// Walk a class/struct body, processing field declarations and methods with visibility tracking.
#[allow(clippy::too_many_arguments)]
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

    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
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
                // Process field with current visibility
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
        "class_specifier" | "struct_specifier" => {
            // Extract class/struct name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let class_name = class_name.trim();
                let span = span_from_node(node);
                let is_struct = node.kind() == "struct_specifier";

                // Build qualified class name
                let qualified_class =
                    build_qualified_name(namespace_stack, class_stack, class_name);

                // Add class/struct node with qualified name
                let visibility = "public";
                let class_id = if is_struct {
                    helper.add_struct_with_visibility(
                        &qualified_class,
                        Some(span),
                        Some(visibility),
                    )
                } else {
                    helper.add_class_with_visibility(&qualified_class, Some(span), Some(visibility))
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
                // Nested classes have internal linkage unless explicitly exported
                if class_stack.is_empty() {
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
                let enum_id = helper.add_enum(&qualified_enum, Some(span));

                if class_stack.is_empty() {
                    let module_id = helper.add_module(FILE_MODULE_NAME, None);
                    helper.add_export_edge(module_id, enum_id);
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
                let caller_function_id = helper.ensure_function(&caller_qname, None, false, false);
                let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);

                // Check if the callee is a known FFI function
                // Only do FFI lookup for unqualified calls (no ::)
                let is_unqualified = !callee_qname.contains("::");
                if is_unqualified {
                    if let Some((ffi_qualified, ffi_convention)) = ffi_registry.get(&callee_qname) {
                        // This is a call to an FFI function - create FfiCall edge
                        let ffi_target_id =
                            helper.ensure_function(ffi_qualified, None, false, true);
                        helper.add_ffi_edge(caller_function_id, ffi_target_id, *ffi_convention);
                    } else {
                        // Regular call - create normal Call edge
                        let target_function_id =
                            helper.ensure_function(&callee_qname, None, false, false);
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
                        helper.ensure_function(&callee_qname, None, false, false);
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
        resolve_callee_name(callee_text, ctx, ast_graph)
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
        assert_has_node, assert_has_node_with_kind, collect_call_edges,
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

        // Outer's nested instance field
        assert_eq!(
            field_types.get(&("demo::Outer".to_string(), "nested_instance".to_string())),
            Some(&"Inner".to_string())
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
}
