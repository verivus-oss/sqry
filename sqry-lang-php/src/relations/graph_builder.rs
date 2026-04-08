//! PHP `GraphBuilder` implementation for tier-2 graph coverage.
//!
//! Migrated to use unified `GraphBuildHelper` following Phase 2.
//!
//! # Supported Features
//!
//! - Function definitions
//! - Class definitions
//! - Method definitions (including static methods)
//! - Function calls
//! - Method calls
//! - Static method calls
//! - Namespace handling
//! - Import edges:
//!   - `use Namespace\Class` statements
//!   - `use Namespace\Class as Alias` aliased imports
//!   - `use Namespace\{Class1, Class2}` grouped imports
//!   - `use function Namespace\func` function imports
//!   - `use const Namespace\CONST` constant imports
//!   - `require`, `require_once`, `include`, `include_once` statements
//! - OOP edges:
//!   - `class Child extends Parent` inheritance
//!   - `class Foo implements IBar, IBaz` interface implementation
//!   - `use SomeTrait` trait usage within classes
//! - Export edges:
//!   - All top-level classes, interfaces, traits, and functions are exported
//!   - PHP's module system treats all top-level symbols as implicitly visible
//! - `TypeOf` and Reference edges:
//!   - `@param {Type}` `PHPDoc` annotations for function/method parameters
//!   - `@return {Type}` `PHPDoc` annotations for function/method return types
//!   - `@var {Type}` `PHPDoc` annotations for variable and property declarations

use std::collections::HashMap;
use std::path::Path;

use sqry_core::graph::unified::edge::kind::{FfiConvention, TypeOfContext};
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, GraphSnapshot, Language, Span,
};
use tree_sitter::{Node, Tree};

use super::phpdoc_parser::{extract_phpdoc_comment, parse_phpdoc_tags};
use super::type_extractor::{canonical_type_string, extract_type_names};

/// Maximum namespace nesting depth to prevent pathological cases.
const DEFAULT_MAX_SCOPE_DEPTH: usize = 5;

/// PHP-specific graph builder.
#[derive(Debug)]
pub struct PhpGraphBuilder {
    pub max_scope_depth: usize,
}

impl Default for PhpGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_MAX_SCOPE_DEPTH,
        }
    }
}

impl GraphBuilder for PhpGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Php);

        // Build AST context for O(1) function lookups
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Map qualified names to NodeIds for call edge creation
        let mut node_map = HashMap::new();

        // Phase 1: Create function/method/class nodes
        for context in ast_graph.contexts() {
            let qualified_name = &context.qualified_name;
            let span = Span::from_bytes(context.span.0, context.span.1);

            let node_id = match &context.kind {
                ContextKind::Function { is_async } => helper.add_function_with_signature(
                    qualified_name,
                    Some(span),
                    *is_async,
                    false, // PHP functions are not unsafe
                    None,  // PHP functions don't have visibility modifiers
                    context.return_type.as_deref(),
                ),
                ContextKind::Method {
                    is_async,
                    is_static,
                    visibility: _,
                } => {
                    // Note: Visibility metadata is stored in the CallContext and used during export filtering.
                    // It's not added to the node metadata at this time due to GraphBuildHelper API limitations.
                    // The export phase (Phase 4) will filter methods based on visibility.
                    helper.add_method_with_signature(
                        qualified_name,
                        Some(span),
                        *is_async,
                        *is_static,
                        None, // Visibility not yet supported in GraphBuildHelper API
                        context.return_type.as_deref(),
                    )
                }
                ContextKind::Class => helper.add_class(qualified_name, Some(span)),
            };
            node_map.insert(qualified_name.clone(), node_id);
        }

        // Phase 2: Walk the tree to find calls, imports, and OOP relationships
        let root = tree.root_node();
        walk_tree_for_edges(root, content, &ast_graph, &mut helper, &mut node_map)?;

        // Phase 3: Process class inheritance and interface implementations
        process_oop_relationships(root, content, &mut helper, &mut node_map);

        // Phase 4: Generate export edges for all top-level symbols
        // In PHP, all classes/interfaces/traits/functions are implicitly exported
        process_exports(root, content, &mut helper, &mut node_map);

        // Phase 5: Process PHPDoc annotations for TypeOf and Reference edges
        process_phpdoc_annotations(root, content, &mut helper)?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Php
    }

    fn detect_cross_language_edges(
        &self,
        _snapshot: &GraphSnapshot,
    ) -> GraphResult<Vec<sqry_core::graph::CodeEdge>> {
        // Cross-file edge detection not implemented by design.
        // Intra-file FFI detection is implemented in build_graph() above.
        Ok(vec![])
    }
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
enum ContextKind {
    Function {
        is_async: bool,
    },
    Method {
        is_async: bool,
        is_static: bool,
        #[allow(dead_code)] // Used in export_public_methods_from_class via AST traversal
        visibility: Option<String>,
    },
    Class,
}

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: (usize, usize),
    kind: ContextKind,
    class_name: Option<String>,
    return_type: Option<String>,
}

struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();
        let mut scope_stack: Vec<String> = Vec::new();
        let mut class_stack: Vec<String> = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        let mut walk_ctx = WalkContext {
            contexts: &mut contexts,
            node_to_context: &mut node_to_context,
            scope_stack: &mut scope_stack,
            class_stack: &mut class_stack,
            max_depth,
        };

        walk_ast(tree.root_node(), content, &mut walk_ctx, &mut guard)?;

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    fn contexts(&self) -> &[CallContext] {
        &self.contexts
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "PHP namespace and scope handling requires a large, unified traversal."
)]
/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
/// Context for AST walking, bundling mutable state to reduce parameter count.
struct WalkContext<'a> {
    contexts: &'a mut Vec<CallContext>,
    node_to_context: &'a mut HashMap<usize, usize>,
    scope_stack: &'a mut Vec<String>,
    class_stack: &'a mut Vec<String>,
    max_depth: usize,
}

#[allow(clippy::too_many_lines)]
fn walk_ast(
    node: Node,
    content: &[u8],
    ctx: &mut WalkContext,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), String> {
    guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    if ctx.scope_stack.len() > ctx.max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        "program" => {
            // Special handling for program node to properly track semicolon-style namespaces.
            // In PHP, `namespace Foo;` affects all subsequent sibling declarations at program level.
            let mut active_namespace_parts: Vec<String> = Vec::new();

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "namespace_definition" {
                    // Check if this is semicolon-style or brace-style
                    let has_body = child
                        .children(&mut child.walk())
                        .any(|c| matches!(c.kind(), "compound_statement" | "declaration_list"));

                    let ns_name = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(content).ok())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();

                    if has_body {
                        // Brace-style: `namespace Foo { ... }` - process with its own scope
                        //
                        // Robustness: If a brace-style namespace follows a semicolon-style
                        // namespace (invalid PHP, but possible in fixtures/partial parses),
                        // we must first clear the active semicolon namespace to avoid
                        // scope pollution.
                        for _ in 0..active_namespace_parts.len() {
                            ctx.scope_stack.pop();
                        }
                        active_namespace_parts.clear();

                        let ns_parts: Vec<String> = if ns_name.is_empty() {
                            Vec::new()
                        } else {
                            ns_name.split('\\').map(ToString::to_string).collect()
                        };

                        for part in &ns_parts {
                            ctx.scope_stack.push(part.clone());
                        }

                        // Process children of the brace body
                        for ns_child in child.children(&mut child.walk()) {
                            if matches!(ns_child.kind(), "compound_statement" | "declaration_list")
                            {
                                for body_child in ns_child.children(&mut ns_child.walk()) {
                                    walk_ast(body_child, content, ctx, guard)?;
                                }
                            }
                        }

                        for _ in 0..ns_parts.len() {
                            ctx.scope_stack.pop();
                        }
                    } else {
                        // Semicolon-style: `namespace Foo;` - update active namespace
                        // First, pop any previous namespace from scope_stack
                        for _ in 0..active_namespace_parts.len() {
                            ctx.scope_stack.pop();
                        }

                        // Set the new active namespace
                        active_namespace_parts = if ns_name.is_empty() {
                            Vec::new()
                        } else {
                            ns_name.split('\\').map(ToString::to_string).collect()
                        };

                        // Push new namespace parts to scope_stack
                        for part in &active_namespace_parts {
                            ctx.scope_stack.push(part.clone());
                        }
                    }
                } else {
                    // Non-namespace declaration at program level - uses current scope
                    walk_ast(child, content, ctx, guard)?;
                }
            }

            // Clean up any remaining namespace from scope_stack
            for _ in 0..active_namespace_parts.len() {
                ctx.scope_stack.pop();
            }

            guard.exit();
            return Ok(());
        }
        "namespace_definition" => {
            // This branch handles namespace definitions when NOT at program level
            // (e.g., nested namespaces or when called from other ctx.contexts)
            let namespace_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            let namespace_parts: Vec<String> = if namespace_name.is_empty() {
                Vec::new()
            } else {
                namespace_name
                    .split('\\')
                    .map(ToString::to_string)
                    .collect()
            };

            let parts_count = namespace_parts.len();
            for part in &namespace_parts {
                ctx.scope_stack.push(part.clone());
            }

            // Recurse into namespace body (either braced block or rest of file)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "compound_statement" | "declaration_list") {
                    let mut body_cursor = child.walk();
                    for body_child in child.children(&mut body_cursor) {
                        walk_ast(body_child, content, ctx, guard)?;
                    }
                }
            }

            // Pop namespace parts
            for _ in 0..parts_count {
                ctx.scope_stack.pop();
            }
        }
        "class_declaration" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| "class_declaration missing name".to_string())?;
            let class_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read class name".to_string())?;

            // Build qualified class name using PHP namespace separator
            let qualified_class = if ctx.scope_stack.is_empty() {
                class_name.to_string()
            } else {
                format!("{}\\{}", ctx.scope_stack.join("\\"), class_name)
            };

            ctx.class_stack.push(qualified_class.clone());
            ctx.scope_stack.push(class_name.to_string());

            // Add class context
            let _context_idx = ctx.contexts.len();
            ctx.contexts.push(CallContext {
                qualified_name: qualified_class.clone(),
                span: (node.start_byte(), node.end_byte()),
                kind: ContextKind::Class,
                class_name: Some(qualified_class),
                return_type: None, // Classes don't have return types
            });

            // Recurse into class body
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "declaration_list" {
                    let mut body_cursor = child.walk();
                    for body_child in child.children(&mut body_cursor) {
                        walk_ast(body_child, content, ctx, guard)?;
                    }
                }
            }

            ctx.class_stack.pop();
            ctx.scope_stack.pop();
        }
        "function_definition" | "method_declaration" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| format!("{} missing name", node.kind()).to_string())?;
            let func_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read function name".to_string())?;

            // Check if async (PHP 8.1+ supports async/await via Fibers)
            let is_async = false; // PHP doesn't have native async keyword like JS/Python

            // Check if static method
            let is_static = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "static_modifier");

            // Extract visibility modifier for methods (public, private, protected)
            let visibility = extract_visibility(&node, content);

            // Extract return type annotation (PHP 7.0+)
            let return_type = extract_return_type(&node, content);

            // Determine if this is a method (inside a class)
            let is_method = !ctx.class_stack.is_empty();
            let class_name = ctx.class_stack.last().cloned();

            // Build qualified function/method name
            // For methods: use ClassName::methodName format (with ::)
            // For functions: use Namespace\functionName format (with \)
            let qualified_func = if is_method {
                // Method: use ClassName::methodName
                if let Some(ref class) = class_name {
                    format!("{class}::{func_name}")
                } else {
                    func_name.to_string()
                }
            } else {
                // Function: use namespace\function format
                if ctx.scope_stack.is_empty() {
                    func_name.to_string()
                } else {
                    format!("{}\\{}", ctx.scope_stack.join("\\"), func_name)
                }
            };

            let kind = if is_method {
                ContextKind::Method {
                    is_async,
                    is_static,
                    visibility: visibility.clone(),
                }
            } else {
                ContextKind::Function { is_async }
            };

            let context_idx = ctx.contexts.len();
            ctx.contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: (node.start_byte(), node.end_byte()),
                kind,
                class_name,
                return_type,
            });

            // Associate all descendants with this context
            if let Some(body) = node.child_by_field_name("body") {
                associate_descendants(body, context_idx, ctx.node_to_context);
            }

            ctx.scope_stack.push(func_name.to_string());

            // Recurse into function body to find nested functions
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(child, content, ctx, guard)?;
                }
            }

            ctx.scope_stack.pop();
        }
        _ => {
            // Recurse into children for other node types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, content, ctx, guard)?;
            }
        }
    }

    guard.exit();
    Ok(())
}

fn associate_descendants(
    node: Node,
    context_idx: usize,
    node_to_context: &mut HashMap<usize, usize>,
) {
    node_to_context.insert(node.id(), context_idx);

    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        node_to_context.insert(current.id(), context_idx);

        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ============================================================================
// Edge Building - calls, method calls, static calls
// ============================================================================

/// Walk the AST tree to create edges (calls, imports)
#[allow(clippy::only_used_in_recursion)]
fn walk_tree_for_edges(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) -> GraphResult<()> {
    match node.kind() {
        "function_call_expression" => {
            process_function_call(node, content, ast_graph, helper, node_map);
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            process_member_call(node, content, ast_graph, helper, node_map);
        }
        "scoped_call_expression" => {
            process_static_call(node, content, ast_graph, helper, node_map);
        }
        // Import edges for namespace use declarations
        "namespace_use_declaration" => {
            process_namespace_use(node, content, helper);
        }
        // Import edges for require/require_once/include/include_once
        "expression_statement" => {
            // Check for require/include expressions within expression statements
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "require_expression"
                    | "require_once_expression"
                    | "include_expression"
                    | "include_once_expression" => {
                        process_file_include(child, content, helper);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_edges(child, content, ast_graph, helper, node_map)?;
    }

    Ok(())
}

fn process_function_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    let Ok(callee_name) = function_node.utf8_text(content) else {
        return;
    };

    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Get or create caller node
    let source_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Get or create callee node
    let target_id = *node_map
        .entry(callee_name.to_string())
        .or_insert_with(|| helper.add_function(callee_name, None, false, false));

    let argument_count = count_call_arguments(node);
    let call_span = span_from_node(node);
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        false,
        vec![call_span],
    );
}

fn process_member_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let Some(method_node) = node.child_by_field_name("name") else {
        return;
    };

    let Ok(method_name) = method_node.utf8_text(content) else {
        return;
    };

    // Check if this is an FFI call (e.g., $ffi->crypto_encrypt())
    if let Some(object_node) = node.child_by_field_name("object")
        && is_php_ffi_call(object_node, content)
    {
        process_ffi_member_call(node, method_name, ast_graph, helper, node_map);
        return;
    }

    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // For $this->method(), resolve to ClassName::method using :: separator
    let callee_qualified = if let Some(class_name) = &call_context.class_name {
        format!("{class_name}::{method_name}")
    } else {
        method_name.to_string()
    };

    // Get or create caller node
    let source_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Get or create callee node
    let target_id = *node_map
        .entry(callee_qualified.clone())
        .or_insert_with(|| helper.add_method(&callee_qualified, None, false, false));

    let argument_count = count_call_arguments(node);
    let call_span = span_from_node(node);
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        false,
        vec![call_span],
    );
}

fn process_static_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let Some(scope_node) = node.child_by_field_name("scope") else {
        return;
    };
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };

    let Ok(class_name) = scope_node.utf8_text(content) else {
        return;
    };
    let Ok(method_name) = name_node.utf8_text(content) else {
        return;
    };

    // Check if this is an FFI static call (FFI::cdef() or FFI::load())
    if is_ffi_static_call(class_name, method_name) {
        process_ffi_static_call(node, method_name, ast_graph, helper, node_map, content);
        return;
    }

    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Static call: Class::method() - use :: separator for methods
    let callee_qualified = format!("{class_name}::{method_name}");

    // Get or create caller node
    let source_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Get or create callee node
    let target_id = *node_map.entry(callee_qualified.clone()).or_insert_with(|| {
        helper.add_method(&callee_qualified, None, false, true) // static method
    });

    let argument_count = count_call_arguments(node);
    let call_span = span_from_node(node);
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        false,
        vec![call_span],
    );
}

// ============================================================================
// Import Edge Building - namespace use, require, include
// ============================================================================

/// Process PHP `use` declarations for namespace imports.
///
/// Handles:
/// - `use Namespace\Class;` - simple use
/// - `use Namespace\Class as Alias;` - aliased use
/// - `use Namespace\{Class1, Class2};` - grouped use
/// - `use function Namespace\func;` - function use
/// - `use const Namespace\CONST;` - constant use
fn process_namespace_use(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Create a module node for the current file
    let file_path = helper.file_path().to_string();
    let importer_id = helper.add_module(&file_path, None);

    // For grouped imports, we need to extract the prefix at the declaration level
    // AST: namespace_use_declaration > namespace_name > namespace_use_group
    let mut prefix = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "namespace_name"
            && let Ok(ns) = child.utf8_text(content)
        {
            prefix = ns.trim().to_string();
            break;
        }
    }

    // Process children for imports
    cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_use_clause" => {
                // Simple or aliased use: use Namespace\Class [as Alias];
                process_use_clause(child, content, helper, importer_id);
            }
            "namespace_use_group" => {
                // Grouped use: use Namespace\{Class1, Class2};
                // Pass the prefix we extracted at the declaration level
                process_use_group(child, content, helper, importer_id, &prefix);
            }
            _ => {}
        }
    }
}

/// Process a single `use` clause like `Namespace\Class` or `Namespace\Class as Alias`.
///
/// AST structure for aliased use (`use App\Services\Mailer as Mail;`):
/// ```text
/// namespace_use_clause
///   qualified_name "App\Services\Mailer"
///     namespace_name "App\Services"
///     name "Mailer"
///   as "as"
///   name "Mail"   <- this is the alias (sibling, not nested)
/// ```
fn process_use_clause(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    import_source_id: NodeId,
) {
    process_use_clause_with_prefix(node, content, helper, import_source_id, None);
}

/// Process a use clause with an optional namespace prefix (for grouped imports).
fn process_use_clause_with_prefix(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    import_source_id: NodeId,
    prefix: Option<&str>,
) {
    // Get the qualified name (e.g., "App\Services\Mailer")
    let mut qualified_name = None;
    let mut alias = None;
    let mut found_as = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "qualified_name" => {
                // Full qualified name like "App\Services\Mailer"
                if let Ok(name) = child.utf8_text(content) {
                    qualified_name = Some(name.trim().to_string());
                }
            }
            "namespace_name" => {
                // Namespace part - only use if no qualified_name yet
                if qualified_name.is_none()
                    && let Ok(name) = child.utf8_text(content)
                {
                    qualified_name = Some(name.trim().to_string());
                }
            }
            "name" => {
                // Could be simple name OR the alias after "as"
                if found_as {
                    // This is the alias name
                    if let Ok(alias_text) = child.utf8_text(content) {
                        alias = Some(alias_text.trim().to_string());
                    }
                } else if qualified_name.is_none() {
                    // Simple name without namespace
                    if let Ok(name) = child.utf8_text(content) {
                        qualified_name = Some(name.trim().to_string());
                    }
                }
            }
            "as" => {
                // Mark that the next "name" node is the alias
                found_as = true;
            }
            _ => {}
        }
    }

    if let Some(name) = qualified_name
        && !name.is_empty()
    {
        // Apply prefix for grouped imports
        let full_name = if let Some(pfx) = prefix {
            format!("{pfx}\\{name}")
        } else {
            name
        };

        // Create an import node for the imported symbol
        let span = span_from_node(node);
        let import_node_id = helper.add_import(&full_name, Some(span));

        // Add import edge with optional alias
        if let Some(alias_str) = alias {
            helper.add_import_edge_full(import_source_id, import_node_id, Some(&alias_str), false);
        } else {
            helper.add_import_edge(import_source_id, import_node_id);
        }
    }
}

/// Process a grouped use declaration like `use Namespace\{Class1, Class2, Class3 as C3}`.
///
/// AST structure for grouped use (`use App\Models\{User, Post, Comment};`):
/// ```text
/// namespace_use_declaration
///   use "use"
///   namespace_name "App\Models"   <- prefix is here, at declaration level
///   \ "\"
///   namespace_use_group            <- this is passed to us
///     { "{"
///     namespace_use_clause "User"  <- NOT namespace_use_group_clause!
///       name "User"
///     , ","
///     namespace_use_clause "Post"
///       name "Post"
///     ...
///     } "}"
/// ```
fn process_use_group(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    import_source_id: NodeId,
    prefix: &str,
) {
    // Process each clause in the group
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // The clauses inside the group are "namespace_use_clause", not "namespace_use_group_clause"
        if child.kind() == "namespace_use_clause" {
            // Reuse the same clause processing logic with the prefix
            process_use_clause_with_prefix(child, content, helper, import_source_id, Some(prefix));
        }
    }
}

/// Process file inclusion statements (require, `require_once`, include, `include_once`).
fn process_file_include(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Create importer node for current file
    let file_path = helper.file_path().to_string();
    let import_source_id = helper.add_module(&file_path, None);

    // Extract the file path from the expression
    // The path is typically a string literal or an expression
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string"
            || child.kind() == "encapsed_string"
            || child.kind() == "binary_expression"
        {
            if let Ok(path_text) = child.utf8_text(content) {
                // Clean up the path string (remove quotes)
                let cleaned_path = path_text
                    .trim()
                    .trim_start_matches(['\'', '"'])
                    .trim_end_matches(['\'', '"'])
                    .to_string();

                if !cleaned_path.is_empty() {
                    let span = span_from_node(node);
                    let import_node_id = helper.add_import(&cleaned_path, Some(span));
                    helper.add_import_edge(import_source_id, import_node_id);
                }
            }
            break;
        }
    }
}

// ============================================================================
// OOP Edge Building - inheritance, interfaces, traits
// ============================================================================

/// Process all class declarations to extract OOP relationships.
fn process_oop_relationships(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let kind = node.kind();
    if kind == "class_declaration" {
        process_class_oop(node, content, helper, node_map);
    } else if kind == "interface_declaration" {
        process_interface_inheritance(node, content, helper, node_map);
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        process_oop_relationships(child, content, helper, node_map);
    }
}

/// Process a class declaration to extract inheritance, interface implementation, and trait usage.
fn process_class_oop(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    // Get the class name
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(class_name) = name_node.utf8_text(content) else {
        return;
    };
    let class_name = class_name.trim();

    // Get or create the class node
    let span = span_from_node(node);
    let class_id = *node_map
        .entry(class_name.to_string())
        .or_insert_with(|| helper.add_class(class_name, Some(span)));

    // Process children to find base_clause (extends), class_interface_clause (implements), and use_declaration (traits)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "base_clause" => {
                // class Child extends Parent
                process_extends_clause(child, content, helper, node_map, class_id);
            }
            "class_interface_clause" => {
                // class Foo implements IBar, IBaz
                process_implements_clause(child, content, helper, node_map, class_id);
            }
            "declaration_list" => {
                // Look for trait use declarations inside the class body
                process_class_body_traits(child, content, helper, node_map, class_id);
            }
            _ => {}
        }
    }
}

/// Process `extends Parent` clause to create Inherits edge.
fn process_extends_clause(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    class_id: NodeId,
) {
    // base_clause contains the parent class name
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "name"
            || child.kind() == "qualified_name"
            || child.kind() == "namespace_name"
        {
            if let Ok(parent_name) = child.utf8_text(content) {
                let parent_name = parent_name.trim();
                if !parent_name.is_empty() {
                    let span = span_from_node(child);
                    let parent_id = *node_map
                        .entry(parent_name.to_string())
                        .or_insert_with(|| helper.add_class(parent_name, Some(span)));

                    helper.add_inherits_edge(class_id, parent_id);
                }
            }
            break;
        }
    }
}

/// Process `implements IFoo, IBar` clause to create Implements edges.
fn process_implements_clause(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    class_id: NodeId,
) {
    // class_interface_clause contains interface names
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "name" | "qualified_name" | "namespace_name")
            && let Ok(interface_name) = child.utf8_text(content)
        {
            let interface_name = interface_name.trim();
            if !interface_name.is_empty() {
                let span = span_from_node(child);
                let interface_id = *node_map
                    .entry(interface_name.to_string())
                    .or_insert_with(|| helper.add_interface(interface_name, Some(span)));

                helper.add_implements_edge(class_id, interface_id);
            }
        }
    }
}

/// Process trait usage within a class body (`use TraitName;`).
fn process_class_body_traits(
    declaration_list: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    class_id: NodeId,
) {
    let mut cursor = declaration_list.walk();
    for child in declaration_list.children(&mut cursor) {
        if child.kind() == "use_declaration" {
            // This is a trait use: use TraitName;
            process_trait_use(child, content, helper, node_map, class_id);
        }
    }
}

/// Process a single trait use declaration (`use TraitName, AnotherTrait;`).
fn process_trait_use(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    class_id: NodeId,
) {
    // use_declaration contains trait names
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "name" | "qualified_name" | "namespace_name")
            && let Ok(trait_name) = child.utf8_text(content)
        {
            let trait_name = trait_name.trim();
            if !trait_name.is_empty() {
                let span = span_from_node(child);
                // Use add_node for traits since there's no dedicated add_trait method
                // We'll use the Trait NodeKind
                let trait_id = *node_map.entry(trait_name.to_string()).or_insert_with(|| {
                    helper.add_node(
                        trait_name,
                        Some(span),
                        sqry_core::graph::unified::node::NodeKind::Trait,
                    )
                });

                // Trait usage is modeled as an Implements edge
                // (similar to interface implementation from a semantic perspective)
                helper.add_implements_edge(class_id, trait_id);
            }
        }
    }
}

/// Process interface declaration to handle interface inheritance (`extends`).
fn process_interface_inheritance(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    // Get the interface name
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(interface_name) = name_node.utf8_text(content) else {
        return;
    };
    let interface_name = interface_name.trim();

    // Get or create the interface node
    let span = span_from_node(node);
    let interface_id = *node_map
        .entry(interface_name.to_string())
        .or_insert_with(|| helper.add_interface(interface_name, Some(span)));

    // Process base_clause for interface inheritance (interface IFoo extends IBar, IBaz)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "base_clause" {
            // Interface extends other interfaces
            let mut base_cursor = child.walk();
            for base_child in child.children(&mut base_cursor) {
                if matches!(
                    base_child.kind(),
                    "name" | "qualified_name" | "namespace_name"
                ) && let Ok(parent_name) = base_child.utf8_text(content)
                {
                    let parent_name = parent_name.trim();
                    if !parent_name.is_empty() {
                        let span = span_from_node(base_child);
                        let parent_id = *node_map
                            .entry(parent_name.to_string())
                            .or_insert_with(|| helper.add_interface(parent_name, Some(span)));

                        // Interface inheritance uses Inherits edge
                        helper.add_inherits_edge(interface_id, parent_id);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Export Edge Building - PHP implicitly exports all top-level symbols
// ============================================================================

/// Process all top-level declarations to create export edges.
///
/// In PHP, all classes, interfaces, traits, enums, and functions defined at the
/// top level (or within a namespace) are implicitly exported and visible to other
/// files via `require`/`use` statements. This function creates export edges from
/// the file module to each such symbol.
///
/// # Namespace Handling
///
/// PHP has two namespace forms:
/// - **Brace-style**: `namespace Foo { class Bar {} }` - contained declarations
/// - **Semicolon-style**: `namespace Foo; class Bar {}` - applies to subsequent siblings
///
/// This implementation handles both by doing a linear scan of `program` children.
fn process_exports(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    // Create module node for this file
    let file_path = helper.file_path().to_string();
    let module_id = helper.add_module(&file_path, None);

    // The program node is expected; if not, return early
    if node.kind() != "program" {
        return;
    }

    // Track current namespace prefix (for semicolon-style namespaces)
    let mut active_namespace = String::new();

    // Linear scan of program children to handle semicolon-style namespaces correctly
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        process_top_level_for_export(
            child,
            content,
            helper,
            node_map,
            module_id,
            &mut active_namespace,
        );
    }
}

/// Process a single top-level statement for export purposes.
///
/// This function is called for each direct child of the `program` node.
/// It handles:
/// - Namespace definitions (both brace and semicolon style)
/// - Class, interface, trait, enum, and function declarations
///
/// It explicitly does NOT recurse into function bodies, class bodies, or
/// other nested scopes to avoid incorrectly exporting nested declarations.
fn process_top_level_for_export(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    module_id: NodeId,
    active_namespace: &mut String,
) {
    match node.kind() {
        "namespace_definition" => {
            // Extract namespace name
            let ns_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            // Check if this is a brace-style namespace by looking for declaration_list/compound_statement
            let has_body = node
                .children(&mut node.walk())
                .any(|c| matches!(c.kind(), "compound_statement" | "declaration_list"));

            if has_body {
                // Brace-style namespace: `namespace Foo { ... }`
                //
                // Robustness: If a brace-style namespace follows a semicolon-style
                // namespace (invalid PHP, but possible in fixtures/partial parses),
                // clear the active namespace to avoid scope pollution.
                active_namespace.clear();

                // Process only declarations within the braced body
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if matches!(child.kind(), "compound_statement" | "declaration_list") {
                        let mut body_cursor = child.walk();
                        for body_child in child.children(&mut body_cursor) {
                            export_declaration_if_exportable(
                                body_child, content, helper, node_map, module_id, &ns_name,
                            );
                        }
                    }
                }
            } else {
                // Semicolon-style namespace: `namespace Foo;`
                // Updates the active namespace for subsequent sibling declarations
                *active_namespace = ns_name;
            }
        }
        // For top-level declarations, use the active namespace
        "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration"
        | "function_definition" => {
            export_declaration_if_exportable(
                node,
                content,
                helper,
                node_map,
                module_id,
                active_namespace,
            );
        }
        _ => {
            // Skip other node types (expression statements, comments, etc.)
            // We explicitly DO NOT recurse to avoid exporting nested declarations
        }
    }
}

/// Look up a node by qualified name, with restricted fallback to simple name.
///
/// When in the global namespace (`namespace_prefix` is empty), we allow fallback
/// to simple name for backwards compatibility. In namespaced ctx.contexts, we require
/// the qualified name to exist to avoid matching the wrong symbol when multiple
/// namespaces contain symbols with the same simple name.
fn lookup_or_create_node<F>(
    node_map: &mut HashMap<String, NodeId>,
    qualified_name: &str,
    simple_name: &str,
    namespace_prefix: &str,
    create_fn: F,
) -> NodeId
where
    F: FnOnce() -> NodeId,
{
    // Always try qualified name first
    if let Some(&id) = node_map.get(qualified_name) {
        return id;
    }

    // Fall back to simple name ONLY in global namespace to avoid mismatches
    // in namespaced files with repeated simple names across namespaces.
    if namespace_prefix.is_empty()
        && let Some(&id) = node_map.get(simple_name)
    {
        return id;
    }

    // Create new node with qualified name
    let id = create_fn();
    node_map.insert(qualified_name.to_string(), id);
    id
}

/// Export a single declaration (class, interface, trait, enum, or function).
///
/// This function handles the actual creation of export edges for top-level
/// declarations. It's called from two contexts:
/// 1. Direct children of `program` (with `active_namespace` from semicolon-style)
/// 2. Children of brace-style namespace bodies (with the namespace name)
///
/// We look up nodes by their qualified name (which includes namespace) because
/// that's what Phase 1 creates in the `node_map`. Fallback to simple name is only
/// allowed in the global namespace to prevent matching wrong symbols in namespaced
/// files with repeated simple names.
///
/// For classes, this also exports all public methods found within the class body.
#[allow(clippy::too_many_lines)] // Single traversal keeps export logic aligned with phases.
fn export_declaration_if_exportable(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    module_id: NodeId,
    namespace_prefix: &str,
) {
    match node.kind() {
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let simple_name = class_name.trim().to_string();
                let qualified_name = build_qualified_name(namespace_prefix, &simple_name);
                let span = span_from_node(node);

                let class_id = lookup_or_create_node(
                    node_map,
                    &qualified_name,
                    &simple_name,
                    namespace_prefix,
                    || helper.add_class(&qualified_name, Some(span)),
                );

                helper.add_export_edge(module_id, class_id);

                // Export public methods from the class
                export_public_methods_from_class(
                    node,
                    content,
                    helper,
                    node_map,
                    module_id,
                    &qualified_name,
                );
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(interface_name) = name_node.utf8_text(content)
            {
                let simple_name = interface_name.trim().to_string();
                let qualified_name = build_qualified_name(namespace_prefix, &simple_name);
                let span = span_from_node(node);

                let interface_id = lookup_or_create_node(
                    node_map,
                    &qualified_name,
                    &simple_name,
                    namespace_prefix,
                    || helper.add_interface(&qualified_name, Some(span)),
                );

                helper.add_export_edge(module_id, interface_id);
            }
        }
        "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(trait_name) = name_node.utf8_text(content)
            {
                let simple_name = trait_name.trim().to_string();
                let qualified_name = build_qualified_name(namespace_prefix, &simple_name);
                let span = span_from_node(node);

                let trait_id = lookup_or_create_node(
                    node_map,
                    &qualified_name,
                    &simple_name,
                    namespace_prefix,
                    || {
                        helper.add_node(
                            &qualified_name,
                            Some(span),
                            sqry_core::graph::unified::node::NodeKind::Trait,
                        )
                    },
                );

                helper.add_export_edge(module_id, trait_id);
            }
        }
        "enum_declaration" => {
            // PHP 8.1+ enums - they are top-level types that should be exported
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(enum_name) = name_node.utf8_text(content)
            {
                let simple_name = enum_name.trim().to_string();
                let qualified_name = build_qualified_name(namespace_prefix, &simple_name);
                let span = span_from_node(node);

                let enum_id = lookup_or_create_node(
                    node_map,
                    &qualified_name,
                    &simple_name,
                    namespace_prefix,
                    || helper.add_enum(&qualified_name, Some(span)),
                );

                helper.add_export_edge(module_id, enum_id);
            }
        }
        "function_definition" => {
            // Top-level functions are exported (we only get here for top-level nodes)
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(func_name) = name_node.utf8_text(content)
            {
                let simple_name = func_name.trim().to_string();
                let qualified_name = build_qualified_name(namespace_prefix, &simple_name);
                let span = span_from_node(node);

                let func_id = lookup_or_create_node(
                    node_map,
                    &qualified_name,
                    &simple_name,
                    namespace_prefix,
                    || helper.add_function(&qualified_name, Some(span), false, false),
                );

                helper.add_export_edge(module_id, func_id);
            }
        }
        _ => {
            // Not an exportable declaration type
        }
    }
}

/// Build a qualified name with namespace prefix.
fn build_qualified_name(namespace_prefix: &str, name: &str) -> String {
    if namespace_prefix.is_empty() {
        name.to_string()
    } else {
        format!("{namespace_prefix}\\{name}")
    }
}

/// Helper function to create a Span from a tree-sitter Node.
fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

fn count_call_arguments(call_node: Node<'_>) -> u8 {
    let args_node = call_node
        .child_by_field_name("arguments")
        .or_else(|| call_node.child_by_field_name("argument_list"))
        .or_else(|| {
            let mut cursor = call_node.walk();
            call_node
                .children(&mut cursor)
                .find(|child| child.kind() == "argument_list")
        });

    let Some(args_node) = args_node else {
        return 255;
    };
    let count = args_node.named_child_count();
    if count <= 254 {
        u8::try_from(count).unwrap_or(u8::MAX)
    } else {
        255
    }
}

/// Extract visibility modifier from a method or property declaration.
///
/// Returns Some("public"), Some("private"), Some("protected"), or None if no visibility modifier is found.
/// In PHP, methods without an explicit visibility modifier are implicitly public.
fn extract_visibility(node: &Node, content: &[u8]) -> Option<String> {
    // Look for visibility modifiers in direct children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "visibility_modifier" => {
                // The visibility_modifier node contains the actual keyword
                if let Ok(vis_text) = child.utf8_text(content) {
                    return Some(vis_text.trim().to_string());
                }
            }
            "public" | "private" | "protected" => {
                // Sometimes the visibility is directly as a keyword node
                if let Ok(vis_text) = child.utf8_text(content) {
                    return Some(vis_text.trim().to_string());
                }
            }
            _ => {}
        }
    }

    // PHP default: methods without explicit visibility are public
    // But we return None here to distinguish "explicitly public" from "implicitly public"
    // For export purposes, we'll treat None as public
    None
}

/// Export public methods from a class declaration.
///
/// This function walks the class body and exports only public methods (including
/// methods with no explicit visibility modifier, which are implicitly public in PHP).
/// Private and protected methods are NOT exported.
fn export_public_methods_from_class(
    class_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    module_id: NodeId,
    class_qualified_name: &str,
) {
    // Find the declaration_list (class body)
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if child.kind() == "declaration_list" {
            // Walk through the class body to find method declarations
            let mut body_cursor = child.walk();
            for body_child in child.children(&mut body_cursor) {
                if body_child.kind() == "method_declaration" {
                    // Extract method visibility
                    let visibility = extract_visibility(&body_child, content);

                    // Only export public methods (explicit or implicit)
                    let is_public = visibility.as_deref() == Some("public") || visibility.is_none();

                    if is_public {
                        // Extract method name
                        if let Some(name_node) = body_child.child_by_field_name("name")
                            && let Ok(method_name) = name_node.utf8_text(content)
                        {
                            let method_name = method_name.trim();
                            let qualified_method_name =
                                format!("{class_qualified_name}::{method_name}");

                            // Look up the method node (should exist from Phase 1)
                            if let Some(&method_id) = node_map.get(&qualified_method_name) {
                                helper.add_export_edge(module_id, method_id);
                            }
                        }
                    }
                }
            }
            break;
        }
    }
}

// ============================================================================
// Type Extraction Helpers
// ============================================================================

/// Extract return type annotation from a PHP function or method declaration.
///
/// PHP return types appear after the `formal_parameters` and a colon:
/// ```php
/// function greet(string $name): string { ... }
///                              ^^^^^^^
/// ```
///
/// This function:
/// 1. Finds the colon (`:`) after the parameters
/// 2. Extracts the next named node (the type annotation)
/// 3. Normalizes the type (strips nullable `?`, takes first type from unions)
///
/// Returns `None` if no return type annotation exists (valid in untyped PHP code).
fn extract_return_type(node: &Node, content: &[u8]) -> Option<String> {
    // Find colon after formal_parameters
    let mut found_colon = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if found_colon && child.is_named() {
            // Next named node after colon is the type annotation
            return extract_type_from_node(&child, content);
        }
        if child.kind() == ":" {
            found_colon = true;
        }
    }
    None
}

/// Extract type string from a PHP type annotation node.
///
/// Handles different type node kinds from tree-sitter-php:
/// - `primitive_type`: `string`, `int`, `float`, `bool`, `array`, etc.
/// - `optional_type`: `?string` → strips `?` and returns `string`
/// - `union_type`: `string|int` → returns first type `string`
/// - `named_type` / `qualified_name`: `User` or `Namespace\User`
/// - `intersection_type`: `A&B` → returns first type `A`
///
/// Design decisions (per SPEC.md):
/// - Nullable types: Strip `?` prefix for simplified matching
/// - Union types: Take first type only (matches TypeScript plugin approach)
/// - Intersection types: Take first type only
fn extract_type_from_node(type_node: &Node, content: &[u8]) -> Option<String> {
    match type_node.kind() {
        "primitive_type" => {
            // Basic types: string, int, float, bool, array, void, etc.
            type_node
                .utf8_text(content)
                .ok()
                .map(|s| s.trim().to_string())
        }
        "optional_type" => {
            // Nullable type: ?string
            // Strip the ? and extract underlying type
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.kind() != "?" && child.is_named() {
                    return extract_type_from_node(&child, content);
                }
            }
            None
        }
        "union_type" => {
            // Union type: string|int
            // Take first type only (per SPEC.md design decision)
            type_node
                .named_child(0)
                .and_then(|first_type| extract_type_from_node(&first_type, content))
        }
        "named_type" | "qualified_name" => {
            // Class names: User or Namespace\User
            type_node
                .utf8_text(content)
                .ok()
                .map(|s| s.trim().to_string())
        }
        "intersection_type" => {
            // Intersection type: A&B
            // Take first type only
            type_node
                .named_child(0)
                .and_then(|first_type| extract_type_from_node(&first_type, content))
        }
        _ => {
            // Fallback: try to get text directly for unknown type nodes
            // For future composite types (e.g., DNF types like (A&B)|C),
            // normalize by taking first type to stay consistent with
            // union/intersection handling.
            type_node
                .utf8_text(content)
                .ok()
                .map(|s| {
                    let trimmed = s.trim();
                    // Split on union (|) or intersection (&) and take first component
                    // This handles future PHP grammar additions like DNF types
                    trimmed
                        .split(&['|', '&'][..])
                        .next()
                        .unwrap_or(trimmed)
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim()
                        .to_string()
                })
                .filter(|s| !s.is_empty())
        }
    }
}

// ============================================================================
// PHPDoc Annotation Processing (Phase 5)
// ============================================================================

/// Process `PHPDoc` annotations for `TypeOf` and Reference edges
fn process_phpdoc_annotations(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Recursively walk the tree looking for nodes with PHPDoc
    match node.kind() {
        "function_definition" => {
            process_function_phpdoc(node, content, helper)?;
        }
        "method_declaration" => {
            process_method_phpdoc(node, content, helper)?;
        }
        "property_declaration" => {
            process_property_phpdoc(node, content, helper)?;
        }
        "simple_property" => {
            // Simple property declarations (e.g., inside property groups)
            process_property_phpdoc(node, content, helper)?;
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        process_phpdoc_annotations(child, content, helper)?;
    }

    Ok(())
}

/// Process `PHPDoc` for function definitions
fn process_function_phpdoc(
    func_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract PHPDoc comment
    let Some(phpdoc_text) = extract_phpdoc_comment(func_node, content) else {
        return Ok(());
    };

    // Parse PHPDoc tags
    let tags = parse_phpdoc_tags(&phpdoc_text);

    // Get function name
    let Some(name_node) = func_node.child_by_field_name("name") else {
        return Ok(());
    };

    let function_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(func_node),
            reason: "failed to read function name".to_string(),
        })?
        .trim()
        .to_string();

    if function_name.is_empty() {
        return Ok(());
    }

    // Get or create function node
    let func_node_id = helper.ensure_function(&function_name, None, false, false);

    // Extract AST parameter list with indices for context (not used in Phase 1)
    let _ast_params = extract_ast_parameters(func_node, content);

    // Process @param tags
    // Create TypeOf and Reference edges regardless of whether the parameter exists in AST
    // (PHPDoc may contain documentation for parameters that exist in the signature)
    for (param_idx, param_tag) in tags.params.iter().enumerate() {
        // Create TypeOf edge: function -> parameter type
        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            func_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            param_idx.try_into().ok(), // Use PHPDoc order as index
            Some(&param_tag.name),
        );

        // Create Reference edges: function -> each referenced type
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(func_node_id, ref_type_id);
        }
    }

    // Process @return tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            func_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges for return type
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(func_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process `PHPDoc` for method definitions
fn process_method_phpdoc(
    method_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract PHPDoc comment
    let Some(phpdoc_text) = extract_phpdoc_comment(method_node, content) else {
        return Ok(());
    };

    // Parse PHPDoc tags
    let tags = parse_phpdoc_tags(&phpdoc_text);

    // Get method name
    let Some(name_node) = method_node.child_by_field_name("name") else {
        return Ok(());
    };

    let method_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(method_node),
            reason: "failed to read method name".to_string(),
        })?
        .trim()
        .to_string();

    if method_name.is_empty() {
        return Ok(());
    }

    // Find the class name by walking up the tree
    let class_name = get_enclosing_class_name(method_node, content)?;
    let Some(class_name) = class_name else {
        return Ok(());
    };

    // Create qualified method name: ClassName::methodName
    let qualified_name = format!("{class_name}.{method_name}");

    // Get existing method node (should already exist from main traversal)
    // Use ensure_method to handle case where it might not exist yet
    let method_node_id = helper.ensure_method(&qualified_name, None, false, false);

    // Extract AST parameter list with indices for context
    let _ast_params = extract_ast_parameters(method_node, content);

    // Process @param tags
    // Create TypeOf and Reference edges regardless of whether the parameter exists in AST
    for (param_idx, param_tag) in tags.params.iter().enumerate() {
        // Create TypeOf edge: method -> parameter type
        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            param_idx.try_into().ok(),
            Some(&param_tag.name),
        );

        // Create Reference edges: method -> each referenced type
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    // Process @return tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges for return type
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process `PHPDoc` for property declarations
#[allow(clippy::unnecessary_wraps)]
fn process_property_phpdoc(
    prop_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract PHPDoc comment
    let Some(phpdoc_text) = extract_phpdoc_comment(prop_node, content) else {
        return Ok(());
    };

    // Parse PHPDoc tags
    let tags = parse_phpdoc_tags(&phpdoc_text);

    // Only process @var tags for properties
    let Some(var_type) = &tags.var_type else {
        return Ok(());
    };

    // Get property name(s)
    let property_names = extract_property_names(prop_node, content);

    if property_names.is_empty() {
        // Fallback: if we can't extract names, use a generic property identifier
        // This ensures we still create edges for the type information
        let generic_name = format!("property_{:?}", prop_node.id());
        let prop_node_id = helper.add_variable(&generic_name, None);

        // Create TypeOf edge: variable -> type
        let canonical_type = canonical_type_string(var_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            prop_node_id,
            type_node_id,
            Some(TypeOfContext::Variable),
            None,
            Some(&generic_name),
        );

        // Create Reference edges: variable -> each referenced type
        let type_names = extract_type_names(var_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(prop_node_id, ref_type_id);
        }

        return Ok(());
    }

    // Process each property name
    for prop_name in property_names {
        // Get or create property node
        // Note: For now we treat properties as simple variables
        // Full property tracking may be added in future phases
        let prop_node_id = helper.add_variable(&prop_name, None);

        // Create TypeOf edge: variable -> type
        let canonical_type = canonical_type_string(var_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            prop_node_id,
            type_node_id,
            Some(TypeOfContext::Variable),
            None,
            Some(&prop_name),
        );

        // Create Reference edges: variable -> each referenced type
        let type_names = extract_type_names(var_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(prop_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Extract parameter names and indices from a function/method declaration
fn extract_ast_parameters(func_node: Node, content: &[u8]) -> Vec<(usize, String)> {
    let mut params = Vec::new();

    // Find parameters node
    let Some(params_node) = func_node.child_by_field_name("parameters") else {
        return params;
    };

    let mut index = 0;
    let mut cursor = params_node.walk();

    for child in params_node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }

        match child.kind() {
            "simple_parameter" => {
                // Extract parameter name (typically the second child, which is the variable)
                let mut param_cursor = child.walk();
                for param_child in child.children(&mut param_cursor) {
                    if param_child.kind() == "variable_name"
                        && let Ok(param_text) = param_child.utf8_text(content)
                    {
                        params.push((index, param_text.trim().to_string()));
                        index += 1;
                        break;
                    }
                }
            }
            "variadic_parameter" => {
                // Extract parameter name from variadic parameter (e.g., ...$args)
                let mut param_cursor = child.walk();
                for param_child in child.children(&mut param_cursor) {
                    if param_child.kind() == "variable_name"
                        && let Ok(param_text) = param_child.utf8_text(content)
                    {
                        params.push((index, param_text.trim().to_string()));
                        index += 1;
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    params
}

/// Get the enclosing class name for a method node
#[allow(clippy::unnecessary_wraps)]
fn get_enclosing_class_name(node: Node, content: &[u8]) -> GraphResult<Option<String>> {
    let mut current = node;

    // Walk up the tree to find the enclosing class
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            // Found the class, extract its name
            if let Some(name_node) = parent.child_by_field_name("name")
                && let Ok(name_text) = name_node.utf8_text(content)
            {
                return Ok(Some(name_text.trim().to_string()));
            }
            return Ok(None);
        }
        current = parent;
    }

    Ok(None)
}

/// Extract property names from a property declaration
fn extract_property_names(prop_node: Node, content: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    match prop_node.kind() {
        "property_declaration" => {
            // Extract all variable names from the property declaration
            // PHP properties can be: property_initializer or simple_property children
            let mut cursor = prop_node.walk();
            for child in prop_node.children(&mut cursor) {
                match child.kind() {
                    "property_initializer" => {
                        // Property with initializer: $name = value
                        if let Some(var_node) = child.child_by_field_name("name")
                            && let Ok(var_text) = var_node.utf8_text(content)
                        {
                            names.push(var_text.trim().to_string());
                        }
                    }
                    "simple_property" => {
                        // Simple property without initializer: $name
                        if let Some(var_node) = child.child_by_field_name("name")
                            && let Ok(var_text) = var_node.utf8_text(content)
                        {
                            names.push(var_text.trim().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        "simple_property" => {
            // Handle simple property declarations directly
            if let Some(var_node) = prop_node.child_by_field_name("name")
                && let Ok(var_text) = var_node.utf8_text(content)
            {
                names.push(var_text.trim().to_string());
            }
        }
        _ => {}
    }

    names
}

// ============================================================================
// FFI Edge Building
// ============================================================================

/// Process FFI member call (e.g., `$ffi->crypto_encrypt()`).
///
/// Creates an `FfiCall` edge from the caller to a native module node.
fn process_ffi_member_call(
    node: Node,
    method_name: &str,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Get or create caller node
    let source_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Create a native module node for the C function
    let ffi_name = format!("native::ffi::{method_name}");
    let call_span = span_from_node(node);
    let target_id = helper.add_module(&ffi_name, Some(call_span));

    // Add FFI edge (PHP FFI uses C calling convention)
    helper.add_ffi_edge(source_id, target_id, FfiConvention::C);
}

/// Process FFI static call (`FFI::cdef()` or `FFI::load()`).
///
/// Creates an `FfiCall` edge from the caller to a native module representing
/// the loaded library.
fn process_ffi_static_call(
    node: Node,
    method_name: &str,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    content: &[u8],
) {
    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Get or create caller node
    let source_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Extract library name from call arguments
    let library_name = extract_php_ffi_library_name(node, content, method_name == "cdef")
        .map_or_else(
            || "unknown".to_string(),
            |lib| php_ffi_library_simple_name(&lib),
        );

    // Create a native module node for the library
    let ffi_name = format!("native::{library_name}");
    let call_span = span_from_node(node);
    let target_id = helper.add_module(&ffi_name, Some(call_span));

    // Add FFI edge (PHP FFI uses C calling convention)
    helper.add_ffi_edge(source_id, target_id, FfiConvention::C);
}

// ============================================================================
// FFI Detection Helpers
// ============================================================================

/// Check if a member call is a PHP FFI call (e.g., `$ffi->function_name()`).
///
/// Returns true for calls on objects that appear to be FFI instances.
/// Common patterns:
/// - `$ffi->...`, `self::$ffi->...`, `$this->ffi->...`
/// - `FFI::cdef(...)->...` (chained call)
/// - `FFI::load(...)->...` (chained call)
/// - `(FFI::cdef(...))->...` (parenthesized)
fn is_php_ffi_call(object_node: Node, content: &[u8]) -> bool {
    // Check for direct chained FFI call: FFI::cdef(...)->method()
    if object_node.kind() == "scoped_call_expression"
        && let Some(scope_node) = object_node.child_by_field_name("scope")
        && let Some(name_node) = object_node.child_by_field_name("name")
        && let Ok(scope_text) = scope_node.utf8_text(content)
        && let Ok(name_text) = name_node.utf8_text(content)
        && is_ffi_static_call(scope_text, name_text)
    {
        return true;
    }

    // Check for parenthesized FFI call: (FFI::cdef(...))->method()
    if object_node.kind() == "parenthesized_expression"
        && let Some(inner) = object_node.named_child(0)
        && inner.kind() == "scoped_call_expression"
        && let Some(scope_node) = inner.child_by_field_name("scope")
        && let Some(name_node) = inner.child_by_field_name("name")
        && let Ok(scope_text) = scope_node.utf8_text(content)
        && let Ok(name_text) = name_node.utf8_text(content)
        && is_ffi_static_call(scope_text, name_text)
    {
        return true;
    }

    // Check text patterns for stored FFI objects
    let Ok(object_text) = object_node.utf8_text(content) else {
        return false;
    };

    let object_text = object_text.trim();

    // Direct FFI object: $ffi->method()
    if object_text == "$ffi" || object_text == "$_ffi" {
        return true;
    }

    // Class property FFI: $this->ffi->method() or self::$ffi->method()
    if object_text.ends_with("->ffi")
        || object_text.ends_with("::$ffi")
        || object_text.ends_with("->_ffi")
        || object_text.ends_with("::$_ffi")
    {
        return true;
    }

    false
}

/// Check if a static call is `FFI::cdef()` or `FFI::load()`.
///
/// Accepts both `FFI` and `\FFI` (fully-qualified) patterns.
fn is_ffi_static_call(scope_text: &str, method_text: &str) -> bool {
    (scope_text == "FFI" || scope_text == "\\FFI")
        && (method_text == "cdef" || method_text == "load")
}

/// Extract library name from FFI call arguments.
///
/// Handles both positional and named arguments:
/// - `FFI::cdef("...", "lib.so")`: positional second argument
/// - `FFI::cdef(lib: "lib.so", cdef: "...")`: named `lib` argument
/// - `FFI::load("header.h")`: positional first argument
/// - `FFI::load(filename: "header.h")`: named `filename` argument
fn extract_php_ffi_library_name(call_node: Node, content: &[u8], is_cdef: bool) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;

    let mut cursor = args.walk();
    let args_vec: Vec<Node> = args
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","))
        .collect();

    // For FFI::cdef, look for named "lib" argument first
    // For FFI::load, look for named "filename" argument first
    let target_arg_name = if is_cdef { "lib" } else { "filename" };

    // Try to find argument by name (PHP 8 named arguments)
    if let Some(named_arg) = find_named_argument(&args_vec, target_arg_name, content) {
        return extract_string_from_argument(named_arg, content);
    }

    // Fall back to positional arguments (PHP 7 style)
    if is_cdef {
        // FFI::cdef() - second argument is library path
        args_vec
            .get(1)
            .and_then(|arg| extract_string_from_argument(*arg, content))
    } else {
        // FFI::load() - first argument is filename
        args_vec
            .first()
            .and_then(|arg| extract_string_from_argument(*arg, content))
    }
}

/// Find a named argument by its parameter name.
///
/// PHP 8 named arguments: `func(param: value)`
/// Tree structure: `argument { name: "param", ":", value }`
///
/// Uses field-based access for resilience against grammar changes.
fn find_named_argument<'a>(args: &'a [Node], param_name: &str, content: &[u8]) -> Option<Node<'a>> {
    for arg in args {
        if arg.kind() != "argument" {
            continue;
        }

        // Check if this is a named argument (has 2+ named children)
        // This is a quick check before trying field-based access
        if arg.named_child_count() < 2 {
            continue;
        }

        // Try field-based access first (more resilient)
        if let Some(name_node) = arg.child_by_field_name("name")
            && let Ok(name_text) = name_node.utf8_text(content)
            && name_text == param_name
        {
            return Some(*arg);
        } else if let Some(name_node) = arg.named_child(0)
            && let Ok(name_text) = name_node.utf8_text(content)
            && name_text == param_name
        {
            // Fallback to child ordering if field not available
            return Some(*arg);
        }
    }

    None
}

/// Extract string literal from an argument node, handling both positional and named arguments.
///
/// PHP 7.x positional: `argument(1 child) -> value`
/// PHP 8.x named: `argument(2+ children) -> name -> value`
///
/// Returns `None` if the argument is not a valid string literal, for example a variable,
/// constant, or interpolated string.
fn extract_string_from_argument(arg_node: Node, content: &[u8]) -> Option<String> {
    // Unwrap argument wrappers to get to the actual value expression
    let value_node = unwrap_argument_node(arg_node)?;

    // Only accept pure string literals, not variables or constants
    if !is_string_literal_node(value_node) {
        return None;
    }

    // Reject interpolated strings (e.g., "lib{$var}.so")
    if is_interpolated_string(value_node) {
        return None;
    }

    extract_php_string_content(value_node, content)
}

/// Unwrap PHP argument node wrappers to get to the value expression.
///
/// Handles:
/// - `argument` nodes with 1 child: PHP 7.x positional args (argument -> value)
/// - `argument` nodes with 2+ children: PHP 8.x named args (argument -> name -> value)
///
/// Uses field-based skipping to extract the value child while excluding
/// the `name` field (named argument parameter name) and `reference_modifier`
/// field (& reference marker). This correctly handles cases where the value
/// itself is a `name` node (e.g., `self`, `parent`, `static`, class names).
/// Returns the innermost value expression.
fn unwrap_argument_node(node: Node) -> Option<Node> {
    if node.kind() != "argument" {
        // Not a wrapper, return as-is
        return Some(node);
    }

    // Tree-sitter-php 0.24.2 `argument` nodes have:
    // - "name" field (for named arguments parameter name)
    // - "reference_modifier" field (for & references)
    // - No "value" field (must select by exclusion)
    //
    // Get the field nodes to exclude by identity comparison
    let name_field_node = node.child_by_field_name("name");
    let ref_modifier_field_node = node.child_by_field_name("reference_modifier");

    // Find the value child by excluding structural field nodes
    for i in 0..node.named_child_count() {
        #[allow(clippy::cast_possible_truncation)] // tree-sitter child count fits in u32
        if let Some(child) = node.named_child(i as u32) {
            // Skip if this child is the name field or reference_modifier field
            let is_name_field = name_field_node.is_some_and(|n| n.id() == child.id());
            let is_ref_modifier = ref_modifier_field_node.is_some_and(|n| n.id() == child.id());

            if !is_name_field && !is_ref_modifier {
                // This is the value child (expression, variadic_unpacking, or name node like self/parent/static)
                return Some(child);
            }
        }
    }

    // If no value child found, return None (malformed argument)
    None
}

/// Check if a node is a string literal (not a variable or constant).
///
/// PHP tree-sitter uses different node kinds for various string types:
/// - `string` for single-quoted strings (`'...'`)
/// - `encapsed_string` for double-quoted strings (`"..."`)
/// - `heredoc` and `nowdoc` for heredoc/nowdoc syntax
fn is_string_literal_node(node: Node) -> bool {
    matches!(
        node.kind(),
        "string" | "encapsed_string" | "heredoc" | "nowdoc"
    )
}

/// Check if a string node contains variable interpolation.
///
/// Double-quoted strings and heredocs can contain interpolation:
/// - `lib{$suffix}.so`: simple variable
/// - `path/$variable/file`: simple variable
/// - `{$arr['key']}`: array access
/// - `{$obj->prop}`: property access
///
/// Single-quoted strings and nowdocs never interpolate, so we only check
/// `encapsed_string` and `heredoc` nodes.
///
/// Scans all descendants recursively to catch complex interpolation patterns.
fn is_interpolated_string(node: Node) -> bool {
    if !matches!(node.kind(), "encapsed_string" | "heredoc") {
        return false;
    }

    // Recursively check all descendants for variable-bearing nodes
    has_variable_node(node)
}

/// Recursively check if a node or any of its descendants contains variables or dynamic expressions.
///
/// Detects all forms of interpolation:
/// - Direct variables: `$var`, `${expr}`
/// - Dynamic variables: `$$var`
/// - Array access: `$arr['key']`, `$arr[$index]`
/// - Property access: `$obj->prop`
/// - Method calls: `$obj->method()`
/// - Function calls: `$foo()`
/// - Static access: `$Class::$prop`, `$Class::method()`
/// - Class constants: `$Class::CONST`
/// - Nullsafe variants: `$obj?->prop`
/// - Any node containing variables at any depth
fn has_variable_node(node: Node) -> bool {
    // Check if this node itself is a variable-bearing or dynamic expression node
    if matches!(
        node.kind(),
        // Direct variable nodes
        "variable_name" | "simple_variable" | "variable" | "complex_variable"
        // Dynamic variables ($$var, ${'expr'})
        | "dynamic_variable_name"
        // Instance access and calls
        | "subscript_expression" | "member_access_expression" | "member_call_expression"
        // Function calls (may contain variables)
        | "function_call_expression"
        // Static/scoped access (may contain variables)
        | "scoped_call_expression" | "scoped_property_access_expression"
        // Class constant access (may have dynamic class name)
        | "class_constant_access_expression"
        // Nullsafe variants
        | "nullsafe_member_access_expression" | "nullsafe_member_call_expression"
    ) {
        return true;
    }

    // Recursively check all children
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)] // tree-sitter child count fits in u32
        if let Some(child) = node.child(i as u32)
            && has_variable_node(child)
        {
            return true;
        }
    }

    false
}

/// Extract content from PHP string literal.
///
/// Handles single-quoted ('...'), double-quoted ("..."), and heredoc strings.
fn extract_php_string_content(string_node: Node, content: &[u8]) -> Option<String> {
    let Ok(text) = string_node.utf8_text(content) else {
        return None;
    };

    let text = text.trim();

    // Strip quotes for simple strings
    if ((text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('\'') && text.ends_with('\'')))
        && text.len() >= 2
    {
        return Some(text[1..text.len() - 1].to_string());
    }

    // For heredoc/nowdoc, return as-is (tree-sitter handles it)
    Some(text.to_string())
}

/// Simplify library path to base name (e.g., "libfoo.so.1" → "libfoo").
fn php_ffi_library_simple_name(library_path: &str) -> String {
    use std::path::Path;

    // Strip directory components first
    let filename = Path::new(library_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(library_path);

    // Handle versioned .so files (libfoo.so.1 → libfoo)
    if let Some(so_pos) = filename.find(".so.") {
        return filename[..so_pos].to_string();
    }

    // Handle standard library and header extensions
    if let Some(dot_pos) = filename.find('.') {
        let extension = &filename[dot_pos + 1..];
        if extension == "so"
            || extension == "dll"
            || extension == "dylib"
            || extension == "h"
            || extension == "hpp"
        {
            return filename[..dot_pos].to_string();
        }
    }

    filename.to_string()
}
