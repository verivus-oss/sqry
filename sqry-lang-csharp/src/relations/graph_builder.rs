//! C# `GraphBuilder` implementation for tier-2 graph coverage.
//!
//! Migrated to use unified `GraphBuildHelper` following Phase 2.
//!
//! # Supported Features
//!
//! - Function/method definitions
//! - Class definitions
//! - Interface definitions
//! - Method definitions (including static methods)
//! - Function calls
//! - Method calls
//! - Static method calls
//! - Async/await detection
//! - Namespace handling
//! - Using directive imports (simple, qualified, static, aliased)
//! - Class inheritance (Inherits edges)
//! - Interface implementation (Implements edges)
//! - Export edges (public and internal types/members)

use std::collections::HashMap;
use std::path::Path;

use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, GraphSnapshot, Language, Span,
};
use tree_sitter::{Node, Tree};

use super::local_scopes;
use super::type_extractor::{extract_all_type_names_from_annotation, extract_type_string};

const DEFAULT_MAX_SCOPE_DEPTH: usize = 6;

/// File-level module name for exports.
/// Distinct from `<module>` to avoid node kind collision in `GraphBuildHelper` cache.
const FILE_MODULE_NAME: &str = "<file_module>";

/// Graph builder for C# files.
#[derive(Debug, Clone, Copy)]
pub struct CSharpGraphBuilder {
    max_scope_depth: usize,
}

impl Default for CSharpGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_MAX_SCOPE_DEPTH,
        }
    }
}

impl CSharpGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for CSharpGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::CSharp);

        // Build AST context for O(1) function lookups
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Map qualified names to NodeIds for call edge creation
        let mut node_map = HashMap::new();

        // Phase 1: Create function/method/class/interface nodes
        for context in ast_graph.contexts() {
            let qualified_name = &context.qualified_name;
            let span = Span::from_bytes(context.span.0, context.span.1);

            let node_id = match context.kind {
                ContextKind::Function { is_async } => {
                    // Use add_function_with_signature for returns: queries
                    helper.add_function_with_signature(
                        qualified_name,
                        Some(span),
                        is_async,
                        false,
                        None, // visibility
                        context.return_type.as_deref(),
                    )
                }
                ContextKind::Method {
                    is_async,
                    is_static,
                } => {
                    // Use add_method_with_signature for returns: queries
                    helper.add_method_with_signature(
                        qualified_name,
                        Some(span),
                        is_async,
                        is_static,
                        None, // visibility
                        context.return_type.as_deref(),
                    )
                }
                ContextKind::Class => helper.add_class(qualified_name, Some(span)),
                ContextKind::Interface => helper.add_interface(qualified_name, Some(span)),
            };
            node_map.insert(qualified_name.clone(), node_id);
        }

        // Build local scope tree for variable reference resolution
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Phase 2: Walk the tree to find calls and OOP edges
        // Track namespace and class context for qualified naming
        let mut namespace_stack = Vec::new();
        let mut class_stack = Vec::new();
        let root = tree.root_node();
        walk_tree_for_edges(
            root,
            content,
            &ast_graph,
            &mut helper,
            &mut node_map,
            &mut namespace_stack,
            &mut class_stack,
            &mut scope_tree,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::CSharp
    }

    fn detect_cross_language_edges(
        &self,
        _snapshot: &GraphSnapshot,
    ) -> GraphResult<Vec<sqry_core::graph::CodeEdge>> {
        // P/Invoke detection is now handled in build_graph() via process_pinvoke_method()
        // This method is required by the trait interface
        Ok(vec![])
    }
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
enum ContextKind {
    Function { is_async: bool },
    Method { is_async: bool, is_static: bool },
    Class,
    Interface,
}

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: (usize, usize),
    kind: ContextKind,
    class_name: Option<String>,
    /// Return type of the method/function (e.g., `Task<User>`, `void`)
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

        let mut walk_context = WalkContext {
            content,
            contexts: &mut contexts,
            node_to_context: &mut node_to_context,
            scope_stack: &mut scope_stack,
            class_stack: &mut class_stack,
            max_depth,
            guard: &mut guard,
        };

        walk_ast(tree.root_node(), &mut walk_context)?;

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

#[allow(clippy::too_many_lines)] // Central traversal; refactor after API stabilization.
/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
struct WalkContext<'a> {
    content: &'a [u8],
    contexts: &'a mut Vec<CallContext>,
    node_to_context: &'a mut HashMap<usize, usize>,
    scope_stack: &'a mut Vec<String>,
    class_stack: &'a mut Vec<String>,
    max_depth: usize,
    guard: &'a mut sqry_core::query::security::RecursionGuard,
}

#[allow(clippy::too_many_lines)]
fn walk_ast(node: Node, context: &mut WalkContext<'_>) -> Result<(), String> {
    context
        .guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    if context.scope_stack.len() > context.max_depth {
        context.guard.exit();
        return Ok(());
    }

    match node.kind() {
        "class_declaration" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| "class_declaration missing name".to_string())?;
            let class_name = name_node
                .utf8_text(context.content)
                .map_err(|_| "failed to read class name".to_string())?;

            // Build qualified class name
            let qualified_class = if context.scope_stack.is_empty() {
                class_name.to_string()
            } else {
                format!("{}.{}", context.scope_stack.join("."), class_name)
            };

            context.class_stack.push(qualified_class.clone());
            context.scope_stack.push(class_name.to_string());

            // Add class context
            let _context_idx = context.contexts.len();
            context.contexts.push(CallContext {
                qualified_name: qualified_class.clone(),
                span: (node.start_byte(), node.end_byte()),
                kind: ContextKind::Class,
                class_name: Some(qualified_class),
                return_type: None,
            });

            // Recurse into class body
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(child, context)?;
                }
            }

            context.class_stack.pop();
            context.scope_stack.pop();
        }
        "interface_declaration" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| "interface_declaration missing name".to_string())?;
            let interface_name = name_node
                .utf8_text(context.content)
                .map_err(|_| "failed to read interface name".to_string())?;

            // Build qualified interface name
            let qualified_interface = if context.scope_stack.is_empty() {
                interface_name.to_string()
            } else {
                format!("{}.{}", context.scope_stack.join("."), interface_name)
            };

            context.class_stack.push(qualified_interface.clone());
            context.scope_stack.push(interface_name.to_string());

            // Add interface context
            let _context_idx = context.contexts.len();
            context.contexts.push(CallContext {
                qualified_name: qualified_interface.clone(),
                span: (node.start_byte(), node.end_byte()),
                kind: ContextKind::Interface,
                class_name: Some(qualified_interface),
                return_type: None,
            });

            // Recurse into interface body
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(child, context)?;
                }
            }

            context.class_stack.pop();
            context.scope_stack.pop();
        }
        "method_declaration" | "constructor_declaration" | "local_function_statement" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| format!("{} missing name", node.kind()).to_string())?;
            let func_name = name_node
                .utf8_text(context.content)
                .map_err(|_| "failed to read function name".to_string())?;

            // Check if async
            let is_async = has_modifier(node, context.content, "async");

            // Check if static method
            let is_static = has_modifier(node, context.content, "static");

            // Extract return type (tree-sitter-c-sharp may use return_type, returns, or type)
            let return_type = node
                .child_by_field_name("return_type")
                .or_else(|| node.child_by_field_name("returns"))
                .or_else(|| node.child_by_field_name("type"))
                .and_then(|type_node| type_node.utf8_text(context.content).ok())
                .map(std::string::ToString::to_string);

            // Build qualified function name
            let qualified_func = if context.scope_stack.is_empty() {
                func_name.to_string()
            } else {
                format!("{}.{}", context.scope_stack.join("."), func_name)
            };

            // Determine if this is a method (inside a class/interface)
            let is_method = !context.class_stack.is_empty();
            let class_name = context.class_stack.last().cloned();

            let kind = if is_method {
                ContextKind::Method {
                    is_async,
                    is_static,
                }
            } else {
                ContextKind::Function { is_async }
            };

            let context_idx = context.contexts.len();
            context.contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: (node.start_byte(), node.end_byte()),
                kind,
                class_name,
                return_type,
            });

            // Associate all descendants with this context
            if let Some(body) = node.child_by_field_name("body") {
                associate_descendants(body, context_idx, context.node_to_context);
            }

            context.scope_stack.push(func_name.to_string());

            // Recurse into function body to find nested functions
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(child, context)?;
                }
            }

            context.scope_stack.pop();
        }
        "namespace_declaration" => {
            // Extract namespace name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(namespace_name) = name_node.utf8_text(context.content)
            {
                context.scope_stack.push(namespace_name.to_string());

                // Recurse into namespace body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_ast(child, context)?;
                    }
                }

                context.scope_stack.pop();
            }
        }
        _ => {
            // Recurse into children for other node types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, context)?;
            }
        }
    }

    context.guard.exit();
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
// Edge Building - calls, imports, inheritance, implements
// ============================================================================

/// Walk the AST tree to create edges (calls, imports, inheritance, implements)
/// Tracks namespace and class context for qualified naming.
#[allow(clippy::too_many_lines)] // Central traversal; refactor after API stabilization.
fn walk_tree_for_edges(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    namespace_stack: &mut Vec<String>,
    class_stack: &mut Vec<String>,
    scope_tree: &mut local_scopes::CSharpScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        "namespace_declaration" => {
            // Track namespace context
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(namespace_name) = name_node.utf8_text(content)
            {
                namespace_stack.push(namespace_name.to_string());

                // Recurse into namespace body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_tree_for_edges(
                            child,
                            content,
                            ast_graph,
                            helper,
                            node_map,
                            namespace_stack,
                            class_stack,
                            scope_tree,
                        )?;
                    }
                }

                namespace_stack.pop();
                return Ok(());
            }
        }
        "class_declaration" => {
            // Track class context and process OOP edges
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                // Build qualified class name
                let qualified_class =
                    build_qualified_name(namespace_stack, class_stack, class_name);
                class_stack.push(class_name.to_string());

                // Process OOP edges with qualified name, passing namespace context for base type resolution
                process_class_declaration(
                    node,
                    content,
                    helper,
                    node_map,
                    &qualified_class,
                    namespace_stack,
                );

                // Emit per-type-parameter Type nodes + where-clause
                // Constraint edges (REQ:R0028 / U19 AC-1, AC-2, AC-3,
                // AC-4, AC-5).
                process_type_parameter_declarations(node, content, &qualified_class, helper);

                // Export class if it has public or internal visibility
                if should_export(node, content)
                    && let Some(class_id) = node_map.get(&qualified_class)
                {
                    export_from_file_module(helper, *class_id);
                }

                // Recurse into class body to handle method exports
                if let Some(body) = node.child_by_field_name("body") {
                    process_class_member_exports(body, content, &qualified_class, helper, node_map);

                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_tree_for_edges(
                            child,
                            content,
                            ast_graph,
                            helper,
                            node_map,
                            namespace_stack,
                            class_stack,
                            scope_tree,
                        )?;
                    }
                }

                class_stack.pop();
                return Ok(());
            }
        }
        "interface_declaration" => {
            // Track interface context and process OOP edges
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(interface_name) = name_node.utf8_text(content)
            {
                // Build qualified interface name
                let qualified_interface =
                    build_qualified_name(namespace_stack, class_stack, interface_name);

                // Process OOP edges with qualified name, passing namespace context for base type resolution
                process_interface_declaration(
                    node,
                    content,
                    helper,
                    node_map,
                    &qualified_interface,
                    namespace_stack,
                );

                // Emit per-type-parameter Type nodes + where-clause
                // Constraint edges (REQ:R0028 / U19 AC-1, AC-2, AC-3,
                // AC-4, AC-5).
                process_type_parameter_declarations(node, content, &qualified_interface, helper);

                // Export interface if it has public or internal visibility
                if should_export(node, content)
                    && let Some(interface_id) = node_map.get(&qualified_interface)
                {
                    export_from_file_module(helper, *interface_id);
                }

                // Process interface method exports and recurse into the
                // interface body so generic interface methods reach the
                // method_declaration arm of this walker (REQ:R0028 / U19
                // AC-1: interface-method type-parameters + where-clause
                // Constraint edges). The interface name is pushed onto
                // class_stack for the duration of the body walk so the
                // method walker builds the correct
                // `<namespace>.<InterfaceName>.<MethodName>.<ParamName>`
                // qualified name.
                if let Some(body) = node.child_by_field_name("body") {
                    process_interface_member_exports(
                        body,
                        content,
                        &qualified_interface,
                        helper,
                        node_map,
                    );

                    class_stack.push(interface_name.to_string());
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_tree_for_edges(
                            child,
                            content,
                            ast_graph,
                            helper,
                            node_map,
                            namespace_stack,
                            class_stack,
                            scope_tree,
                        )?;
                    }
                    class_stack.pop();
                }

                return Ok(());
            }
        }
        "invocation_expression" => {
            process_invocation(node, content, ast_graph, helper, node_map);
        }
        "object_creation_expression" => {
            process_object_creation(node, content, ast_graph, helper, node_map);
        }
        "using_directive" => {
            process_using_directive(node, content, helper);
        }
        "method_declaration" => {
            // Check for P/Invoke (DllImport attribute + extern modifier)
            process_pinvoke_method(node, content, helper, node_map, namespace_stack);

            // Process method parameters and return type for TypeOf edges
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(method_name) = name_node.utf8_text(content)
            {
                // Build qualified method name with namespace and class context
                // Must match format from extract_callable_context (scope_stack.join("."))
                let mut scope_parts = namespace_stack.clone();
                scope_parts.extend(class_stack.iter().cloned());

                let qualified_name = if scope_parts.is_empty() {
                    method_name.to_string()
                } else {
                    format!("{}.{}", scope_parts.join("."), method_name)
                };

                process_method_parameters(node, &qualified_name, content, helper);
                process_method_return_type(node, &qualified_name, content, helper);

                // Emit per-type-parameter Type nodes + where-clause
                // Constraint edges for generic methods (REQ:R0028 /
                // U19 AC-1, AC-2, AC-3, AC-4).
                process_type_parameter_declarations(node, content, &qualified_name, helper);
            }
        }
        "local_declaration_statement" => {
            // Process local variable declarations with TypeOf edges
            process_local_variables(node, content, helper, class_stack);
        }
        "field_declaration" => {
            // Process field declarations with TypeOf edges
            process_field_declaration(node, content, helper, class_stack);
        }
        "property_declaration" => {
            // Process property declarations with TypeOf edges
            process_property_declaration(node, content, helper, class_stack);
        }
        "identifier" => {
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_edges(
            child,
            content,
            ast_graph,
            helper,
            node_map,
            namespace_stack,
            class_stack,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Build a qualified name from namespace and class context.
fn build_qualified_name(namespace_stack: &[String], class_stack: &[String], name: &str) -> String {
    let mut parts = Vec::new();
    parts.extend(namespace_stack.iter().cloned());
    parts.extend(class_stack.iter().cloned());
    parts.push(name.to_string());
    parts.join(".")
}

/// Qualify a type name with namespace context if it's not already qualified.
///
/// For types that are already qualified (contain '.'), returns as-is.
/// For unqualified types in a namespace, prefixes with the namespace.
/// This is a best-effort heuristic - without full import resolution,
/// we assume unqualified types in a namespace are from that namespace.
fn qualify_type_name(type_name: &str, namespace_stack: &[String]) -> String {
    // If already qualified (contains '.'), use as-is
    if type_name.contains('.') {
        return type_name.to_string();
    }

    // If no namespace context, use type name as-is
    if namespace_stack.is_empty() {
        return type_name.to_string();
    }

    // Prefix with namespace
    format!("{}.{}", namespace_stack.join("."), type_name)
}

fn process_invocation(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    let Ok(callee_text) = function_node.utf8_text(content) else {
        return;
    };

    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Handle different invocation patterns:
    // - Simple: methodName()
    // - Member access: obj.methodName()
    // - Static: ClassName.methodName()
    let callee_qualified = if callee_text.contains('.') {
        // Handle member access or static calls
        callee_text.to_string()
    } else if let Some(class_name) = &call_context.class_name {
        // For simple calls inside a class, resolve to ClassName.method
        format!("{class_name}.{callee_text}")
    } else {
        callee_text.to_string()
    };

    // Get or create caller node
    let caller_function_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Get or create callee node
    let target_function_id = *node_map
        .entry(callee_qualified.clone())
        .or_insert_with(|| helper.add_function(&callee_qualified, None, false, false));

    let argument_count = count_call_arguments(node);
    let call_span = Span::from_bytes(node.start_byte(), node.end_byte());
    helper.add_call_edge_full_with_span(
        caller_function_id,
        target_function_id,
        argument_count,
        false,
        vec![call_span],
    );
}

fn process_object_creation(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };

    let Ok(type_name) = type_node.utf8_text(content) else {
        return;
    };

    // Get the caller context
    let Some(call_context) = ast_graph.get_callable_context(node.id()) else {
        return;
    };

    // Treat constructor calls as calls to ClassName.ctor
    let callee_qualified = format!("{type_name}.ctor");

    // Get or create caller node
    let caller_function_id = *node_map
        .entry(call_context.qualified_name.clone())
        .or_insert_with(|| helper.add_function(&call_context.qualified_name, None, false, false));

    // Get or create callee node (constructor)
    let target_function_id = *node_map
        .entry(callee_qualified.clone())
        .or_insert_with(|| helper.add_method(&callee_qualified, None, false, false));

    let argument_count = count_call_arguments(node);
    let call_span = Span::from_bytes(node.start_byte(), node.end_byte());
    helper.add_call_edge_full_with_span(
        caller_function_id,
        target_function_id,
        argument_count,
        false,
        vec![call_span],
    );
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
        u8::MAX
    }
}

// ============================================================================
// Import Processing - using directives
// ============================================================================

/// Process using directive to create Import edges.
///
/// Handles patterns like:
/// - `using System;` - simple namespace import
/// - `using System.Collections.Generic;` - qualified namespace import
/// - `using static System.Math;` - static using (`is_wildcard`: true for all static members)
/// - `using Alias = Namespace.Type;` - aliased using (populate alias field)
///
/// # tree-sitter-c-sharp AST structure
///
/// Simple using:
/// ```text
/// using_directive [0..13] "using System;"
///   using [0..5] "using"
///   identifier [6..12] "System"
///   ; [12..13] ";"
/// ```
///
/// Qualified using:
/// ```text
/// using_directive
///   using
///   qualified_name "System.Collections.Generic"
///     identifier "System"
///     . "."
///     identifier "Collections"
///     . "."
///     identifier "Generic"
///   ;
/// ```
///
/// Static using:
/// ```text
/// using_directive
///   using
///   static
///   qualified_name "System.Math"
///   ;
/// ```
///
/// Aliased using:
/// ```text
/// using_directive [0..21] "using IO = System.IO;"
///   using [0..5] "using"
///   identifier [6..8] "IO"          <- alias
///   = [9..10] "="
///   qualified_name [11..20] "System.IO"  <- target
///   ; [20..21] ";"
/// ```
fn process_using_directive(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Check for static using modifier
    let is_static = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "static");

    // Detect aliased using: pattern is "using <identifier> = <target>;"
    // We look for an "=" child, which indicates aliased using
    let has_equals = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "=");

    // Extract alias and target based on the structure
    let (alias, imported_name) = if has_equals {
        // Aliased using: first identifier is alias, qualified_name/identifier after "=" is target
        extract_aliased_using(node, content)
    } else {
        // Simple or static using: first identifier/qualified_name is the target
        (None, extract_simple_using_target(node, content))
    };

    let Some(imported_name) = imported_name else {
        return;
    };

    // Create module node (represents the current file as the importing entity)
    let module_id = helper.add_module("<file>", None);

    // Create import node for the imported namespace/type
    let span = Span::from_bytes(node.start_byte(), node.end_byte());
    let import_name = if is_static {
        format!("static {imported_name}")
    } else {
        imported_name.clone()
    };
    let imported_id = helper.add_import(&import_name, Some(span));

    // Add import edge with appropriate metadata
    // Static usings are wildcard imports (all static members are accessible)
    // Aliased usings have an alias but are not wildcard
    match (alias.as_deref(), is_static) {
        (Some(alias_str), _) => {
            // Aliased import: using IO = System.IO;
            helper.add_import_edge_full(module_id, imported_id, Some(alias_str), false);
        }
        (None, true) => {
            // Static import: using static System.Math;
            // All static members are imported, so is_wildcard = true
            helper.add_import_edge_full(module_id, imported_id, None, true);
        }
        (None, false) => {
            // Simple import: using System;
            helper.add_import_edge(module_id, imported_id);
        }
    }
}

/// Extract alias and target from an aliased using directive.
///
/// Structure: `using <alias> = <target>;`
/// - The first identifier before "=" is the alias
/// - The `identifier/qualified_name` after "=" is the target
fn extract_aliased_using(node: Node, content: &[u8]) -> (Option<String>, Option<String>) {
    let mut alias: Option<String> = None;
    let mut target: Option<String> = None;
    let mut past_equals = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if kind == "=" {
            past_equals = true;
            continue;
        }

        // Skip keywords and punctuation
        if kind == "using" || kind == "static" || kind == ";" {
            continue;
        }

        if past_equals {
            // After "=" - this is the target (identifier or qualified_name)
            if matches!(kind, "identifier" | "qualified_name") && target.is_none() {
                target = child
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
        } else if kind == "identifier" && alias.is_none() {
            // Before "=" - this is the alias (should be identifier)
            alias = child
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string);
        }
    }

    (alias, target)
}

/// Extract the target from a simple or static using directive.
///
/// Structure: `using [static] <target>;`
/// - The first `identifier/qualified_name` (after optional "static") is the target
fn extract_simple_using_target(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();

        // Skip keywords and punctuation
        if kind == "using" || kind == "static" || kind == ";" || kind == "=" {
            continue;
        }

        // First identifier or qualified_name is the target
        if matches!(kind, "identifier" | "identifier_name" | "qualified_name") {
            return child
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string);
        }
    }

    // Fallback: try to get via field name
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map(std::string::ToString::to_string)
}

// ============================================================================
// OOP Processing - Inheritance and Interface Implementation
// ============================================================================

/// Process class declaration to extract Inherits and Implements edges.
///
/// Handles patterns like:
/// - `class Child : Parent` → Inherits edge
/// - `class Foo : IBar` → Implements edge (I prefix convention)
/// - `class Foo : Parent, IBar, IBaz` → One Inherits + multiple Implements edges
///
/// tree-sitter-c-sharp structure:
/// ```text
/// class_declaration
///   class (keyword)
///   name: identifier
///   [base_list]
///     : (colon)
///     [type_identifier | generic_name | qualified_name]+
///   body: declaration_list
/// ```
fn process_class_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    qualified_class_name: &str,
    namespace_stack: &[String],
) {
    // Get or create class node using qualified name (same as Phase 1)
    let class_id = *node_map
        .entry(qualified_class_name.to_string())
        .or_insert_with(|| helper.add_class(qualified_class_name, None));

    // Find the base_list node (contains inheritance and interface implementation)
    let mut cursor = node.walk();
    let base_list = node
        .children(&mut cursor)
        .find(|child| child.kind() == "base_list");

    let Some(base_list) = base_list else {
        return;
    };

    // Track whether we've seen a class inheritance (first non-interface type)
    let mut first_base_class = true;

    // Process each type in the base list
    let mut base_cursor = base_list.walk();
    for base_child in base_list.children(&mut base_cursor) {
        let base_type_name = match base_child.kind() {
            "identifier" | "identifier_name" | "type_identifier" | "qualified_name" => base_child
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string),
            "generic_name" => {
                // Generic type like List<T> - extract the base type name
                base_child
                    .child_by_field_name("name")
                    .or_else(|| base_child.child(0))
                    .and_then(|n| n.utf8_text(content).ok())
                    .map(std::string::ToString::to_string)
            }
            _ => None,
        };

        let Some(base_name) = base_type_name else {
            continue;
        };

        // Qualify base type name if it's unqualified and we have namespace context
        // If already qualified (contains '.'), use as-is; otherwise prefix with namespace
        let qualified_base_name = qualify_type_name(&base_name, namespace_stack);

        // Determine if this is an interface using base-list position semantics:
        // In C#, if a class has both a base class and interfaces, the base class must come first.
        // We also check the I* naming convention as a secondary heuristic.
        let is_interface = is_interface_name(&base_name);

        if is_interface {
            // Create interface node and add Implements edge
            let interface_id = *node_map
                .entry(qualified_base_name.clone())
                .or_insert_with(|| helper.add_interface(&qualified_base_name, None));
            helper.add_implements_edge(class_id, interface_id);
        } else if first_base_class {
            // First non-interface type is the base class
            let parent_id = *node_map
                .entry(qualified_base_name.clone())
                .or_insert_with(|| helper.add_class(&qualified_base_name, None));
            helper.add_inherits_edge(class_id, parent_id);
            first_base_class = false;
        }
        // Note: C# only allows single class inheritance, so we only process the first class
    }
}

/// Process interface declaration to extract Inherits edges for interface extension.
///
/// Handles patterns like:
/// - `interface IChild : IParent` → Inherits edge
/// - `interface IChild : IParent, IOther` → Multiple Inherits edges
///
/// tree-sitter-c-sharp structure:
/// ```text
/// interface_declaration
///   interface (keyword)
///   name: identifier
///   [base_list]
///     : (colon)
///     [identifier | qualified_name]+
///   body: declaration_list
/// ```
fn process_interface_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    qualified_interface_name: &str,
    namespace_stack: &[String],
) {
    // Get or create interface node using qualified name (same as Phase 1)
    let interface_id = *node_map
        .entry(qualified_interface_name.to_string())
        .or_insert_with(|| helper.add_interface(qualified_interface_name, None));

    // Find the base_list node
    let mut cursor = node.walk();
    let base_list = node
        .children(&mut cursor)
        .find(|child| child.kind() == "base_list");

    let Some(base_list) = base_list else {
        return;
    };

    // Process each parent interface in the base list
    let mut base_cursor = base_list.walk();
    for base_child in base_list.children(&mut base_cursor) {
        let parent_name = match base_child.kind() {
            "identifier" | "identifier_name" | "type_identifier" | "qualified_name" => base_child
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string),
            "generic_name" => base_child
                .child_by_field_name("name")
                .or_else(|| base_child.child(0))
                .and_then(|n| n.utf8_text(content).ok())
                .map(std::string::ToString::to_string),
            _ => None,
        };

        let Some(parent_name) = parent_name else {
            continue;
        };

        // Qualify parent type name if it's unqualified and we have namespace context
        let qualified_parent_name = qualify_type_name(&parent_name, namespace_stack);

        // All base types for interfaces are parent interfaces → Inherits edge
        let parent_id = *node_map
            .entry(qualified_parent_name.clone())
            .or_insert_with(|| helper.add_interface(&qualified_parent_name, None));
        helper.add_inherits_edge(interface_id, parent_id);
    }
}

/// Determine if a type name is an interface based on C# naming convention.
///
/// In C#, interfaces by convention start with 'I' followed by an uppercase letter.
/// Examples: `IDisposable`, `IEnumerable`, `IRepository`
/// Counter-examples: Int32, Image, Item
fn is_interface_name(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() >= 2 {
        // Must start with 'I' followed by uppercase letter
        chars[0] == 'I' && chars[1].is_ascii_uppercase()
    } else {
        false
    }
}

// ============================================================================
// Visibility Detection for Export Edges
// ============================================================================

/// Check if a node has the `public` visibility modifier.
fn is_public(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, content, "public")
}

/// Check if a node has the `internal` visibility modifier.
/// In C#, internal members are accessible within the same assembly and should be exported.
fn is_internal(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, content, "internal")
}

/// Check if a node has the `private` visibility modifier.
fn is_private(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, content, "private")
}

/// Check if a node has the `protected` visibility modifier.
#[allow(dead_code)] // Reserved for potential future use
fn is_protected(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, content, "protected")
}

/// Check if a node has a specific visibility modifier.
fn has_visibility_modifier(node: Node, content: &[u8], modifier: &str) -> bool {
    node.children(&mut node.walk())
        .any(|child| child.kind() == modifier || child.utf8_text(content).unwrap_or("") == modifier)
}

fn has_modifier(node: Node, content: &[u8], modifier: &str) -> bool {
    node.children(&mut node.walk())
        .any(|child| child.kind() == modifier || child.utf8_text(content).unwrap_or("") == modifier)
}

/// Check if a member should be exported (public or internal visibility).
/// In C#, both public and internal types/members are exported:
/// - public: accessible everywhere
/// - internal: accessible within the same assembly
/// - private and protected: NOT exported
fn should_export(node: Node, content: &[u8]) -> bool {
    is_public(node, content) || is_internal(node, content)
}

/// Create an export edge from the file module to the exported node.
fn export_from_file_module(helper: &mut GraphBuildHelper, exported: NodeId) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

/// Process public/internal methods and fields within a class body for export edges.
fn process_class_member_exports(
    body_node: Node,
    content: &[u8],
    class_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "method_declaration" | "constructor_declaration" => {
                // Export method/constructor if it has public or internal visibility
                if should_export(child, content)
                    && let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(method_name) = name_node.utf8_text(content)
                {
                    let qualified_name = format!("{class_qualified_name}.{method_name}");
                    // Get the method from node_map if it exists
                    if let Some(method_id) = node_map.get(&qualified_name) {
                        export_from_file_module(helper, *method_id);
                    }
                } else if should_export(child, content) && child.kind() == "constructor_declaration"
                {
                    // Constructors don't have a name field, use the class name
                    let class_name = class_qualified_name
                        .rsplit('.')
                        .next()
                        .unwrap_or(class_qualified_name);
                    let qualified_name = format!("{class_qualified_name}.{class_name}");
                    if let Some(method_id) = node_map.get(&qualified_name) {
                        export_from_file_module(helper, *method_id);
                    }
                }
            }
            "field_declaration" | "property_declaration" => {
                // Export field/property if it has public or internal
                // visibility. Re-emit through the same Property/Constant
                // discrimination as the typeof pass so the helper's
                // (qualified_name + kind) dedup key collapses to one node
                // (REQ:R0001, R0003, R0004, R0005, R0018, R0019).
                if should_export(child, content) {
                    let is_property = child.kind() == "property_declaration";
                    let is_const = !is_property && has_modifier(child, content, "const");
                    let is_readonly = !is_property && has_modifier(child, content, "readonly");
                    let is_static = is_const || has_modifier(child, content, "static");
                    let visibility = extract_field_visibility(child, content);
                    let get_only = is_property && is_get_only_property(child);
                    let emit_constant = is_const || is_readonly || get_only;

                    // Fields and properties can have multiple declarators
                    let mut field_cursor = child.walk();
                    for field_child in child.children(&mut field_cursor) {
                        if field_child.kind() == "variable_declarator"
                            && let Some(name_node) = field_child.child_by_field_name("name")
                            && let Ok(field_name) = name_node.utf8_text(content)
                        {
                            let qualified_name = format!("{class_qualified_name}.{field_name}");
                            let span =
                                Span::from_bytes(field_child.start_byte(), field_child.end_byte());

                            let field_id = if emit_constant {
                                helper.add_constant_with_static_and_visibility(
                                    &qualified_name,
                                    Some(span),
                                    is_static,
                                    Some(visibility),
                                )
                            } else {
                                helper.add_property_with_static_and_visibility(
                                    &qualified_name,
                                    Some(span),
                                    is_static,
                                    Some(visibility),
                                )
                            };
                            export_from_file_module(helper, field_id);
                        } else if field_child.kind() == "identifier"
                            && let Ok(prop_name) = field_child.utf8_text(content)
                        {
                            // Property name is directly an identifier
                            let qualified_name = format!("{class_qualified_name}.{prop_name}");
                            let span = Span::from_bytes(child.start_byte(), child.end_byte());

                            let prop_id = if emit_constant {
                                helper.add_constant_with_static_and_visibility(
                                    &qualified_name,
                                    Some(span),
                                    is_static,
                                    Some(visibility),
                                )
                            } else {
                                helper.add_property_with_static_and_visibility(
                                    &qualified_name,
                                    Some(span),
                                    is_static,
                                    Some(visibility),
                                )
                            };
                            export_from_file_module(helper, prop_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Process interface method exports.
/// In C#, interface methods are implicitly public unless explicitly marked private (C# 8.0+).
fn process_interface_member_exports(
    body_node: Node,
    content: &[u8],
    interface_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
) {
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        if child.kind() == "method_declaration"
            && !is_private(child, content)
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(method_name) = name_node.utf8_text(content)
        {
            // Interface methods are implicitly public unless explicitly private
            let qualified_name = format!("{interface_qualified_name}.{method_name}");
            // Get the method from node_map if it exists
            if let Some(method_id) = node_map.get(&qualified_name) {
                export_from_file_module(helper, *method_id);
            }
        }
    }
}

// ============================================================================
// P/Invoke (FFI) Processing
// ============================================================================

/// Process method declaration to detect P/Invoke (Platform Invocation Services).
///
/// P/Invoke pattern in C#:
/// ```csharp
/// [DllImport("user32.dll")]
/// static extern int MessageBox(IntPtr hWnd, string text, string caption, uint type);
///
/// [DllImport("kernel32.dll", CharSet = CharSet.Auto)]
/// static extern bool Beep(uint frequency, uint duration);
/// ```
///
/// tree-sitter-c-sharp structure:
/// ```text
/// method_declaration
///   attribute_list
///     attribute
///       name: identifier ("DllImport")
///       argument_list
///         attribute_argument
///           expression: string_literal ("user32.dll")
///   modifier: static
///   modifier: extern
///   return_type: predefined_type
///   name: identifier
///   parameter_list
/// ```
fn process_pinvoke_method(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    node_map: &mut HashMap<String, NodeId>,
    namespace_stack: &[String],
) {
    // Check for extern modifier
    let has_extern = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "extern");

    if !has_extern {
        return;
    }

    // Look for DllImport attribute
    let mut cursor = node.walk();
    let attribute_list = node
        .children(&mut cursor)
        .find(|child| child.kind() == "attribute_list");

    let Some(attribute_list) = attribute_list else {
        return;
    };

    // Find DllImport attribute and extract library name
    let (dll_name, calling_convention) = extract_dllimport_info(attribute_list, content);

    let Some(dll_name) = dll_name else {
        return;
    };

    // Extract method name
    let method_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map(std::string::ToString::to_string);

    let Some(method_name) = method_name else {
        return;
    };

    // Build qualified method name
    let qualified_method = if namespace_stack.is_empty() {
        method_name.clone()
    } else {
        format!("{}.{}", namespace_stack.join("."), method_name)
    };

    // Get or create method node (caller - the C# method declaration)
    let method_span = Span::from_bytes(node.start_byte(), node.end_byte());
    let method_id = *node_map
        .entry(qualified_method.clone())
        .or_insert_with(|| helper.add_method(&qualified_method, Some(method_span), false, true));

    // Create FFI function node (the native function in the DLL)
    let ffi_func_name = format!("ffi::{dll_name}::{method_name}");
    let ffi_func_id = *node_map
        .entry(ffi_func_name.clone())
        .or_insert_with(|| helper.add_function(&ffi_func_name, None, false, false));

    // Determine FFI convention from CallingConvention parameter
    let convention = match calling_convention.as_deref() {
        Some("CallingConvention.Cdecl" | "Cdecl") => FfiConvention::Cdecl,
        Some("CallingConvention.FastCall" | "FastCall") => FfiConvention::Fastcall,
        // Default Windows convention is StdCall for P/Invoke
        _ => FfiConvention::Stdcall,
    };

    // Add FfiCall edge
    helper.add_ffi_edge(method_id, ffi_func_id, convention);
}

/// Extract `DllImport` attribute information (library name and calling convention).
///
/// Returns (`dll_name`, `calling_convention`) tuple.
fn extract_dllimport_info(
    attribute_list: Node,
    content: &[u8],
) -> (Option<String>, Option<String>) {
    let mut dll_name = None;
    let mut calling_convention = None;

    let mut list_cursor = attribute_list.walk();
    for attr_child in attribute_list.children(&mut list_cursor) {
        if attr_child.kind() != "attribute" {
            continue;
        }

        // Check if this is DllImport attribute
        let attr_name = attr_child
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok());

        let is_dllimport = attr_name.is_some_and(|name| {
            name == "DllImport" || name == "System.Runtime.InteropServices.DllImport"
        });

        if !is_dllimport {
            continue;
        }

        // Extract arguments
        let mut attr_cursor = attr_child.walk();
        let arg_list = attr_child
            .children(&mut attr_cursor)
            .find(|child| child.kind() == "attribute_argument_list");

        let Some(arg_list) = arg_list else {
            continue;
        };

        let mut arg_cursor = arg_list.walk();
        for arg in arg_list.children(&mut arg_cursor) {
            if arg.kind() != "attribute_argument" {
                continue;
            }

            // Check for named argument (CallingConvention = ...)
            if let Some(name_node) = arg.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                if name == "CallingConvention"
                    && let Some(expr) = arg.child_by_field_name("expression")
                    && let Ok(value) = expr.utf8_text(content)
                {
                    calling_convention = Some(value.to_string());
                }
                continue;
            }

            // Positional argument - first one is the DLL name
            if dll_name.is_none() {
                // Find the string literal
                let expr = arg.child_by_field_name("expression").or_else(|| {
                    // Sometimes the string is a direct child
                    let mut c = arg.walk();
                    arg.children(&mut c)
                        .find(|child| child.kind() == "string_literal")
                });

                if let Some(expr) = expr
                    && let Ok(text) = expr.utf8_text(content)
                {
                    // Remove quotes from string literal
                    let trimmed = text.trim();
                    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                        || (trimmed.starts_with('@') && trimmed.len() > 2)
                    {
                        let start = if trimmed.starts_with('@') { 2 } else { 1 };
                        dll_name = Some(trimmed[start..trimmed.len() - 1].to_string());
                    } else {
                        dll_name = Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    (dll_name, calling_convention)
}

// ============================================================================
// TypeOf and Reference Edge Processing
// ============================================================================

/// Process local variable declarations to create `TypeOf` and Reference edges.
///
/// Handles patterns like:
/// - `int x = 5;`
/// - `string name = "test";`
/// - `int a = 1, b = 2;` (multiple declarators)
/// - `List<User> users = new List<User>();` (generics - extract base type)
/// - `int? count = null;` (nullable - extract base type)
/// - `int[] numbers = new int[5];` (arrays - extract base type)
///
/// tree-sitter-c-sharp structure:
/// ```text
/// local_declaration_statement
///   variable_declaration
///     type: predefined_type | identifier | generic_name | nullable_type | array_type
///     variable_declarator
///       name: identifier
///       initializer: [expression]
/// ```
fn process_local_variables(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    _class_stack: &[String],
) {
    // Get variable_declaration child
    let mut cursor = node.walk();
    let var_decl = node
        .children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration");

    let Some(var_decl) = var_decl else {
        return;
    };

    // Extract type
    let type_node = var_decl.child_by_field_name("type");
    let Some(type_node) = type_node else {
        return;
    };

    // Extract full type signature for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        return;
    };

    // Extract all type names for Reference edges
    let all_type_names = extract_all_type_names_from_annotation(type_node, content);

    // Process all variable declarators (may be multiple: int a = 1, b = 2;)
    let mut var_cursor = var_decl.walk();
    for child in var_decl.children(&mut var_cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(var_name) = name_node.utf8_text(content)
        {
            let span = Span::from_bytes(child.start_byte(), child.end_byte());

            // Create variable node
            let var_id = helper.add_variable(var_name, Some(span));

            // Create TypeOf edge with full type signature and Variable context
            let type_id = helper.add_type(&type_text, None);
            helper.add_typeof_edge_with_context(
                var_id,
                type_id,
                Some(TypeOfContext::Variable),
                None,
                Some(var_name),
            );

            // Create Reference edges for all nested types
            for type_name in &all_type_names {
                let ref_type_id = helper.add_type(type_name, None);
                helper.add_reference_edge(var_id, ref_type_id);
            }
        }
    }
}

/// Process field declarations to create `TypeOf` and Reference edges.
///
/// Handles patterns like:
/// - `private UserRepository repository;`
/// - `private int age;`
/// - `public List<User> users;` (generics - extract base type)
///
/// tree-sitter-c-sharp structure:
/// ```text
/// field_declaration
///   modifiers: [...]
///   declaration:
///     type: predefined_type | identifier | generic_name
///     variable_declarator
///       name: identifier
///       initializer: [expression]
/// ```
fn process_field_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_stack: &[String],
) {
    // Get the declaration child (variable_declaration)
    let decl_node = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "variable_declaration");

    let Some(decl_node) = decl_node else {
        return;
    };

    // Extract type
    let type_node = decl_node.child_by_field_name("type");
    let Some(type_node) = type_node else {
        return;
    };

    // Extract full type signature for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        return;
    };

    // Extract all type names for Reference edges
    let all_type_names = extract_all_type_names_from_annotation(type_node, content);

    // Get the containing class name
    let class_name = class_stack.last().map_or("", String::as_str);

    // Process all variable declarators
    let mut var_cursor = decl_node.walk();
    for child in decl_node.children(&mut var_cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(field_name) = name_node.utf8_text(content)
        {
            // Build qualified field name
            let qualified_name = if class_name.is_empty() {
                field_name.to_string()
            } else {
                format!("{class_name}.{field_name}")
            };

            let span = Span::from_bytes(child.start_byte(), child.end_byte());

            // Branch on `readonly` / `const` / `static` modifiers (REQ:R0001,
            // R0003, R0004, R0005, R0018, R0019). C# semantics:
            //   - `const` is implicitly static and immutable -> Constant.
            //   - `readonly` -> Constant (immutable post-construction).
            //   - otherwise mutable field -> Property.
            // Visibility comes from the accessibility modifier; the C# default
            // for a class member is `private` (REQ:R0023). `internal` is a
            // first-class accessibility level and is passed through verbatim.
            let is_const = has_modifier(node, content, "const");
            let is_readonly = has_modifier(node, content, "readonly");
            let is_static = is_const || has_modifier(node, content, "static");
            let visibility = extract_field_visibility(node, content);

            let field_id = if is_const || is_readonly {
                helper.add_constant_with_static_and_visibility(
                    &qualified_name,
                    Some(span),
                    is_static,
                    Some(visibility),
                )
            } else {
                helper.add_property_with_static_and_visibility(
                    &qualified_name,
                    Some(span),
                    is_static,
                    Some(visibility),
                )
            };

            // Create TypeOf edge with full type signature, Field context, and
            // the bare field name (REQ:R0023 / AC-4). Cross-language byte-exact
            // `field:<name>` planner queries depend on the unqualified form.
            let type_id = helper.add_type(&type_text, None);
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(field_name),
            );

            // Create Reference edges for all nested types
            for type_name in &all_type_names {
                let ref_type_id = helper.add_type(type_name, None);
                helper.add_reference_edge(field_id, ref_type_id);
            }
        }
    }
}

/// Process property declarations to create `TypeOf` and Reference edges.
///
/// Handles patterns like:
/// - `public int Age { get; set; }` (auto-property)
/// - `public string Name { get; }` (read-only property)
/// - `public bool IsAdult { get { return age >= 18; } }` (computed property)
/// - `public List<User> Users { get; set; }` (generic property)
///
/// tree-sitter-c-sharp structure:
/// ```text
/// property_declaration
///   modifiers: [...]
///   type: predefined_type | identifier | generic_name
///   name: identifier
///   accessor_list: { get; set; }
/// ```
fn process_property_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_stack: &[String],
) {
    // Extract type
    let type_node = node.child_by_field_name("type");
    let Some(type_node) = type_node else {
        return;
    };

    // Extract full type signature for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        return;
    };

    // Extract all type names for Reference edges
    let all_type_names = extract_all_type_names_from_annotation(type_node, content);

    // Extract property name
    let name_node = node.child_by_field_name("name");
    let Some(name_node) = name_node else {
        return;
    };

    let Ok(prop_name) = name_node.utf8_text(content) else {
        return;
    };

    // Get the containing class name
    let class_name = class_stack.last().map_or("", String::as_str);

    // Build qualified property name
    let qualified_name = if class_name.is_empty() {
        prop_name.to_string()
    } else {
        format!("{class_name}.{prop_name}")
    };

    let span = Span::from_bytes(node.start_byte(), node.end_byte());

    // Branch on modifiers + accessor shape (REQ:R0001, R0003, R0004, R0005,
    // R0018, R0019). C# property semantics:
    //   - `const` is illegal on properties (rejected by the compiler).
    //   - `readonly` is illegal on properties.
    //   - A get-only auto-property (`{ get; }` with no setter and no body) is
    //     externally immutable -> Constant.
    //   - Computed get-only properties (`{ get { ... } }`) and
    //     expression-bodied properties (`=> expr`) are computed callable
    //     surfaces; their backing storage (if any) is mutable from the
    //     implementation side, so they emit Property, not Constant.
    //   - Property with any setter (or `init`) -> Property.
    // Visibility comes from the accessibility modifier; the default for a
    // class member is `private` (REQ:R0023). `internal` is passed through.
    let is_static = has_modifier(node, content, "static");
    let visibility = extract_field_visibility(node, content);
    let get_only = is_get_only_property(node);

    let prop_id = if get_only {
        helper.add_constant_with_static_and_visibility(
            &qualified_name,
            Some(span),
            is_static,
            Some(visibility),
        )
    } else {
        helper.add_property_with_static_and_visibility(
            &qualified_name,
            Some(span),
            is_static,
            Some(visibility),
        )
    };

    // Create TypeOf edge with full type signature, Field context, and the bare
    // property name (REQ:R0023 / AC-4). Cross-language byte-exact
    // `field:<name>` planner queries depend on the unqualified form.
    // (Properties are treated as fields for TypeOf context.)
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        prop_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(prop_name),
    );

    // Create Reference edges for all nested types
    for type_name in &all_type_names {
        let ref_type_id = helper.add_type(type_name, None);
        helper.add_reference_edge(prop_id, ref_type_id);
    }
}

/// Extract the accessibility modifier from a field/property declaration.
///
/// C# class members default to `private` when no accessibility modifier is
/// present (REQ:R0023). `internal` is a first-class accessibility level and is
/// passed through verbatim. `protected internal` (more accessible than either
/// alone) is normalized to the canonical two-word form.
fn extract_field_visibility(node: Node, content: &[u8]) -> &'static str {
    let has_protected = has_modifier(node, content, "protected");
    let has_internal = has_modifier(node, content, "internal");
    if has_protected && has_internal {
        "protected internal"
    } else if has_modifier(node, content, "public") {
        "public"
    } else if has_protected {
        "protected"
    } else if has_internal {
        "internal"
    } else if has_modifier(node, content, "private") {
        "private"
    } else {
        // C# class-member default.
        "private"
    }
}

/// Determine whether a property declaration is a true get-only
/// auto-implemented property (`{ get; }`), as distinct from a computed
/// getter or an expression-bodied property.
///
/// Returns `true` only for:
///   - An `accessor_list` containing exactly one `get` accessor with NO body
///     (i.e. `get;`) and no `set;` / `init;` accessor. This is C#'s
///     auto-implemented immutable property, externally indistinguishable from
///     a `readonly` field.
///
/// Returns `false` for:
///   - `{ get { ... } }` — computed body. The backing storage (if any) is
///     mutable from the implementation's perspective; the surface is a
///     callable, not an immutable value.
///   - `T X => expr;` (`arrow_expression_clause`) — expression-bodied
///     properties are computed getters, same rationale as above.
///   - Any property with a `set` or `init` accessor.
///   - Any property without an `accessor_list` (defensive default).
fn is_get_only_property(node: Node) -> bool {
    // Expression-bodied property: `T X => expr;` is a computed getter, NOT
    // an auto-implemented immutable property. Reject before scanning the
    // accessor list.
    if node
        .children(&mut node.walk())
        .any(|child| child.kind() == "arrow_expression_clause")
    {
        return false;
    }

    let Some(accessor_list) = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "accessor_list")
    else {
        // No accessor list and no expression body -> not classifiable as a
        // get-only auto-property; default to mutable.
        return false;
    };

    let mut has_auto_get = false;
    let mut has_set_like = false;
    for accessor in accessor_list.children(&mut accessor_list.walk()) {
        if accessor.kind() != "accessor_declaration" {
            continue;
        }

        // Determine the accessor keyword and whether it has a body. An
        // auto-implemented accessor terminates immediately with `;` (no
        // `block` and no `arrow_expression_clause` child). A computed
        // accessor carries a `block` (`get { ... }`) or
        // `arrow_expression_clause` (`get => expr`).
        let mut keyword: Option<&str> = None;
        let mut has_body = false;
        for tok in accessor.children(&mut accessor.walk()) {
            match tok.kind() {
                "get" | "set" | "init" => keyword = Some(tok.kind()),
                "block" | "arrow_expression_clause" => has_body = true,
                _ => {}
            }
        }

        match keyword {
            Some("get") => {
                if has_body {
                    // Computed getter — disqualifies the property from being
                    // a get-only auto-property.
                    return false;
                }
                has_auto_get = true;
            }
            Some("set" | "init") => has_set_like = true,
            _ => {}
        }
    }

    has_auto_get && !has_set_like
}
/// Process method parameters to create `TypeOf` and Reference edges.
///
/// Handles patterns like:
/// - `void Method(int count, string name)`
/// - `User Process(Repository repo, List<Item> items)`
///
/// tree-sitter-c-sharp structure:
/// ```text
/// method_declaration
///   return_type: predefined_type | identifier | generic_name
///   name: identifier
///   parameter_list
///     parameter
///       type: predefined_type | identifier | generic_name
///       name: identifier
/// ```
fn process_method_parameters(
    node: Node,
    _method_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Get parameter_list
    let Some(param_list) = node.child_by_field_name("parameter_list") else {
        return;
    };

    // Process each parameter
    let mut cursor = param_list.walk();
    let mut param_index: u16 = 0;

    // Iterate through children - tree-sitter-c-sharp may use various node kinds for parameters
    for child in param_list.children(&mut cursor) {
        // Skip punctuation nodes (parentheses, commas)
        if !child.is_named() {
            continue;
        }
        // Most parameter-like nodes should be processed
        {
            // Extract parameter name
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let Ok(param_name) = name_node.utf8_text(content) else {
                continue;
            };

            // Extract parameter type
            let Some(type_node) = child.child_by_field_name("type") else {
                continue;
            };

            // Extract full type signature for TypeOf edge
            let Some(type_text) = extract_type_string(type_node, content) else {
                continue;
            };

            // Extract all type names for Reference edges
            let all_type_names = extract_all_type_names_from_annotation(type_node, content);

            // Create parameter variable node
            let param_span = Span::from_bytes(child.start_byte(), child.end_byte());
            let param_id = helper.add_variable(param_name, Some(param_span));

            // Create TypeOf edge with full type signature and Parameter context
            let type_id = helper.add_type(&type_text, None);
            helper.add_typeof_edge_with_context(
                param_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(param_index),
                Some(param_name),
            );

            // Create Reference edges for all nested types
            for type_name in &all_type_names {
                let ref_type_id = helper.add_type(type_name, None);
                helper.add_reference_edge(param_id, ref_type_id);
            }

            param_index += 1;
        }
    }
}

/// Process method return type to create `TypeOf` and Reference edges.
///
/// Handles patterns like:
/// - `string GetName()`
/// - `List<User> GetUsers()`
/// - `Task<Result> ProcessAsync()`
///
/// tree-sitter-c-sharp structure:
/// ```text
/// method_declaration
///   return_type: predefined_type | identifier | generic_name
///   name: identifier
///   parameter_list
/// ```
fn process_method_return_type(
    node: Node,
    method_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Get return_type - C# uses "return_type", "returns", or "type" field
    let return_type_node = node
        .child_by_field_name("return_type")
        .or_else(|| node.child_by_field_name("returns"))
        .or_else(|| node.child_by_field_name("type"));

    let Some(return_type_node) = return_type_node else {
        return;
    };

    // Skip void return types
    if let Ok(type_text) = return_type_node.utf8_text(content)
        && type_text.trim() == "void"
    {
        return;
    }

    // Extract full type signature for TypeOf edge
    let Some(type_text) = extract_type_string(return_type_node, content) else {
        return;
    };

    // Extract all type names for Reference edges
    let all_type_names = extract_all_type_names_from_annotation(return_type_node, content);

    // Get or find the method node ID
    let method_span = Span::from_bytes(node.start_byte(), node.end_byte());
    let method_id = helper.add_method(method_name, Some(method_span), false, false);

    // Create TypeOf edge with full type signature and Return context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        method_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0), // Return always has index 0
        Some(method_name),
    );

    // Create Reference edges for all nested types
    for type_name in &all_type_names {
        let ref_type_id = helper.add_type(type_name, None);
        helper.add_reference_edge(method_id, ref_type_id);
    }
}

// ============================================================================
// Generic Type Parameter Emission (REQ:R0028 / U19 — C2_GEN_TP_CSHARP)
// ============================================================================

/// Emit per-type-parameter `Type` nodes and `TypeOf{Constraint}` edges
/// for a C# generic declaration (class, interface, or method).
///
/// Handles all three shapes that carry a `type_parameter_list` child on
/// tree-sitter-c-sharp declarations:
///
/// 1. `class_declaration`     → `<namespace>.<ClassName>.<ParamName>`
/// 2. `interface_declaration` → `<namespace>.<InterfaceName>.<ParamName>`
/// 3. `method_declaration`    → `<namespace>.<ClassName>.<MethodName>.<ParamName>`
///
/// Tree-sitter-c-sharp grammar shape:
///
/// ```text
/// type_parameter_list:
///   '<' commaSep1(type_parameter) '>'
/// type_parameter:
///   repeat(attribute_list)
///   optional('in' | 'out')      // variance — emit base node, attribute deferred
///   name: identifier
/// type_parameter_constraints_clause:
///   'where' identifier ':' commaSep1(type_parameter_constraint)
/// type_parameter_constraint:
///   class_constraint  ('class')
///   | struct_constraint ('struct')
///   | unmanaged ('unmanaged')
///   | notnull ('notnull')
///   | constructor_constraint ('new()')
///   | type: <named-type>
/// ```
///
/// In the live tree-sitter-c-sharp 0.23.x grammar `class`/`struct`/
/// `unmanaged`/`notnull` arrive as **unnamed** keyword children of the
/// `type_parameter_constraint` node (not as separate named rules).
/// `constructor_constraint` is named. The bound type itself comes
/// through the `type` field on `type_parameter_constraint` and may be
/// an `identifier`, `qualified_name`, `generic_name`, or
/// `nullable_type` etc. — handled by `extract_constraint_base_type_name`.
///
/// AC-5: variance modifiers (`in`/`out`) are recorded on the parameter
/// only via the base Type-node emission. The variance attribute itself
/// is deferred to a future `EdgeKind` extension.
fn process_type_parameter_declarations(
    decl_node: Node,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    // type_parameter_list is an unnamed-position child of the
    // declaration (no `type_parameters` field name in c-sharp grammar).
    let mut decl_cursor = decl_node.walk();
    let Some(params_node) = decl_node
        .children(&mut decl_cursor)
        .find(|c| c.kind() == "type_parameter_list")
    else {
        return;
    };

    // Map parameter-name -> NodeId so where-clauses can target the
    // right parameter Type node by identifier match.
    let mut param_ids: HashMap<String, sqry_core::graph::unified::node::NodeId> = HashMap::new();

    let mut params_cursor = params_node.walk();
    for param_node in params_node.children(&mut params_cursor) {
        if param_node.kind() != "type_parameter" {
            continue;
        }

        // Parameter name lives under the `name` field.
        let Some(name_node) = param_node.child_by_field_name("name") else {
            continue;
        };
        let Ok(param_name) = name_node.utf8_text(content) else {
            continue;
        };

        let qualified_param = format!("{parent_qualified_name}.{param_name}");
        let span = Span::from_bytes(name_node.start_byte(), name_node.end_byte());
        // AC-2: span anchored on the parameter identifier so
        // "Find Definition" / hover navigation lands on the declaration
        // site rather than the synthetic `(0, 0)` sentinel.
        let param_id = helper.add_type(&qualified_param, Some(span));
        param_ids.insert(param_name.to_string(), param_id);
    }

    // AC-3 / AC-4: walk every `type_parameter_constraints_clause`
    // sibling on the same declaration node and emit one
    // `TypeOf{Constraint}` edge per constraint entry.
    let mut clause_cursor = decl_node.walk();
    for clause_node in decl_node.children(&mut clause_cursor) {
        if clause_node.kind() != "type_parameter_constraints_clause" {
            continue;
        }
        emit_type_parameter_constraint_clause(clause_node, content, &param_ids, helper);
    }
}

/// Emit `TypeOf{Constraint}` edges for one
/// `type_parameter_constraints_clause`.
///
/// The clause shape is:
/// ```text
/// type_parameter_constraints_clause:
///   'where' identifier ':' commaSep1(type_parameter_constraint)
/// ```
///
/// The leading `identifier` child (after the `where` keyword) names the
/// type parameter being constrained. We match it against the parameter
/// IDs collected during the `type_parameter_list` walk; an unknown
/// parameter is skipped silently (defensive — the grammar guarantees
/// it matches one of the declared parameters).
fn emit_type_parameter_constraint_clause(
    clause_node: Node,
    content: &[u8],
    param_ids: &HashMap<String, sqry_core::graph::unified::node::NodeId>,
    helper: &mut GraphBuildHelper,
) {
    // The first named child is the parameter identifier (the `where`
    // keyword and `:` are unnamed). Subsequent named children are
    // `type_parameter_constraint` nodes.
    let mut cursor = clause_node.walk();
    let mut named_children = clause_node
        .children(&mut cursor)
        .filter(tree_sitter::Node::is_named);

    let Some(param_id_node) = named_children.next() else {
        return;
    };
    if param_id_node.kind() != "identifier" {
        return;
    }
    let Ok(param_name) = param_id_node.utf8_text(content) else {
        return;
    };
    let Some(&param_id) = param_ids.get(param_name) else {
        // Constraint clause references a parameter we did not emit —
        // skip silently. (Should not happen for well-formed C#.)
        return;
    };

    for constraint_node in named_children {
        if constraint_node.kind() != "type_parameter_constraint" {
            continue;
        }
        let Some(constraint_target_name) = extract_constraint_target_name(constraint_node, content)
        else {
            continue;
        };
        let constraint_id = helper.add_type(&constraint_target_name, None);
        helper.add_typeof_edge_with_context(
            param_id,
            constraint_id,
            Some(TypeOfContext::Constraint),
            None,
            None,
        );
    }
}

/// Extract the constraint target identifier from a single
/// `type_parameter_constraint` node.
///
/// Constraint shapes recognised:
///
/// - **Synthetic keywords**: `class`, `struct`, `unmanaged`, `notnull`
///   appear as **unnamed** keyword children → return the keyword text
///   verbatim. These become interned synthetic Type nodes named
///   exactly `class` / `struct` / `unmanaged` / `notnull`.
/// - **`constructor_constraint`** (`new()`) is a named child → return
///   the literal string `"new()"` as the synthetic target name.
/// - **Named-type bound** (`where T : IFoo`, `where T : Comparable<T>`):
///   the `type` field carries an `identifier`, `qualified_name`,
///   `generic_name`, or `nullable_type` — return the base name with
///   any generic argument list stripped (so `Comparable<T>` becomes
///   `Comparable`). Cross-file unification (Phase 4c-prime) collapses
///   the synthetic stub into the canonical declaration when one exists.
fn extract_constraint_target_name(constraint_node: Node, content: &[u8]) -> Option<String> {
    // Iterate all children: first check for the named `constructor_constraint`,
    // then the `type` field, then fall back to unnamed keyword tokens.
    let mut cursor = constraint_node.walk();
    let mut had_named_constructor = false;
    let mut keyword_token: Option<&'static str> = None;

    for child in constraint_node.children(&mut cursor) {
        match child.kind() {
            "constructor_constraint" => {
                had_named_constructor = true;
            }
            "class" if !child.is_named() => {
                keyword_token = Some("class");
            }
            "struct" if !child.is_named() => {
                keyword_token = Some("struct");
            }
            "unmanaged" if !child.is_named() => {
                keyword_token = Some("unmanaged");
            }
            "notnull" if !child.is_named() => {
                keyword_token = Some("notnull");
            }
            _ => {}
        }
    }

    if had_named_constructor {
        return Some("new()".to_string());
    }
    if let Some(kw) = keyword_token {
        return Some(kw.to_string());
    }

    // Otherwise the bound is a named type carried by the `type` field
    // (identifier, qualified_name, generic_name, nullable_type, ...).
    let type_node = constraint_node.child_by_field_name("type")?;
    Some(extract_constraint_base_type_name(type_node, content))
}

/// Extract the base name from a constraint bound type, stripping any
/// generic type-argument list.
///
/// `where T : Comparable<T>` should produce a `Comparable` Type node
/// (not `Comparable<T>`), matching the Java implementation's behaviour
/// in `extract_bound_type_base_name`. This lets cross-file unification
/// collapse the synthetic stub into the canonical `Comparable`
/// declaration when one exists in the same workspace.
fn extract_constraint_base_type_name(type_node: Node, content: &[u8]) -> String {
    match type_node.kind() {
        "generic_name" => {
            // `generic_name` has shape: identifier type_argument_list.
            // Take the leading identifier as the base name.
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if matches!(child.kind(), "identifier" | "qualified_name") {
                    return extract_constraint_base_type_name(child, content);
                }
            }
            type_node.utf8_text(content).unwrap_or_default().to_string()
        }
        "qualified_name" => {
            // `qualified_name` may end in a `generic_name` (e.g.
            // `System.Collections.Generic.IEnumerable<T>`). Recurse into
            // the trailing child to strip the type-argument list while
            // preserving the dotted prefix.
            let mut cursor = type_node.walk();
            let children: Vec<_> = type_node
                .children(&mut cursor)
                .filter(tree_sitter::Node::is_named)
                .collect();
            // Last named child is the leaf: identifier or generic_name.
            if let Some(last) = children.last()
                && last.kind() == "generic_name"
            {
                // Reconstruct dotted prefix + stripped leaf base.
                let prefix_segments: Vec<String> = children
                    .iter()
                    .take(children.len() - 1)
                    .filter_map(|c| c.utf8_text(content).ok().map(str::to_string))
                    .collect();
                let leaf_base = extract_constraint_base_type_name(*last, content);
                if prefix_segments.is_empty() {
                    return leaf_base;
                }
                return format!("{}.{}", prefix_segments.join("."), leaf_base);
            }
            type_node.utf8_text(content).unwrap_or_default().to_string()
        }
        "nullable_type" => {
            // `T?` — strip the trailing `?` by recursing into the inner
            // type (first named child).
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.is_named() {
                    return extract_constraint_base_type_name(child, content);
                }
            }
            type_node.utf8_text(content).unwrap_or_default().to_string()
        }
        _ => type_node.utf8_text(content).unwrap_or_default().to_string(),
    }
}
