//! Dart `GraphBuilder` implementation for code graph construction.
//!
//! Extracts Dart-specific relationships:
//! - Class definitions
//! - Function and method definitions
//! - Widget class definitions (`StatelessWidget`, `StatefulWidget`, etc.)
//! - Widget build hierarchies (Flutter)
//! - `MethodChannel` platform invocations
//! - Function and method call edges
//! - Async call detection

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Position, Span,
    unified::{GraphBuildHelper, StagingGraph},
};
use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::relations::type_extractor::{
    extract_all_type_names_from_dart_type, extract_type_string, is_type_node,
};

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// Dart-specific `GraphBuilder` implementation.
///
/// Performs multi-pass analysis:
/// 1. Build AST graph with callable contexts for O(1) lookups
/// 2. Extract class and method definitions
/// 3. Extract function/method call edges with proper caller tracking
/// 4. Handle async calls with `await` detection
/// 5. Handle cascade notation (`..method()`)
#[derive(Debug, Default, Clone, Copy)]
pub struct DartGraphBuilder {
    max_scope_depth: usize,
}

impl DartGraphBuilder {
    /// Create a new Dart `GraphBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }

    /// Create a new Dart `GraphBuilder` with custom scope depth.
    #[must_use]
    pub fn with_max_scope_depth(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for DartGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Dart);

        // Phase 1: Build AST context graph for O(1) callable lookups
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth);

        // Phase 2: Create function/method nodes for all callables
        for context in ast_graph.contexts() {
            let visibility = visibility_for_qualified_name(&context.qualified_name);
            let node_id = if context.is_method {
                helper.add_method_with_visibility(
                    &context.qualified_name,
                    Some(context.span),
                    context.is_async,
                    context.is_static,
                    Some(visibility),
                )
            } else {
                helper.add_function_with_visibility(
                    &context.qualified_name,
                    Some(context.span),
                    context.is_async,
                    false,
                    Some(visibility),
                )
            };

            // Export public module-level functions (not methods, not nested functions)
            // In Dart, symbols are public unless they start with underscore
            // A function is module-level if it's not nested in another function
            let is_nested = ast_graph.is_nested_function(context);
            if !context.is_method && !is_nested && is_public_name(&context.qualified_name) {
                export_from_file_module(&mut helper, node_id);
            }
        }

        // Phase 3: Walk the tree to find call edges and other relationships
        walk_tree_for_edges(tree.root_node(), content, &ast_graph, &mut helper);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Dart
    }
}

// ================================
// ASTGraph: In-memory function context index
// ================================

/// Callable context for tracking function/method scope during call edge detection.
#[derive(Debug, Clone)]
struct CallableContext {
    /// Qualified name (e.g., "MyClass.myMethod" or "myFunction")
    qualified_name: String,
    /// Byte span of the callable for containment lookups (start, end)
    byte_span: (usize, usize),
    /// Proper span with row/column info for node creation
    span: Span,
    /// Nesting depth for resolving ambiguity
    depth: usize,
    /// Whether this is a method (inside a class)
    is_method: bool,
    /// Whether this is async
    is_async: bool,
    /// Whether this is static
    is_static: bool,
}

/// AST graph for O(1) callable context lookups.
#[derive(Debug)]
struct ASTGraph {
    contexts: Vec<CallableContext>,
}

impl ASTGraph {
    /// Build the AST graph from a tree-sitter tree.
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Self {
        let mut contexts = Vec::new();
        let mut class_stack: Vec<String> = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .expect("Failed to load recursion limits");
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .expect("Invalid file_ops_depth configuration");
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .expect("Failed to create recursion guard");

        if let Err(_e) = extract_dart_contexts(
            tree.root_node(),
            content,
            &mut contexts,
            &mut class_stack,
            0,
            max_depth,
            &mut guard,
        ) {
            // // eprintln!("Warning: Dart AST traversal hit recursion limit: {e}");
        }

        Self { contexts }
    }

    /// Get all callable contexts.
    fn contexts(&self) -> &[CallableContext] {
        &self.contexts
    }

    /// Find the enclosing callable context for a given byte position.
    fn find_enclosing(&self, byte_pos: usize) -> Option<&CallableContext> {
        self.contexts
            .iter()
            .filter(|ctx| byte_pos >= ctx.byte_span.0 && byte_pos < ctx.byte_span.1)
            .max_by_key(|ctx| ctx.depth)
    }

    /// Check if a function is nested inside another function.
    ///
    /// A function is nested if its byte span is fully contained within another
    /// non-method function's byte span.
    fn is_nested_function(&self, context: &CallableContext) -> bool {
        if context.is_method {
            return false; // Methods are not considered nested functions
        }

        self.contexts.iter().any(|other| {
            // Check if this context is contained within another function (not method)
            !other.is_method
                && other.byte_span.0 < context.byte_span.0
                && other.byte_span.1 > context.byte_span.1
        })
    }
}

/// Recursively extract callable contexts from Dart AST.
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_dart_contexts(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallableContext>,
    class_stack: &mut Vec<String>,
    depth: usize,
    max_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    guard.enter()?;

    if depth > max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        "class_definition" => {
            // Extract class name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let class_name = class_name.trim().to_string();
                class_stack.push(class_name);

                // Process class body
                if let Some(body) = node.child_by_field_name("body") {
                    extract_dart_contexts(
                        body,
                        content,
                        contexts,
                        class_stack,
                        depth + 1,
                        max_depth,
                        guard,
                    )?;
                }

                class_stack.pop();
                guard.exit();
                return Ok(());
            }
        }
        "class_body" => {
            // Process class members
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_dart_contexts(
                    child,
                    content,
                    contexts,
                    class_stack,
                    depth,
                    max_depth,
                    guard,
                )?;
            }
            guard.exit();
            return Ok(());
        }
        "lambda_expression" => {
            // Top-level or nested function: lambda_expression contains function_signature + function_body
            if let Some(context) = extract_callable_context(node, content, class_stack, depth) {
                contexts.push(context);
            }
        }
        "class_member_definition" => {
            // Method definition: class_member_definition contains method_signature + function_body
            if let Some(context) = extract_method_context(node, content, class_stack, depth) {
                contexts.push(context);
            }
        }
        "function_signature" => {
            // Standalone function signature (could be top-level)
            // Only process if parent is NOT lambda_expression (to avoid duplicates)
            let parent_kind = node.parent().map(|p| p.kind());
            if parent_kind != Some("lambda_expression")
                && parent_kind != Some("method_signature")
                && let Some(context) = extract_standalone_function_context(node, content, depth)
            {
                contexts.push(context);
            }
        }
        _ => {}
    }

    // Continue traversing children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_dart_contexts(
            child,
            content,
            contexts,
            class_stack,
            depth,
            max_depth,
            guard,
        )?;
    }

    guard.exit();
    Ok(())
}

/// Extract callable context from a `lambda_expression` node.
fn extract_callable_context(
    node: Node,
    content: &[u8],
    class_stack: &[String],
    depth: usize,
) -> Option<CallableContext> {
    // Find function_signature child to get the name
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_signature"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
        {
            let name = name.trim().to_string();
            let is_async = has_async_modifier(&node);
            let is_static = has_static_modifier(&node);

            let (qualified_name, is_method) = if let Some(class) = class_stack.last() {
                (format!("{class}.{name}"), true)
            } else {
                (name, false)
            };

            return Some(CallableContext {
                qualified_name,
                byte_span: (node.start_byte(), node.end_byte()),
                span: node_to_span(&node),
                depth,
                is_method,
                is_async,
                is_static,
            });
        }
    }
    None
}

/// Extract method context from a `class_member_definition` node.
fn extract_method_context(
    node: Node,
    content: &[u8],
    class_stack: &[String],
    depth: usize,
) -> Option<CallableContext> {
    // class_member_definition -> method_signature -> function_signature -> identifier
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "method_signature" {
            let mut sig_cursor = child.walk();
            for sig_child in child.children(&mut sig_cursor) {
                if sig_child.kind() == "function_signature"
                    && let Some(name_node) = sig_child.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    let is_async = has_async_modifier(&node);
                    let is_static = has_static_modifier(&node);

                    let qualified_name = if let Some(class) = class_stack.last() {
                        format!("{class}.{name}")
                    } else {
                        name
                    };

                    return Some(CallableContext {
                        qualified_name,
                        byte_span: (node.start_byte(), node.end_byte()),
                        span: node_to_span(&node),
                        depth,
                        is_method: true,
                        is_async,
                        is_static,
                    });
                }
            }
        }
    }
    None
}

/// Extract context from a standalone `function_signature` (top-level functions).
fn extract_standalone_function_context(
    node: Node,
    content: &[u8],
    depth: usize,
) -> Option<CallableContext> {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(content)
    {
        let name = name.trim().to_string();

        // Check parent for async/static modifiers
        let parent = node.parent();
        let is_async = parent.as_ref().is_some_and(|p| has_async_modifier(p));
        let is_static = parent.as_ref().is_some_and(|p| has_static_modifier(p));

        // Get the span from the parent if available (to include the function body)
        let (byte_span, span) = if let Some(ref p) = parent {
            ((p.start_byte(), p.end_byte()), node_to_span(p))
        } else {
            ((node.start_byte(), node.end_byte()), node_to_span(&node))
        };

        return Some(CallableContext {
            qualified_name: name,
            byte_span,
            span,
            depth,
            is_method: false,
            is_async,
            is_static,
        });
    }
    None
}

/// Check if a function node has an async modifier.
fn has_async_modifier(node: &Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_body" || child.kind() == "function_expression_body" {
            let mut body_cursor = child.walk();
            for body_child in child.children(&mut body_cursor) {
                if body_child.kind() == "async" || body_child.kind() == "async*" {
                    return true;
                }
                // Only check the first few children
                if body_child.is_named()
                    && body_child.kind() != "async"
                    && body_child.kind() != "async*"
                {
                    break;
                }
            }
        }
    }
    false
}

/// Check if a node has a static modifier.
fn has_static_modifier(node: &Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "static" {
            return true;
        }
    }
    false
}

// ================================
// Export Helpers
// ================================

/// Check if a name is public (does not start with underscore).
///
/// In Dart, names starting with an underscore are private to the library.
/// Public names do not start with an underscore.
fn visibility_for_qualified_name(name: &str) -> &'static str {
    let short_name = name.rsplit('.').next().unwrap_or(name);
    if short_name.starts_with('_') {
        "private"
    } else {
        "public"
    }
}

fn is_public_name(name: &str) -> bool {
    visibility_for_qualified_name(name) == "public"
}

/// Check if a node is at module level (top-level in the file).
///
/// In tree-sitter Dart AST, module-level items are direct children of the root "program" node.
/// We check if the parent is "program" to determine module-level scope.
fn is_module_level(node: Node<'_>) -> bool {
    // Walk up the tree to find the immediate container
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "program" => return true,
            "class_definition" | "function_signature" | "lambda_expression" => return false,
            _ => current = parent.parent(),
        }
    }
    false
}

/// Export a symbol from the file module.
///
/// File-level module name for exports.
/// Distinct from function contexts to avoid conflicts.
const FILE_MODULE_NAME: &str = "<file_module>";

fn export_from_file_module(
    helper: &mut GraphBuildHelper,
    exported: sqry_core::graph::unified::node::NodeId,
) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

// ================================
// Edge Building
// ================================

/// Process an import statement and create import edges.
///
/// Dart imports look like:
/// - `import 'dart:async';` - Dart SDK imports
/// - `import 'package:flutter/material.dart';` - Package imports
/// - `import 'package:http/http.dart' as http;` - Aliased imports
/// - `import 'models/user.dart' show User;` - Selective imports
fn process_import(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Verify this is actually an import (not an export)
    if !is_import_statement(node, content) {
        return;
    }

    // Extract the import URI from the library_import child
    let Some(import_uri) = extract_import_uri(node, content) else {
        return;
    };

    // Extract optional alias (e.g., 'as http')
    let alias = extract_import_alias(node, content);

    // Get the span for this import node
    let span = Some(node_to_span(&node));

    // Create module node for the current file
    let file_path = helper.file_path().to_string();
    let from_id = helper.add_module(&file_path, None);

    // Create import node for the imported module
    let to_id = helper.add_import(&import_uri, span);

    // Add import edge with optional alias
    if let Some(alias_str) = alias {
        helper.add_import_edge_full(from_id, to_id, Some(&alias_str), false);
    } else {
        helper.add_import_edge(from_id, to_id);
    }
}

/// Check if an `import_or_export` node is actually an import statement.
fn is_import_statement(node: Node, content: &[u8]) -> bool {
    // Get the text and check if it starts with "import"
    if let Ok(text) = node.utf8_text(content) {
        text.trim().starts_with("import")
    } else {
        false
    }
}

/// Extract the import URI from an `import_or_export` node.
///
/// Returns the string literal content without quotes.
fn extract_import_uri(node: Node, content: &[u8]) -> Option<String> {
    // Look for library_import child node
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "library_import" {
            // Extract the URI from the library_import node
            return extract_uri_from_library_import(child, content);
        }
    }
    None
}

/// Extract URI from a `library_import` node.
fn extract_uri_from_library_import(node: Node, content: &[u8]) -> Option<String> {
    // Get the full text and extract the string literal
    let text = node.utf8_text(content).ok()?;

    // Find the first string literal (single or double quoted)
    if let Some(start) = text.find('\'')
        && let Some(end) = text[start + 1..].find('\'')
    {
        return Some(text[start + 1..start + 1 + end].to_string());
    } else if let Some(start) = text.find('"')
        && let Some(end) = text[start + 1..].find('"')
    {
        return Some(text[start + 1..start + 1 + end].to_string());
    }

    None
}

/// Extract the alias from an import statement (e.g., 'as http').
///
/// Returns the identifier after 'as' keyword.
fn extract_import_alias(node: Node, content: &[u8]) -> Option<String> {
    extract_alias_recursive(node, content)
}

/// Recursively search for 'as identifier' pattern in the AST.
fn extract_alias_recursive(node: Node, content: &[u8]) -> Option<String> {
    let mut found_as = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();

        // Look for 'as' keyword
        if kind == "as" {
            found_as = true;
        } else if found_as && kind == "identifier" {
            // Found the alias identifier
            if let Ok(alias) = child.utf8_text(content) {
                return Some(alias.trim().to_string());
            }
        }
    }

    // If not found at this level, search children recursively
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(alias) = extract_alias_recursive(child, content) {
            return Some(alias);
        }
    }

    None
}

/// Process a top-level variable declaration and create export edges for public variables.
///
/// Dart variables can be declared with:
/// - `final Type name = value;` - Final variable (runtime constant)
/// - `const Type name = value;` - Compile-time constant
/// - `var name = value;` - Type-inferred variable
/// - `late Type name;` - Late-initialized variable
///
/// Variables starting with underscore are private to the library.
fn process_variable_declaration(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Find initialized_variable_definition child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "initialized_variable_definition"
            || child.kind() == "declared_identifier"
        {
            // Extract variable name
            if let Some(name_node) = child.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim();
                let span = Some(node_to_span(&node));

                // Determine if this is a constant based on const_builtin
                let is_const = has_const_modifier(&child);
                let visibility = visibility_for_qualified_name(name);
                let var_id = if is_const {
                    helper.add_constant_with_visibility(name, span, Some(visibility))
                } else {
                    helper.add_node_with_visibility(
                        name,
                        span,
                        NodeKind::Variable,
                        Some(visibility),
                    )
                };

                // Only export public variables (not starting with underscore)
                if is_public_name(name) {
                    export_from_file_module(helper, var_id);
                }
            }
        }
    }
}

/// Check if a variable definition has the const modifier.
fn has_const_modifier(node: &Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "const_builtin" {
            return true;
        }
    }
    false
}

/// Find the enclosing class name for a node (if any).
///
/// Walks up the AST tree to find a `class_definition` ancestor and extracts its name.
/// Returns None if the node is not inside a class.
fn find_enclosing_class(node: Node, content: &[u8]) -> Option<String> {
    let mut current = node.parent()?;

    loop {
        if current.kind() == "class_definition" {
            if let Some(name_node) = current.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                return Some(class_name.trim().to_string());
            }
            return None;
        }

        current = current.parent()?;
    }
}

// ============================================================================
// TypeOf and Reference Edge Processing
// ============================================================================

/// Process `TypeOf` and Reference edges for function parameters.
///
/// Extracts type annotations from function/method parameters and creates:
/// - `TypeOf` edges with Parameter context and index
/// - Reference edges for each nested type name
///
/// # Arguments
///
/// * `func_node` - The function/method definition node
/// * `func_name` - Qualified function/method name
/// * `class_name` - Optional class name if this is a method
/// * `helper` - `GraphBuildHelper` for adding edges
/// * `content` - Source file bytes
fn process_function_parameters_typeof(
    func_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Find formal_parameter_list node (Dart uses this for function parameters)
    // Structure: function_signature → formal_parameter_list → formal_parameter nodes
    let mut cursor = func_node.walk();
    let params_node = func_node.children(&mut cursor).find(|child| {
        child.kind() == "formal_parameter_list" || child.kind() == "function_signature" // might need to go deeper
    });

    let params_node = if let Some(node) = params_node {
        if node.kind() == "function_signature" {
            // Look inside function_signature for formal_parameter_list
            let mut sig_cursor = node.walk();
            node.children(&mut sig_cursor)
                .find(|child| child.kind() == "formal_parameter_list")
        } else {
            Some(node)
        }
    } else {
        None
    };

    let Some(params_node) = params_node else {
        return Ok(()); // No parameters
    };

    // Process each parameter
    let mut param_index = 0u8;
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        if child.kind() == "formal_parameter" || child.kind() == "normal_parameter" {
            process_parameter_typeof(child, func_name, class_name, param_index, helper, content)?;
            param_index = param_index.saturating_add(1);
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a single parameter.
///
/// # Arguments
///
/// * `param_node` - The parameter node
/// * `func_name` - Qualified function/method name
/// * `class_name` - Optional class name if this is a method
/// * `param_index` - Zero-based parameter index
/// * `helper` - `GraphBuildHelper` for adding edges
/// * `content` - Source file bytes
#[allow(clippy::unnecessary_wraps)]
fn process_parameter_typeof(
    param_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    param_index: u8,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Extract parameter name
    // Dart parameter structure: type_identifier → identifier (name)
    let name_node = param_node.child_by_field_name("name");
    let Some(name_node) = name_node else {
        return Ok(()); // No name (shouldn't happen)
    };

    let param_name = name_node.utf8_text(content).ok();
    let Some(param_name) = param_name else {
        return Ok(());
    };

    // Find type annotation
    // Parameters have: type_identifier/predefined_type (type) → identifier (name)
    let mut type_node: Option<Node> = None;
    let mut cursor = param_node.walk();
    for child in param_node.children(&mut cursor) {
        if is_type_node(child.kind()) {
            type_node = Some(child);
            break;
        }
    }

    let Some(type_node) = type_node else {
        return Ok(()); // No type annotation (dynamic or inferred)
    };

    // Extract full type text for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        return Ok(());
    };

    // Get or create the function node
    let func_id = if class_name.is_some() {
        helper.ensure_method(func_name, None, false, false)
    } else {
        helper.ensure_callee(
            func_name,
            node_to_span(&param_node),
            CalleeKindHint::Function,
        )
    };

    // Create TypeOf edge with Parameter context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(u16::from(param_index)),
        Some(param_name),
    );

    // Create Reference edges for all nested types
    let referenced_types = extract_all_type_names_from_dart_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a variable or field declaration.
///
/// Extracts type annotations from Dart variable declarations and creates:
/// - `TypeOf` edges with the full type signature
/// - Reference edges for each nested type name
///
/// Handles:
/// - Top-level variables: `final String name = "value";`
/// - Class fields: `class User { final int age; }`
/// - Local variables: `var count = 0;`
/// - Type-inferred declarations (skips if no explicit type)
///
/// # Arguments
///
/// * `node` - The variable/field declaration node
/// * `helper` - `GraphBuildHelper` for adding nodes and edges
/// * `content` - Source file bytes
/// * `owner_class` - Optional class name if this is a field (None for top-level variables)
///
/// # Returns
///
/// `GraphResult<()>` - Ok if successful, Err if critical error occurs
#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)]
#[allow(clippy::unnecessary_wraps)]
fn process_variable_typeof_edges(
    node: Node,
    helper: &mut GraphBuildHelper,
    content: &[u8],
    owner_class: Option<&str>,
) -> GraphResult<()> {
    // Find the variable name and type annotation
    // Dart variable declarations have structure:
    // - initialized_variable_definition or declared_identifier
    //   - type annotation (type_identifier, predefined_type, etc.)
    //   - name (identifier)
    //   - assignment/initializer (optional)

    let mut cursor = node.walk();

    // For class fields (declaration node), the structure is different:
    // declaration → type_identifier + initialized_identifier_list (direct children)
    // For top-level vars: declaration → initialized_variable_definition → ...

    // Check if this is a class field (direct type_identifier child)
    let has_direct_type_child = node
        .children(&mut node.walk())
        .any(|c| is_type_node(c.kind()));

    if let (true, Some(owner_class_name)) = (has_direct_type_child, owner_class) {
        // Class field: declaration has type_identifier + initialized_identifier_list as direct children

        // Extract type
        let type_node = node
            .children(&mut node.walk())
            .find(|c| is_type_node(c.kind()));
        let Some(type_node) = type_node else {
            return Ok(());
        };

        // Extract field name from initialized_identifier_list
        let id_list = node
            .children(&mut node.walk())
            .find(|c| c.kind() == "initialized_identifier_list");
        let Some(id_list) = id_list else {
            return Ok(());
        };

        // Get the initialized_identifier from the list
        let init_id = id_list
            .children(&mut id_list.walk())
            .find(|c| c.kind() == "initialized_identifier");
        let Some(init_id) = init_id else {
            return Ok(());
        };

        // The identifier is a child of initialized_identifier
        let identifier = init_id
            .children(&mut init_id.walk())
            .find(|c| c.kind() == "identifier");
        let Some(identifier) = identifier else {
            return Ok(());
        };

        let variable_name = identifier.utf8_text(content).ok();
        let Some(variable_name) = variable_name else {
            return Ok(());
        };

        // Extract type text
        let type_text = extract_type_string(type_node, content);
        let Some(type_text) = type_text else {
            return Ok(());
        };

        // Build qualified name
        let qualified_name = format!("{owner_class_name}.{variable_name}");

        // Create field node
        let visibility = visibility_for_qualified_name(&qualified_name);
        let is_static = has_static_modifier(&node);
        let var_id = helper.add_property_with_static_and_visibility(
            &qualified_name,
            Some(node_to_span(&node)),
            is_static,
            Some(visibility),
        );

        // Create TypeOf edge
        let type_id = helper.add_type(&type_text, None);
        helper.add_typeof_edge_with_context(
            var_id,
            type_id,
            Some(TypeOfContext::Field),
            None,
            Some(variable_name),
        );

        // Create Reference edges
        let referenced_types = extract_all_type_names_from_dart_type(type_node, content);
        for ref_type_name in referenced_types {
            let ref_type_id = helper.add_type(&ref_type_name, None);
            helper.add_reference_edge(var_id, ref_type_id);
        }

        return Ok(());
    }

    for child in node.children(&mut cursor) {
        if child.kind() == "initialized_variable_definition"
            || child.kind() == "declared_identifier"
        {
            // Extract variable name
            let name_node = child.child_by_field_name("name");
            let Some(name_node) = name_node else {
                continue;
            };

            let variable_name = name_node.utf8_text(content).ok();
            let Some(variable_name) = variable_name else {
                continue;
            };

            // Find type annotation - it's a sibling of the name
            // For generic types like List<String>, we need to find the parent node that
            // encompasses both the type_identifier and type_arguments
            let mut type_node: Option<Node> = None;
            let mut inner_cursor = child.walk();
            for inner_child in child.children(&mut inner_cursor) {
                if is_type_node(inner_child.kind()) {
                    // Check if this node has type_arguments as a sibling (generic type)
                    // If so, we need to find a common parent
                    let has_type_args_sibling = child
                        .children(&mut child.walk())
                        .any(|c| c.kind() == "type_arguments");

                    if has_type_args_sibling {
                        // For generic types, we want the text from the type_identifier through type_arguments
                        // We'll use the child (parent of both) as the type node since utf8_text
                        // will get all the text
                        type_node = Some(child);
                    } else {
                        // Simple type without generics
                        type_node = Some(inner_child);
                    }
                    break;
                }
            }

            let Some(type_node) = type_node else {
                // No type annotation - could be type-inferred (var x = ...)
                continue;
            };

            // Extract full type text for TypeOf edge
            // For generic types, we need to extract just the type part, not the whole declaration
            let type_text = if type_node.kind() == "initialized_variable_definition"
                || type_node.kind() == "declared_identifier"
            {
                // We're using the whole declaration node, extract just the type part
                // Find the text span from first type node to last type_arguments
                let mut first_type_byte = None;
                let mut last_type_byte = None;

                let mut cursor = type_node.walk();
                for c in type_node.children(&mut cursor) {
                    if is_type_node(c.kind()) || c.kind() == "type_arguments" {
                        if first_type_byte.is_none() {
                            first_type_byte = Some(c.start_byte());
                        }
                        last_type_byte = Some(c.end_byte());
                    }
                }

                if let (Some(start), Some(mut end)) = (first_type_byte, last_type_byte) {
                    // Check for nullable marker '?' immediately after the type
                    if let Some(next_byte) = content.get(end)
                        && *next_byte == b'?'
                    {
                        end += 1;
                    }

                    content
                        .get(start..end)
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .map(std::string::ToString::to_string)
                } else {
                    None
                }
            } else {
                // For simple types, also check for nullable marker
                let mut type_text = extract_type_string(type_node, content);
                if let Some(text) = type_text.as_ref() {
                    // Check if there's a '?' right after the type node
                    let end_byte = type_node.end_byte();
                    if let Some(next_byte) = content.get(end_byte)
                        && *next_byte == b'?'
                        && !text.ends_with('?')
                    {
                        type_text = Some(format!("{text}?"));
                    }
                }
                type_text
            };

            let Some(type_text) = type_text else {
                continue;
            };

            // Determine qualified name (prepend class name if field)
            let qualified_name = if let Some(class_name) = owner_class {
                format!("{class_name}.{variable_name}")
            } else {
                variable_name.to_string()
            };

            // Determine if this is a const (Constant node) or variable
            let is_const = has_const_modifier(&child);
            let is_static = has_static_modifier(&node);

            // Get or create the variable/field/constant node
            let visibility = visibility_for_qualified_name(&qualified_name);
            let var_id = if is_const {
                // Use ensure for constants
                helper.add_constant_with_visibility(
                    &qualified_name,
                    Some(node_to_span(&node)),
                    Some(visibility),
                )
            } else if owner_class.is_some() {
                // Class field - ensure Property node exists
                helper.add_property_with_static_and_visibility(
                    &qualified_name,
                    Some(node_to_span(&node)),
                    is_static,
                    Some(visibility),
                )
            } else {
                // Top-level variable - should already exist from process_variable_declaration
                // But use add_ to ensure it exists
                helper.add_node_with_visibility(
                    &qualified_name,
                    Some(node_to_span(&node)),
                    NodeKind::Variable,
                    Some(visibility),
                )
            };

            // Create TypeOf edge
            let type_id = helper.add_type(&type_text, None);
            let context = if owner_class.is_some() {
                TypeOfContext::Field
            } else {
                TypeOfContext::Variable
            };
            helper.add_typeof_edge_with_context(
                var_id,
                type_id,
                Some(context),
                None,
                Some(variable_name),
            );

            // Create Reference edges for all nested types
            // Note: For generic types, type_node might be the whole declaration,
            // so we need to re-find the actual type nodes
            let mut referenced_types = Vec::new();
            let mut cursor = child.walk();
            for c in child.children(&mut cursor) {
                if is_type_node(c.kind()) {
                    referenced_types.extend(extract_all_type_names_from_dart_type(c, content));
                }
                if c.kind() == "type_arguments" {
                    referenced_types.extend(extract_all_type_names_from_dart_type(c, content));
                }
            }

            for ref_type_name in referenced_types {
                let ref_type_id = helper.add_type(&ref_type_name, None);
                helper.add_reference_edge(var_id, ref_type_id);
            }
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for function return type.
///
/// Extracts return type annotation from function/method signature and creates:
/// - `TypeOf` edge with Return context (index 0)
/// - Reference edges for each nested type name
///
/// # Arguments
///
/// * `func_node` - The function/method definition node
/// * `func_name` - Qualified function/method name
/// * `class_name` - Optional class name if this is a method
/// * `helper` - `GraphBuildHelper` for adding edges
/// * `content` - Source file bytes
#[allow(clippy::unnecessary_wraps)]
fn process_function_return_typeof(
    func_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Find function_signature node
    // Dart return types appear before the function name in function_signature
    // Structure: function_signature → type_identifier (return type) → identifier (name) → formal_parameter_list
    let mut cursor = func_node.walk();
    let sig_node = func_node
        .children(&mut cursor)
        .find(|child| child.kind() == "function_signature");

    let Some(sig_node) = sig_node else {
        return Ok(()); // No signature found
    };

    // Find return type (first type node before the function name)
    let mut type_node: Option<Node> = None;
    let mut cursor = sig_node.walk();
    for child in sig_node.children(&mut cursor) {
        if is_type_node(child.kind()) {
            type_node = Some(child);
            break; // Return type is the first type node
        }
    }

    let Some(type_node) = type_node else {
        return Ok(()); // No return type annotation (void or inferred)
    };

    // Extract full type text for TypeOf edge
    // For generic types like Future<String>, we need to include type_arguments
    let type_text = {
        // Check if there's a type_arguments sibling
        let has_type_args = sig_node
            .children(&mut sig_node.walk())
            .any(|c| c.kind() == "type_arguments");

        if has_type_args {
            // Extract text from type_identifier through type_arguments
            let mut first_byte = type_node.start_byte();
            let mut last_byte = type_node.end_byte();

            // Find all type-related nodes and get the span
            let mut cursor = sig_node.walk();
            for child in sig_node.children(&mut cursor) {
                if is_type_node(child.kind()) || child.kind() == "type_arguments" {
                    first_byte = first_byte.min(child.start_byte());
                    last_byte = last_byte.max(child.end_byte());
                }
                // Stop at function name or parameters
                if child.kind() == "identifier" || child.kind() == "formal_parameter_list" {
                    break;
                }
            }

            content
                .get(first_byte..last_byte)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(std::string::ToString::to_string)
        } else {
            extract_type_string(type_node, content)
        }
    };

    let Some(type_text) = type_text else {
        return Ok(());
    };

    // Get or create the function node
    let func_id = if class_name.is_some() {
        helper.ensure_method(func_name, None, false, false)
    } else {
        helper.ensure_callee(
            func_name,
            node_to_span(&func_node),
            CalleeKindHint::Function,
        )
    };

    // Create TypeOf edge with Return context (index 0)
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0), // Return type index is always 0
        None,
    );

    // Create Reference edges for all nested types
    let referenced_types = extract_all_type_names_from_dart_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Walk the AST tree and build edges using `GraphBuildHelper`.
#[allow(clippy::too_many_lines)]
fn walk_tree_for_edges(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    match node.kind() {
        "import_or_export" => {
            // Handle import statements
            process_import(node, content, helper);
        }
        "class_definition" => {
            // Extract class node
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim();
                let span = Some(node_to_span(&node));
                let visibility = visibility_for_qualified_name(name);
                let class_id = helper.add_class_with_visibility(name, span, Some(visibility));

                // Export public top-level classes
                // In Dart, classes are public unless they start with underscore
                if is_module_level(node) && is_public_name(name) {
                    export_from_file_module(helper, class_id);
                }
            }
        }
        "enum_declaration" => {
            // Extract enum node
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim();
                let span = Some(node_to_span(&node));
                let visibility = visibility_for_qualified_name(name);
                let enum_id = helper.add_enum_with_visibility(name, span, Some(visibility));

                // Export public top-level enums
                // In Dart, enums are public unless they start with underscore
                if is_module_level(node) && is_public_name(name) {
                    export_from_file_module(helper, enum_id);
                }
            }
        }
        "mixin_declaration" => {
            // Extract mixin node (mixins are treated as traits)
            // Note: mixin_declaration doesn't have a "name" field, so we look for identifier child
            let mut cursor = node.walk();
            let name_node = node
                .children(&mut cursor)
                .find(|child| child.kind() == "identifier");

            if let Some(name_node) = name_node
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim();
                let span = Some(node_to_span(&node));
                let visibility = visibility_for_qualified_name(name);
                let mixin_id = helper.add_node_with_visibility(
                    name,
                    span,
                    sqry_core::graph::unified::node::NodeKind::Trait,
                    Some(visibility),
                );

                // Export public top-level mixins
                // In Dart, mixins are public unless they start with underscore
                if is_module_level(node) && is_public_name(name) {
                    export_from_file_module(helper, mixin_id);
                }
            }
        }
        "local_variable_declaration" | "declaration" => {
            // Handle variable declarations (both top-level and class fields)
            if is_module_level(node) {
                // Top-level variable
                if node.kind() == "local_variable_declaration" {
                    process_variable_declaration(node, content, helper);
                }
                let _ = process_variable_typeof_edges(node, helper, content, None);
            } else {
                // Could be a class field - extract class name from ancestors
                let class_name = find_enclosing_class(node, content);
                if class_name.is_some() {
                    // This is a field declaration - process TypeOf edges
                    // Note: Fields are not exported, so we don't call process_variable_declaration
                    let _ =
                        process_variable_typeof_edges(node, helper, content, class_name.as_deref());
                }
            }
        }
        "member_access" => {
            // Check for FFI patterns first
            if is_ffi_dynamic_library_call(&node, content) {
                detect_library_loading(node, content, ast_graph, helper);
            } else if is_lookup_chain(&node, content) {
                detect_lookup_chain(node, content, ast_graph, helper);
            } else if is_lookup_function_call(&node, content) {
                detect_lookup_function(node, content, ast_graph, helper);
            } else if is_function_call(&node) {
                // Original: Check if this is a function call (has argument_part in selector)
                process_function_call(node, content, ast_graph, helper);
            }
        }
        "cascade_section" => {
            // Handle cascade notation: object..method1()..method2()
            process_cascade_call(node, content, ast_graph, helper);
        }
        "lambda_expression" => {
            // Process top-level or nested function
            // Extract function name and process parameters/return type
            if let Some(sig_node) = node
                .children(&mut node.walk())
                .find(|child| child.kind() == "function_signature")
                && let Some(name_node) = sig_node.child_by_field_name("name")
                && let Ok(func_name) = name_node.utf8_text(content)
            {
                let func_name = func_name.trim();

                // Check if this is a method (inside a class)
                let class_name = find_enclosing_class(node, content);
                let qualified_name = if let Some(ref class) = class_name {
                    format!("{class}.{func_name}")
                } else {
                    func_name.to_string()
                };

                // Process parameters and return type
                let _ = process_function_parameters_typeof(
                    node,
                    &qualified_name,
                    class_name.as_deref(),
                    helper,
                    content,
                );
                let _ = process_function_return_typeof(
                    node,
                    &qualified_name,
                    class_name.as_deref(),
                    helper,
                    content,
                );
            }
        }
        "class_member_definition" => {
            // Process class method
            // Extract method name and process parameters/return type
            // Structure: class_member_definition → method_signature → function_signature
            if let Some(method_sig) = node
                .children(&mut node.walk())
                .find(|child| child.kind() == "method_signature")
                && let Some(func_sig) = method_sig
                    .children(&mut method_sig.walk())
                    .find(|child| child.kind() == "function_signature")
                && let Some(name_node) = func_sig.child_by_field_name("name")
                && let Ok(method_name) = name_node.utf8_text(content)
            {
                let method_name = method_name.trim();

                // Get class name
                let class_name = find_enclosing_class(node, content);
                let qualified_name = if let Some(ref class) = class_name {
                    format!("{class}.{method_name}")
                } else {
                    method_name.to_string()
                };

                // Process parameters and return type
                // Pass the method_signature node which contains function_signature
                let _ = process_function_parameters_typeof(
                    method_sig,
                    &qualified_name,
                    class_name.as_deref(),
                    helper,
                    content,
                );
                let _ = process_function_return_typeof(
                    method_sig,
                    &qualified_name,
                    class_name.as_deref(),
                    helper,
                    content,
                );
            }
        }
        "external_declaration" => {
            // Check for FFI annotations (@Native or @FfiNative) on external declarations
            detect_ffi_annotation(node, content, helper);
        }
        "marker_annotation" => {
            // Handle @Native/@FfiNative annotations (when grammar doesn't create external_declaration)
            // Due to tree-sitter-dart grammar limitations, @Native<T>() with empty parens
            // creates a broken AST where the function is in an ERROR node
            if let Ok(text) = node.utf8_text(content)
                && is_ffi_annotation(text)
            {
                // Check if 'external' keyword is present in siblings
                // In malformed AST, 'external' appears as initialized_identifier_list
                let mut has_external = false;
                let mut current_sibling = node.next_named_sibling();
                while let Some(sibling) = current_sibling {
                    if sibling.kind() == "initialized_identifier_list"
                        && let Ok(id_text) = sibling.utf8_text(content)
                        && id_text.split_whitespace().any(|tok| tok == "external")
                    {
                        has_external = true;
                    }
                    current_sibling = sibling.next_named_sibling();
                }

                // Only process if external keyword found
                if !has_external {
                    return;
                }

                // Look for function_signature or ERROR node containing the function
                current_sibling = node.next_named_sibling();
                while let Some(sibling) = current_sibling {
                    match sibling.kind() {
                        "function_signature" => {
                            detect_ffi_annotation_from_marker(node, sibling, content, helper);
                            break;
                        }
                        "ERROR" => {
                            // Check if ERROR node contains a function declaration
                            if let Some(func_name) =
                                extract_function_name_from_error_node(&sibling, content)
                            {
                                detect_ffi_annotation_from_marker_simple(
                                    node, &func_name, content, helper,
                                );
                                break;
                            }
                            current_sibling = sibling.next_named_sibling();
                        }
                        "function_type" | "type_arguments" | "initialized_identifier_list" => {
                            // Skip these intermediate nodes
                            current_sibling = sibling.next_named_sibling();
                        }
                        _ => break,
                    }
                }
            }
        }
        _ => {}
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_edges(child, content, ast_graph, helper);
    }
}

/// Check if a `member_access` node represents a function call.
fn is_function_call(node: &Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "selector" {
            let mut sel_cursor = child.walk();
            for sel_child in child.children(&mut sel_cursor) {
                if sel_child.kind() == "argument_part" {
                    return true;
                }
            }
        }
    }
    false
}

/// Process a function/method call and create a call edge.
fn process_function_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Find the caller context
    let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) else {
        return; // Call outside function context (e.g., field initializer)
    };

    // Extract callee name
    let Some(target_name) = extract_call_target(&node, content) else {
        return;
    };

    if target_name.is_empty() {
        return;
    }

    // Check if this is an async call (wrapped in await_expression)
    let is_async_call = is_await_call(&node);

    // Count arguments
    let argument_count = count_arguments(&node);

    // Create caller node (ensure it exists) - use proper Span from context
    let source_id = if caller_context.is_method {
        helper.ensure_method(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            caller_context.is_static,
        )
    } else {
        helper.ensure_function(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            false,
        )
    };

    // Create callee node (ensure it exists)
    let call_site_span = node_to_span(&node);
    let target_id = helper.ensure_callee(&target_name, call_site_span, CalleeKindHint::Function);

    // Add call edge with metadata
    let call_span: Vec<Span> = Some(call_site_span).into_iter().collect();
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        is_async_call,
        call_span,
    );
}

/// Process a cascade call (..`method()`) and create a call edge.
fn process_cascade_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Find the caller context
    let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) else {
        return;
    };

    // Extract method name from cascade_section
    let Some(target_name) = extract_cascade_method_name(&node, content) else {
        return;
    };

    if target_name.is_empty() {
        return;
    }

    // Check if this is an async call
    let is_async_call = is_await_call(&node);

    // Count arguments
    let argument_count = count_cascade_arguments(&node);

    // Create caller node - use proper Span from context
    let source_id = if caller_context.is_method {
        helper.ensure_method(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            caller_context.is_static,
        )
    } else {
        helper.ensure_function(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            false,
        )
    };

    // Create callee node
    let cascade_site_span = node_to_span(&node);
    let target_id = helper.ensure_callee(&target_name, cascade_site_span, CalleeKindHint::Function);

    // Add call edge
    let call_span: Vec<Span> = Some(cascade_site_span).into_iter().collect();
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        is_async_call,
        call_span,
    );
}

/// Extract the call target name from a `member_access` node.
fn extract_call_target(node: &Node, content: &[u8]) -> Option<String> {
    // For member_access nodes (Dart function calls), extract the method name from the selector
    if node.kind() == "member_access" {
        // Look for selector child, which contains the method name
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "selector"
                && let Some(name) = extract_selector_identifier(&child, content)
            {
                return Some(name);
            }
        }

        // Fallback: try to get first identifier (for simple function calls)
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier"
                && let Ok(text) = child.utf8_text(content)
            {
                return Some(text.trim().to_string());
            }
        }
    }
    None
}

/// Extract identifier from a selector node.
fn extract_selector_identifier(selector: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = selector.walk();
    for child in selector.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    return Some(text.trim().to_string());
                }
            }
            "unconditional_assignable_selector" | "assignable_selector" => {
                if let Some(name) = extract_nested_identifier(&child, content) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract identifier from nested selector nodes.
fn extract_nested_identifier(node: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(text) = child.utf8_text(content)
        {
            return Some(text.trim().to_string());
        }
        if let Some(name) = extract_nested_identifier(&child, content) {
            return Some(name);
        }
    }
    None
}

/// Extract method name from a `cascade_section` node.
fn extract_cascade_method_name(node: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "cascade_selector" => {
                if let Some(name) = extract_nested_identifier(&child, content)
                    && (has_argument_part(&child) || has_argument_part(node))
                {
                    return Some(name);
                }
            }
            "selector" => {
                if let Some(name) = extract_selector_identifier(&child, content)
                    && (has_argument_part(&child) || has_argument_part(node))
                {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Check if a node has an `argument_part` child.
fn has_argument_part(node: &Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "argument_part" {
            return true;
        }
        // Check one level deeper for nested structures
        let mut nested_cursor = child.walk();
        for nested_child in child.children(&mut nested_cursor) {
            if nested_child.kind() == "argument_part" {
                return true;
            }
        }
    }
    false
}

/// Check if a call is wrapped in an await expression.
fn is_await_call(node: &Node) -> bool {
    // Check up to 3 parent levels for await_expression
    let mut current = *node;
    for _ in 0..3 {
        if let Some(parent) = current.parent() {
            if parent.kind() == "await_expression" {
                return true;
            }
            current = parent;
        } else {
            break;
        }
    }
    false
}

/// Count arguments in a function call.
fn count_arguments(node: &Node) -> u8 {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "selector" {
            let mut sel_cursor = child.walk();
            for sel_child in child.children(&mut sel_cursor) {
                if sel_child.kind() == "argument_part" {
                    return count_args_in_argument_part(&sel_child);
                }
            }
        }
    }
    255 // Unknown
}

/// Count arguments in a cascade call.
fn count_cascade_arguments(node: &Node) -> u8 {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "argument_part" {
            return count_args_in_argument_part(&child);
        }
        // Check nested selectors
        let mut nested_cursor = child.walk();
        for nested_child in child.children(&mut nested_cursor) {
            if nested_child.kind() == "argument_part" {
                return count_args_in_argument_part(&nested_child);
            }
        }
    }
    255 // Unknown
}

/// Count arguments in an `argument_part` node.
fn count_args_in_argument_part(node: &Node) -> u8 {
    let mut count: u8 = 0;
    let mut found_arguments = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "arguments" {
            found_arguments = true;
            let mut args_cursor = child.walk();
            for arg_child in child.children(&mut args_cursor) {
                // Count named children that are actual arguments (not parentheses or commas)
                if arg_child.is_named() && arg_child.kind() != "comment" {
                    count = count.saturating_add(1);
                }
            }
        }
    }
    // Return 0 if we found an arguments node with no args (empty parens),
    // or 255 if we couldn't find an arguments node at all
    if found_arguments { count } else { 255 }
}

/// Convert a tree-sitter node to a Span.
fn node_to_span(node: &Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

// ================================
// FFI Edge Detection
// ================================

/// Extract the full qualified call path from a `member_access` node.
/// For `ffi.DynamicLibrary.open()`, returns `ffi.DynamicLibrary.open`.
fn extract_full_call_path(node: &Node, content: &[u8]) -> Option<String> {
    if node.kind() != "member_access" {
        return None;
    }

    let mut parts = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    parts.push(text.trim().to_string());
                }
            }
            "selector" => {
                if let Some(name) = extract_selector_identifier(&child, content) {
                    parts.push(name);
                }
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

/// Check if a call path ends with `DynamicLibrary.{method}`.
fn is_dynamic_library_method(full_path: &str, method: &str) -> bool {
    // Split by '.' and check that the last two segments are exactly "DynamicLibrary" and method
    let parts: Vec<&str> = full_path.split('.').collect();
    if parts.len() >= 2 {
        let last_two = &parts[parts.len() - 2..];
        last_two[0] == "DynamicLibrary" && last_two[1] == method
    } else {
        false
    }
}

/// Check if a `member_access` node is a `DynamicLibrary.{open, executable, process}` call.
fn is_ffi_dynamic_library_call(node: &Node, content: &[u8]) -> bool {
    // Structure: member_access with "DynamicLibrary" and method {open, executable, process}
    if let Some(full_path) = extract_full_call_path(node, content) {
        is_dynamic_library_method(&full_path, "open")
            || is_dynamic_library_method(&full_path, "executable")
            || is_dynamic_library_method(&full_path, "process")
    } else {
        false
    }
}

/// Check if a `member_access` node is a `lookup().asFunction()` chain.
fn is_lookup_chain(node: &Node, content: &[u8]) -> bool {
    // The full path should contain both "lookup" and "asFunction"
    // This handles: dylib.lookup('symbol').asFunction()
    if let Some(full_path) = extract_full_call_path(node, content) {
        full_path.contains(".asFunction")
    } else {
        false
    }
}

/// Check if a `member_access` node is a `lookupFunction()` call.
fn is_lookup_function_call(node: &Node, content: &[u8]) -> bool {
    if let Some(full_path) = extract_full_call_path(node, content) {
        full_path.contains(".lookupFunction")
    } else {
        false
    }
}

/// Detect DynamicLibrary.{open, executable, process} calls and create FFI edges.
#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_library_loading(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Find the caller context
    let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) else {
        return;
    };

    // Extract the full path (e.g., "ffi.DynamicLibrary.open")
    let Some(full_path) = extract_full_call_path(&node, content) else {
        return;
    };

    // Extract method name (open, executable, or process) - validate exact segment match
    let method_name = if is_dynamic_library_method(&full_path, "open") {
        "open"
    } else if is_dynamic_library_method(&full_path, "executable") {
        "executable"
    } else if is_dynamic_library_method(&full_path, "process") {
        "process"
    } else {
        return;
    };

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:DynamicLibrary.{method_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    let caller_id = if caller_context.is_method {
        helper.ensure_method(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            caller_context.is_static,
        )
    } else {
        helper.ensure_function(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            false,
        )
    };

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

/// Detect `lookup().asFunction()` chains and create FFI edges.
#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_lookup_chain(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Find the caller context
    let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) else {
        return;
    };

    // Find selectors - we need to find both the "lookup" selector and the selector with arguments
    // Structure: member_access with multiple selectors
    // Example: dylib.lookup('hello').asFunction()
    //   selector: unconditional_assignable_selector "lookup"
    //   selector: argument_part with arguments ('hello')
    //   selector: unconditional_assignable_selector "asFunction"
    //   selector: argument_part (empty)
    let mut symbol_name: Option<String> = None;
    let mut found_lookup = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "selector" {
            // Check if this is the "lookup" identifier selector
            if !found_lookup {
                let has_lookup = {
                    let mut sel_cursor = child.walk();
                    child.children(&mut sel_cursor).any(|c| {
                        if let Ok(text) = c.utf8_text(content) {
                            text.contains("lookup")
                        } else {
                            false
                        }
                    })
                };
                if has_lookup {
                    found_lookup = true;
                    // Try to extract symbol from this same selector in case arguments are inline
                    // Pattern: lookup<...>('symbol') where identifier and arguments are in same selector
                    if symbol_name.is_none() {
                        symbol_name = extract_symbol_from_selector(&child, content);
                        if symbol_name.is_some() {
                            break;
                        }
                    }
                    continue;
                }
            }

            // If we found lookup, the next selector with argument_part has the symbol
            if found_lookup && symbol_name.is_none() {
                symbol_name = extract_symbol_from_selector(&child, content);
                if symbol_name.is_some() {
                    break;
                }
            }
        }
    }

    let Some(symbol_name) = symbol_name else {
        // DEBUG: lookup chain: No symbol found");
        return;
    };

    if symbol_name.is_empty() {
        // DEBUG: lookup chain: Empty symbol");
        return;
    }
    // DEBUG: lookup chain: Found symbol '{}'", symbol_name);

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:{symbol_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    let caller_id = if caller_context.is_method {
        helper.ensure_method(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            caller_context.is_static,
        )
    } else {
        helper.ensure_function(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            false,
        )
    };

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

/// Detect `lookupFunction()` calls and create FFI edges.
#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_lookup_function(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Find the caller context
    let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) else {
        return;
    };

    // Find selectors - lookupFunction has the method name and then arguments in next selector
    let mut symbol_name: Option<String> = None;
    let mut found_lookup_function = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "selector" {
            // Check if this is the "lookupFunction" identifier selector
            if !found_lookup_function {
                let has_lookup_function = {
                    let mut sel_cursor = child.walk();
                    child.children(&mut sel_cursor).any(|c| {
                        if let Ok(text) = c.utf8_text(content) {
                            text.contains("lookupFunction")
                        } else {
                            false
                        }
                    })
                };
                if has_lookup_function {
                    found_lookup_function = true;
                    continue;
                }
            }

            // If we found lookupFunction, the next selector with argument_part has the symbol
            if found_lookup_function && symbol_name.is_none() {
                symbol_name = extract_symbol_from_selector(&child, content);
                if symbol_name.is_some() {
                    break;
                }
            }
        }
    }

    let Some(symbol_name) = symbol_name else {
        return;
    };

    if symbol_name.is_empty() {
        return;
    }

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:{symbol_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    let caller_id = if caller_context.is_method {
        helper.ensure_method(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            caller_context.is_static,
        )
    } else {
        helper.ensure_function(
            &caller_context.qualified_name,
            Some(caller_context.span),
            caller_context.is_async,
            false,
        )
    };

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

/// Detect `@Native` and `@FfiNative` annotations on external functions and create FFI edges.
/// Detect FFI annotations from the `marker_annotation` + `function_signature` pattern
/// used when tree-sitter does not create `external_declaration` nodes.
#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_ffi_annotation_from_marker(
    annotation_node: Node,
    func_sig: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract function name from the signature
    let Some(func_name_node) = func_sig.child_by_field_name("name") else {
        return;
    };
    let Ok(func_name) = func_name_node.utf8_text(content) else {
        return;
    };
    let func_name = func_name.trim();

    // Try to extract symbol from annotation, fallback to function name
    let symbol_name = extract_symbol_from_annotation_node(annotation_node, content)
        .unwrap_or_else(|| func_name.to_string());

    if symbol_name.is_empty() {
        return;
    }

    // Find enclosing context
    let (caller_qualified_name, is_method) =
        if let Some(class_name) = find_enclosing_class(func_sig, content) {
            (format!("{class_name}.{func_name}"), true)
        } else {
            (func_name.to_string(), false)
        };

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:{symbol_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    let ffi_site_span = node_to_span(&func_sig);
    let caller_id = if is_method {
        helper.ensure_method(&caller_qualified_name, None, false, false)
    } else {
        helper.ensure_callee(
            &caller_qualified_name,
            ffi_site_span,
            CalleeKindHint::Function,
        )
    };

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

/// Extract a symbol from a `marker_annotation` node directly.
fn extract_symbol_from_annotation_node(annotation_node: Node, content: &[u8]) -> Option<String> {
    // Look for arguments in the annotation
    let mut cursor = annotation_node.walk();
    for child in annotation_node.children(&mut cursor) {
        if child.kind() == "arguments" || child.kind() == "argument_list" {
            return extract_symbol_from_arguments_node(child, content);
        }
    }
    None
}

/// Check if an annotation text is an FFI annotation (`@Native` or `@FfiNative`).
/// Returns `true` only for exact matches, not substrings such as `@NativeCallable`.
fn is_ffi_annotation(annotation_text: &str) -> bool {
    let cleaned = annotation_text.trim().trim_start_matches('@');

    // Extract base annotation name before type parameters OR arguments
    // Examples:
    //   "ffi.Native<ffi.Int32>(symbol: 'add')" -> "ffi.Native"
    //   "FfiNative('name')" -> "FfiNative"
    //   "Native()" -> "Native"
    let base_annotation = cleaned.split(['<', '(']).next().unwrap_or(cleaned);

    // Then split by '.' to handle qualified names like "ffi.Native"
    let parts: Vec<&str> = base_annotation.split('.').collect();
    let identifier = *parts.last().unwrap_or(&"");

    // Check for exact matches only
    matches!(identifier, "Native" | "FfiNative")
}

/// Extract a function name from an `ERROR` node containing `external int functionName();`.
/// This handles the tree-sitter-dart grammar bug where `@Native<T>()` creates malformed AST.
fn extract_function_name_from_error_node(error_node: &Node, content: &[u8]) -> Option<String> {
    let text = error_node.utf8_text(content).ok()?;

    // Pattern: "external [modifiers] type functionName(...)"
    // Examples:
    //   "external int getValue();"
    //   "external static int getValue();"
    //   "external @annotation int getValue();"
    let text = text.trim();

    // Extract everything before the opening parenthesis
    let before_paren = text.split('(').next()?;

    // Get the last whitespace-delimited token before '('
    // This is robust to modifiers: "external static int getValue" -> "getValue"
    let func_name = before_paren.split_whitespace().last()?.trim();

    if !func_name.is_empty() && func_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(func_name.to_string());
    }

    None
}

/// Simplified version of `detect_ffi_annotation_from_marker` that takes just the function name.
#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_ffi_annotation_from_marker_simple(
    annotation_node: Node,
    func_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Try to extract symbol from annotation, fallback to function name
    let symbol_name = extract_symbol_from_annotation_node(annotation_node, content)
        .unwrap_or_else(|| func_name.to_string());

    if symbol_name.is_empty() {
        return;
    }

    // For ERROR node case, we don't have proper AST context, so just use function name
    let caller_qualified_name = func_name.to_string();

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:{symbol_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    let caller_id = helper.ensure_callee(
        &caller_qualified_name,
        node_to_span(&annotation_node),
        CalleeKindHint::Function,
    );

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

#[allow(clippy::items_after_statements)] // FFI use imports near usage
fn detect_ffi_annotation(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // The node is external_declaration, get the function_signature from it
    let func_sig = node.child_by_field_name("signature");
    if func_sig.is_none() {
        return;
    }
    let func_sig = func_sig.unwrap();

    // Check for @Native or @FfiNative annotation (previous sibling of external_declaration)
    let annotation_name = get_ffi_annotation_name(node, content);
    if annotation_name.is_none() {
        return;
    }

    // Extract function name from the signature
    let Some(func_name_node) = func_sig.child_by_field_name("name") else {
        return;
    };
    let Ok(func_name) = func_name_node.utf8_text(content) else {
        return;
    };
    let func_name = func_name.trim();

    // Extract symbol from annotation or use function name
    let extracted_symbol = extract_symbol_from_annotation(node, content);
    // eprintln!("DEBUG: Extracted symbol from annotation: {:?}", extracted_symbol);
    let symbol_name = extracted_symbol.unwrap_or_else(|| func_name.to_string());
    // eprintln!("DEBUG: Using symbol: '{}'", symbol_name);

    if symbol_name.is_empty() {
        return;
    }

    // Find the enclosing context (function or class method)
    let (caller_qualified_name, is_method) =
        if let Some(class_name) = find_enclosing_class(node, content) {
            // This is a method
            (format!("{class_name}.{func_name}"), true)
        } else {
            // This is a top-level function
            (func_name.to_string(), false)
        };

    // Create synthetic FFI target
    let ffi_target_name = format!("<ffi:{symbol_name}>");
    let ffi_target_id = helper.add_function(&ffi_target_name, None, false, false);

    // Get or create caller node ID
    // Note: We don't have full context info here, so we use basic ensure_callee/ensure_method
    let ffi_site_span = node_to_span(&node);
    let caller_id = if is_method {
        helper.ensure_method(&caller_qualified_name, None, false, false)
    } else {
        helper.ensure_callee(
            &caller_qualified_name,
            ffi_site_span,
            CalleeKindHint::Function,
        )
    };

    // Create FFI edge
    use sqry_core::graph::unified::edge::kind::FfiConvention;
    helper.add_ffi_edge(caller_id, ffi_target_id, FfiConvention::C);
}

/// Check if a `function_signature` or `method_signature` has an `external` keyword.
#[allow(dead_code)]
fn has_external_keyword(node: Node, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "external" {
            return true;
        }
        // Also check for identifier "external" in case the grammar differs
        if child.kind() == "identifier"
            && let Ok(text) = child.utf8_text(content)
            && text == "external"
        {
            return true;
        }
    }
    false
}

/// Get the FFI annotation name (`@Native` or `@FfiNative`) if present.
fn get_ffi_annotation_name(node: Node, content: &[u8]) -> Option<String> {
    // Look for annotation in the metadata field or as a previous sibling
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if (child.kind() == "annotation" || child.kind() == "marker_annotation")
            && let Ok(text) = child.utf8_text(content)
            && is_ffi_annotation(text)
        {
            let cleaned = text.trim().trim_start_matches('@');
            return Some(cleaned.to_string());
        }
    }

    // Also check previous siblings for annotations
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        // Check if this sibling is an annotation
        if (sibling.kind() == "annotation" || sibling.kind() == "marker_annotation")
            && let Ok(text) = sibling.utf8_text(content)
            && is_ffi_annotation(text)
        {
            let cleaned = text.trim().trim_start_matches('@');
            return Some(cleaned.to_string());
        }

        // Also check if annotation is inside ERROR node (grammar limitation)
        // The ERROR node might contain the annotation or we can look at the ERROR's full text
        if sibling.kind() == "ERROR" {
            // First check if ERROR contains an annotation child
            let mut err_cursor = sibling.walk();
            for err_child in sibling.children(&mut err_cursor) {
                if (err_child.kind() == "annotation" || err_child.kind() == "marker_annotation")
                    && let Ok(text) = err_child.utf8_text(content)
                    && is_ffi_annotation(text)
                {
                    let cleaned = text.trim().trim_start_matches('@');
                    return Some(cleaned.to_string());
                }
            }
        }

        current = sibling.prev_named_sibling();
    }

    None
}

/// Extract a symbol name from annotation arguments.
/// For `@Native(symbol: 'name')`, extracts `name`.
/// For `@FfiNative('name')`, extracts `name`.
fn extract_symbol_from_annotation(node: Node, content: &[u8]) -> Option<String> {
    // Look for annotation in metadata or previous siblings
    let annotation_node = {
        let mut cursor = node.walk();
        let mut found = None;
        for child in node.children(&mut cursor) {
            if (child.kind() == "annotation" || child.kind() == "marker_annotation")
                && let Ok(text) = child.utf8_text(content)
                && is_ffi_annotation(text)
            {
                found = Some(child);
                break;
            }
        }
        if found.is_none() {
            // Check previous siblings (including inside ERROR nodes)
            let mut current = node.prev_named_sibling();
            while let Some(sibling) = current {
                // Check direct sibling
                if (sibling.kind() == "annotation" || sibling.kind() == "marker_annotation")
                    && let Ok(text) = sibling.utf8_text(content)
                    && is_ffi_annotation(text)
                {
                    found = Some(sibling);
                    break;
                }
                // Check inside ERROR node - the ERROR might contain annotation + arguments as separate children
                if sibling.kind() == "ERROR" {
                    // The ERROR node might have marker_annotation + arguments as separate children
                    // So return the ERROR node itself as the "annotation node" for further processing
                    if let Ok(text) = sibling.utf8_text(content)
                        && is_ffi_annotation(text)
                    {
                        found = Some(sibling);
                        break;
                    }
                }
                current = sibling.prev_named_sibling();
            }
        }
        found
    };

    let Some(annotation_node) = annotation_node else {
        // eprintln!("  DEBUG extract_symbol: No annotation node found");
        return None;
    };

    // eprintln!("  DEBUG extract_symbol: Found annotation node, kind={}", annotation_node.kind());

    // If annotation_node is ERROR, the grammar failed to parse the annotation properly
    // Try to extract the symbol directly from string literals in the ERROR node
    if annotation_node.kind() == "ERROR" {
        // Look for string_literal children that might be the symbol
        // Pattern: @Native<Type>(symbol: 'name') or @FfiNative<Type>('name')
        let mut cursor = annotation_node.walk();
        let mut found_symbol_label = false;

        for child in annotation_node.children(&mut cursor) {
            // eprintln!("    DEBUG ERROR child: kind={}, text={:?}", child.kind(), child.utf8_text(content).ok());

            // Check if this is the "symbol:" label
            if child.kind() == "identifier"
                && let Ok(text) = child.utf8_text(content)
                && text == "symbol"
            {
                found_symbol_label = true;
                continue;
            }

            // If we found symbol label, next string_literal is the value
            if found_symbol_label
                && child.kind() == "string_literal"
                && let Ok(text) = child.utf8_text(content)
            {
                let cleaned = text.trim().trim_matches('\'').trim_matches('"');
                // eprintln!("    DEBUG Found symbol after 'symbol:' label: '{}'", cleaned);
                return Some(cleaned.to_string());
            }

            // For @FfiNative, look for first string_literal (no symbol: label)
            if !found_symbol_label
                && child.kind() == "string_literal"
                && child
                    .prev_sibling()
                    .is_some_and(|p| p.kind() != "identifier")
                && let Ok(text) = child.utf8_text(content)
            {
                let cleaned = text.trim().trim_matches('\'').trim_matches('"');
                // eprintln!("    DEBUG Found positional string literal: '{}'", cleaned);
                return Some(cleaned.to_string());
            }
        }

        // No symbol found in ERROR
        // eprintln!("    DEBUG No symbol found in ERROR");
        return None;
    }

    // Look for arguments in the annotation (use field name for reliability)
    if let Some(args_node) = annotation_node.child_by_field_name("arguments") {
        // eprintln!("  DEBUG extract_symbol: Found arguments field");
        let result = extract_symbol_from_arguments_node(args_node, content);
        // eprintln!("  DEBUG extract_symbol: Result from arguments_node: {:?}", result);
        return result;
    }

    // Fallback: iterate children
    let mut cursor = annotation_node.walk();
    for child in annotation_node.children(&mut cursor) {
        if child.kind() == "arguments" || child.kind() == "argument_list" {
            return extract_symbol_from_arguments_node(child, content);
        }
    }

    None
}

/// Extract symbol from annotation arguments node.
fn extract_symbol_from_arguments_node(args_node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = args_node.walk();
    for child in args_node.named_children(&mut cursor) {
        // // eprintln!("    DEBUG arguments child: kind={}", child.kind());
        match child.kind() {
            "string_literal" | "string" => {
                // Direct string argument: @FfiNative('symbol')
                if let Ok(text) = child.utf8_text(content) {
                    let cleaned = text.trim().trim_matches('\'').trim_matches('"');
                    // // eprintln!("    DEBUG found string literal: '{}'", cleaned);
                    return Some(cleaned.to_string());
                }
            }
            "named_argument" | "argument" => {
                // Named argument: @Native(symbol: 'name')
                // OR simple argument: lookup('hello')
                let mut arg_cursor = child.walk();
                let mut is_symbol_arg = false;
                let mut symbol_value = None;

                for arg_child in child.children(&mut arg_cursor) {
                    // // eprintln!("      DEBUG arg child: kind={}", arg_child.kind());
                    if arg_child.kind() == "identifier"
                        && let Ok(text) = arg_child.utf8_text(content)
                        && text == "symbol"
                    {
                        is_symbol_arg = true;
                    }
                    if (arg_child.kind() == "string_literal" || arg_child.kind() == "string")
                        && let Ok(text) = arg_child.utf8_text(content)
                    {
                        let cleaned = text.trim().trim_matches('\'').trim_matches('"');
                        // // eprintln!("      DEBUG found string in argument: '{}'", cleaned);
                        symbol_value = Some(cleaned.to_string());
                    }
                }

                // For named arguments, only return if it's the "symbol" parameter
                // For simple arguments, always return the first string
                if is_symbol_arg && symbol_value.is_some() {
                    return symbol_value;
                } else if !is_symbol_arg && symbol_value.is_some() {
                    // Simple positional argument
                    return symbol_value;
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract symbol from a selector node that contains arguments.
fn extract_symbol_from_selector(selector_node: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = selector_node.walk();
    for child in selector_node.children(&mut cursor) {
        // DEBUG: selector child: kind={}", child.kind());
        if child.kind() == "argument_part" {
            let mut arg_cursor = child.walk();
            for arg_child in child.children(&mut arg_cursor) {
                // // eprintln!("  DEBUG arg_part child: kind={}", arg_child.kind());
                if arg_child.kind() == "arguments" {
                    let result = extract_symbol_from_arguments_node(arg_child, content);
                    // // eprintln!("  DEBUG extract_symbol result: {:?}", result);
                    return result;
                }
            }
        }
    }
    None
}

/// Extract symbol name from function call arguments (for lookup/lookupFunction).
#[allow(dead_code)]
fn extract_symbol_from_call_arguments(node: &Node, content: &[u8]) -> Option<String> {
    // Look for argument_part child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "selector" {
            return extract_symbol_from_selector(&child, content);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use std::path::PathBuf;
    use tree_sitter::Parser;

    fn parse_tree(code: &[u8]) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_dart::language())
            .expect("set Dart language");
        parser.parse(code, None).expect("parse source")
    }

    #[allow(dead_code)]
    fn extract_call_edges(staging: &StagingGraph) -> Vec<(String, String)> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Calls { .. },
                    ..
                } = op
                {
                    // We need to extract the node names from the staging graph
                    // This is a simplified extraction for testing
                    Some((format!("{source:?}"), format!("{target:?}")))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn test_dart_graph_builder_new() {
        let builder = DartGraphBuilder::new();
        assert_eq!(builder.language(), Language::Dart);
    }

    #[test]
    fn test_simple_function_call() {
        let code = b"void helper() {
  print('hello');
}

void main() {
  helper();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Verify that call edges were created
        let stats = staging.stats();
        assert!(
            stats.edges_staged >= 2,
            "Expected at least 2 call edges (main->helper, helper->print)"
        );
    }

    #[test]
    fn test_method_call_on_object() {
        let code = b"class User {
  void save() {}
}

void process(User user) {
  user.save();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 call edge (process->save)"
        );
    }

    #[test]
    fn test_async_function_call() {
        let code = b"Future<void> fetchData() async {
  await Future.delayed(Duration(seconds: 1));
}

Future<void> main() async {
  await fetchData();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (fetchData, main)"
        );
        assert!(stats.edges_staged >= 1, "Expected at least 1 call edge");

        // Check that we detect async calls properly
        let has_async_edge = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Calls { is_async: true, .. },
                    ..
                }
            )
        });
        assert!(has_async_edge, "Expected at least one async call edge");
    }

    #[test]
    fn test_cascade_notation_calls() {
        let code = b"class User {
  void save() {}
  void notify() {}
}

void process(User user) {
  user..save()..notify();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(
            stats.edges_staged >= 2,
            "Expected at least 2 call edges for cascade (process->save, process->notify)"
        );
    }

    #[test]
    fn test_method_inside_class() {
        let code = b"class DataRepository {
  List<int> _fetchData() {
    return [1, 2, 3];
  }

  List<int> process() {
    final data = _fetchData();
    return data;
  }
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(stats.nodes_staged >= 2, "Expected at least 2 method nodes");
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 call edge (process->_fetchData)"
        );
    }

    #[test]
    fn test_nested_class_methods() {
        let code = b"class Outer {
  void outerMethod() {
    innerHelper();
  }

  void innerHelper() {
    print('helper');
  }
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(stats.edges_staged >= 2, "Expected at least 2 call edges");
    }

    #[test]
    fn test_constructor_call() {
        let code = b"class User {
  User(String name);
}

void main() {
  final user = User('Alice');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Constructor calls should be detected
        let stats = staging.stats();
        assert!(stats.nodes_staged >= 1, "Expected at least 1 node");
    }

    #[test]
    fn test_ast_graph_context_tracking() {
        let code = b"void outerFunction() {
  void innerFunction() {
    print('nested');
  }
  innerFunction();
}";
        let tree = parse_tree(code);
        let ast_graph = ASTGraph::from_tree(&tree, code, DEFAULT_SCOPE_DEPTH);

        // Should have at least 2 contexts (outerFunction, innerFunction)
        assert!(
            !ast_graph.contexts().is_empty(),
            "Expected at least 1 callable context"
        );
    }

    #[test]
    fn test_static_method_detection() {
        let code = b"class Utils {
  static void helper() {
    print('static helper');
  }
}

void main() {
  Utils.helper();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        assert!(stats.nodes_staged >= 2, "Expected at least 2 nodes");
    }

    #[test]
    fn test_static_field_detection() {
        let code = b"class Counter {
  static int count = 0;
  int instance = 0;

  static void increment() {
    count++;
  }
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Find the static field node (Counter.count)
        let static_field = staging.nodes().find(|n| {
            staging
                .resolve_node_display_name(Language::Dart, n.entry)
                .as_deref()
                == Some("Counter.count")
        });
        assert!(
            static_field.is_some(),
            "Expected to find Counter.count field"
        );
        let static_field = static_field.unwrap();
        assert_eq!(
            static_field.entry.kind,
            NodeKind::Property,
            "count should be a Property"
        );
        assert!(
            static_field.entry.is_static,
            "Counter.count should be marked as static"
        );

        // Find the instance field node (Counter.instance)
        let instance_field = staging.nodes().find(|n| {
            staging
                .resolve_node_display_name(Language::Dart, n.entry)
                .as_deref()
                == Some("Counter.instance")
        });
        assert!(
            instance_field.is_some(),
            "Expected to find Counter.instance field"
        );
        let instance_field = instance_field.unwrap();
        assert_eq!(
            instance_field.entry.kind,
            NodeKind::Property,
            "instance should be a Property"
        );
        assert!(
            !instance_field.entry.is_static,
            "Counter.instance should not be marked as static"
        );
    }

    #[test]
    fn test_export_public_function() {
        let code = b"void publicFunction() {
  print('public');
}

void _privateFunction() {
  print('private');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Check that we have Export edges
        let has_export = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        });
        assert!(
            has_export,
            "Expected at least one Export edge for public function"
        );
    }

    #[test]
    fn test_export_public_class() {
        let code = b"class User {
  final String name;
  User(this.name);
}

class _PrivateClass {
  void method() {}
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Check that we have Export edges for the public class
        let has_export = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        });
        assert!(has_export, "Expected Export edge for public class");
    }

    #[test]
    fn test_no_export_for_nested_functions() {
        let code = b"void outer() {
  void inner() {
    print('nested');
  }
  inner();
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Count Export edges - should only be 1 (for outer)
        let export_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            export_count, 1,
            "Expected exactly 1 Export edge (for outer function only)"
        );
    }

    #[test]
    fn test_simple_dart_import() {
        let code = b"import 'dart:async';

void main() {
  print('hello');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Check that we have Import edges
        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });
        assert!(has_import, "Expected at least one Import edge");
    }

    #[test]
    fn test_package_import() {
        let code = b"import 'package:flutter/material.dart';

void main() {
  runApp(MyApp());
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });
        assert!(has_import, "Expected Import edge for package import");
    }

    #[test]
    fn test_aliased_import() {
        let code = b"import 'package:http/http.dart' as http;

void main() {
  http.get('https://example.com');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Check for Import edge with alias
        let has_aliased_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { alias: Some(_), .. },
                    ..
                }
            )
        });
        assert!(
            has_aliased_import,
            "Expected Import edge with alias for 'as http'"
        );
    }

    #[test]
    fn test_multiple_imports() {
        let code = b"import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';

void main() {
  print('hello');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Count Import edges - should be at least 3
        let import_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Imports { .. },
                        ..
                    }
                )
            })
            .count();

        assert!(
            import_count >= 3,
            "Expected at least 3 Import edges, found {import_count}"
        );
    }

    #[test]
    fn test_relative_import() {
        let code = b"import 'models/user.dart';

void main() {
  final user = User('Alice');
}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });
        assert!(has_import, "Expected Import edge for relative import");
    }

    #[test]
    fn test_export_public_top_level_variable() {
        let code = b"final String publicVar = 'test';
final String _privateVar = 'private';

void testFunc() {}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        let stats = staging.stats();
        // Should have: publicVar (variable), testFunc (function), <file_module> (module)
        // Plus _privateVar (variable) - it gets created but not exported
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes, found {}",
            stats.nodes_staged
        );

        // Check that we have Export edges for public variable
        let export_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count();

        // Should export: publicVar and testFunc (not _privateVar)
        assert_eq!(
            export_count, 2,
            "Expected 2 Export edges (publicVar and testFunc)"
        );
    }

    #[test]
    fn test_export_public_top_level_const() {
        let code = b"const int publicConst = 42;
const int _privateConst = 99;

class PublicClass {}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Check that we have Export edges for public const
        let export_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count();

        // Should export: publicConst and PublicClass (not _privateConst)
        assert_eq!(
            export_count, 2,
            "Expected 2 Export edges (publicConst and PublicClass)"
        );
    }

    #[test]
    fn test_no_export_for_local_variables() {
        let code = b"void testFunc() {
  final String localVar = 'local';
  const int localConst = 42;
}

final String topLevelVar = 'top';";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Count Export edges - should only export topLevelVar and testFunc
        let export_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            export_count, 2,
            "Expected 2 Export edges (topLevelVar and testFunc, not local variables)"
        );
    }

    #[test]
    fn test_export_mixed_declarations() {
        let code = b"// Public declarations
final String publicVar = 'public';
const int publicConst = 42;

class PublicClass {}

void publicFunction() {}

// Private declarations
final String _privateVar = 'private';
const int _privateConst = 99;

class _PrivateClass {}

void _privateFunction() {}";
        let tree = parse_tree(code);
        let file = PathBuf::from("test.dart");
        let mut staging = StagingGraph::new();

        let builder = DartGraphBuilder::new();
        builder
            .build_graph(&tree, code, &file, &mut staging)
            .unwrap();

        // Count Export edges - should only export public symbols
        let export_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count();

        // Should export: publicVar, publicConst, PublicClass, publicFunction (4 total)
        assert_eq!(
            export_count, 4,
            "Expected 4 Export edges (all public symbols, none private)"
        );
    }
}
