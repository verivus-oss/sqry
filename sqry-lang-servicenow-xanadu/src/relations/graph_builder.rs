// Nested conditionals kept for readability in member/function traversal

//! `ServiceNow` `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts relationships from `ServiceNow` JavaScript code:
//! - ES6 `import` statements and CommonJS `require()` calls (Import edges)
//! - `GlideRecord` table access (e.g., `new GlideRecord('incident')`)
//! - `gs.*` API calls (logging, info, error, etc.)
//! - Script Include class dependencies (`Class.create()`)
//! - Function calls and method invocations
//!
//! ## Import Detection
//!
//! Uses a dual model matching the TypeScript plugin pattern:
//! - **Import edge**: `module_id → Import(module_source)` for cross-file navigation
//! - **Binding Import nodes**: `Import(specifier_name)` for `imports:Symbol` queryability
//! - Raw module source text as target (no path resolution) — ServiceNow scripts
//!   are database-stored, so relative paths are the canonical module identifier
//!
//! ## ServiceNow-Specific Patterns
//!
//! `ServiceNow` scripts follow specific patterns:
//! - `GlideRecord` for database table operations
//! - `gs.log()`, `gs.info()`, `gs.error()` for logging
//! - `Class.create()` for Script Includes
//! - `GlideAjax` for client-server communication

use std::{collections::HashMap, path::Path};

use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Position, Span,
    unified::{StagingGraph, edge::TableWriteOp, node::NodeId},
};
use tree_sitter::{Node, Tree};

/// Context for tracking `GlideRecord` variable-to-table mappings
#[derive(Default)]
struct GlideRecordContext {
    /// Maps variable names to their `GlideRecord` table names
    /// e.g., "gr" -> "incident", "taskGR" -> "`sc_task`"
    var_to_table: HashMap<String, String>,
}

/// Tracks callable (function/method) contexts discovered in the AST.
#[derive(Debug)]
struct CallableContext {
    /// Qualified name
    qualified_name: String,
    /// Byte span for containment lookups (start, end)
    byte_span: (usize, usize),
    /// Proper span with row/column info for node creation
    span: Span,
    /// Whether this is a method (inside a class)
    is_method: bool,
}

/// Pre-computed AST context for O(1) lookups during edge detection.
struct ASTContext {
    callable_contexts: Vec<CallableContext>,
}

impl ASTContext {
    fn from_tree(tree: &Tree, content: &[u8]) -> Self {
        let mut contexts = Vec::new();
        let mut class_stack: Vec<String> = Vec::new();
        extract_callable_contexts(tree.root_node(), content, &mut contexts, &mut class_stack);
        Self {
            callable_contexts: contexts,
        }
    }

    /// Find the enclosing callable context for a given byte position.
    fn find_enclosing(&self, byte_pos: usize) -> Option<&CallableContext> {
        self.callable_contexts
            .iter()
            .filter(|ctx| byte_pos >= ctx.byte_span.0 && byte_pos < ctx.byte_span.1)
            .min_by_key(|ctx| ctx.byte_span.1 - ctx.byte_span.0) // Prefer innermost scope
    }
}

/// Recursively extract callable contexts from the AST.
fn extract_callable_contexts(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallableContext>,
    class_stack: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim().to_string();
                class_stack.push(name);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_callable_contexts(child, content, contexts, class_stack);
                }
                class_stack.pop();
                return;
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim().to_string();
                let qualified_name = if let Some(class) = class_stack.last() {
                    format!("{class}.{name}")
                } else {
                    name
                };
                contexts.push(CallableContext {
                    qualified_name,
                    byte_span: (node.start_byte(), node.end_byte()),
                    span: node_to_span(&node),
                    is_method: !class_stack.is_empty(),
                });
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let name = name.trim().to_string();
                let qualified_name = if let Some(class) = class_stack.last() {
                    format!("{class}.{name}")
                } else {
                    name
                };
                contexts.push(CallableContext {
                    qualified_name,
                    byte_span: (node.start_byte(), node.end_byte()),
                    span: node_to_span(&node),
                    is_method: true,
                });
            }
        }
        "variable_declarator" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
                && let Some(value_node) = node.child_by_field_name("value")
                && matches!(value_node.kind(), "function_expression" | "arrow_function")
            {
                let span = node_to_span(&value_node);
                contexts.push(CallableContext {
                    qualified_name: name.trim().to_string(),
                    byte_span: (value_node.start_byte(), value_node.end_byte()),
                    span,
                    is_method: false,
                });
            }
        }
        _ => {}
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_callable_contexts(child, content, contexts, class_stack);
    }
}

/// Convert a tree-sitter node to a Span with proper row/column info.
fn node_to_span(node: &Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

/// `GraphBuilder` for `ServiceNow` Xanadu JavaScript files
#[derive(Debug, Default)]
pub struct ServiceNowGraphBuilder;

impl ServiceNowGraphBuilder {
    /// Create a new `ServiceNow` graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for ServiceNowGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        use sqry_core::graph::unified::GraphBuildHelper;

        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::ServiceNow);

        // Build GlideRecord context to track variable-to-table mappings
        let gr_context = build_gliderecord_context(tree.root_node(), content);

        // Build AST context to track callable (function/method) contexts
        let ast_context = ASTContext::from_tree(tree, content);

        // Create module node as fallback for table operations at module level
        let module_name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<module>");
        let module_id = helper.add_module(module_name, None);

        // Walk tree to extract nodes and edges
        walk_ast(
            tree.root_node(),
            content,
            &gr_context,
            &ast_context,
            &mut helper,
            module_id,
        )?;

        // Create export edges for top-level symbols (ServiceNow Script Include exports)
        create_export_edges(tree.root_node(), content, &mut helper, module_id);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::ServiceNow
    }
}

/// Build context for `GlideRecord` variable-to-table mappings
fn build_gliderecord_context(node: Node<'_>, content: &[u8]) -> GlideRecordContext {
    let mut record_context = GlideRecordContext::default();
    walk_for_gliderecord_context(node, content, &mut record_context);
    record_context
}

/// Walk tree to build `GlideRecord` context
fn walk_for_gliderecord_context(
    node: Node<'_>,
    content: &[u8],
    record_context: &mut GlideRecordContext,
) {
    // Look for: var gr = new GlideRecord('table_name')
    if node.kind() == "variable_declarator"
        && let Some(name_node) = node.child_by_field_name("name")
        && let Some(value_node) = node.child_by_field_name("value")
        && value_node.kind() == "new_expression"
        && let Some(constructor) = value_node.child_by_field_name("constructor")
        && let Ok(constructor_text) = constructor.utf8_text(content)
        && (constructor_text == "GlideRecord" || constructor_text == "GlideRecordSecure")
        && let Ok(var_name) = name_node.utf8_text(content)
        && let Some(args) = value_node.child_by_field_name("arguments")
        && let Some(table_name) = extract_string_argument(args, content)
    {
        record_context
            .var_to_table
            .insert(var_name.to_string(), table_name);
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_gliderecord_context(child, content, record_context);
    }
}

/// Extract string argument from arguments node
fn extract_string_argument(args_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = args_node.walk();
    for child in args_node.children(&mut cursor) {
        if child.kind() == "string" {
            // Try to get string content
            if let Some(fragment) = child.child_by_field_name("content") {
                return fragment.utf8_text(content).ok().map(String::from);
            }
            // Fallback: extract string_fragment child
            let mut str_cursor = child.walk();
            for str_child in child.children(&mut str_cursor) {
                if str_child.kind() == "string_fragment" {
                    return str_child.utf8_text(content).ok().map(String::from);
                }
            }
        }
    }
    None
}

/// Get the caller node ID - either from enclosing callable context or fallback to module
fn get_caller_id(
    node: &Node,
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) -> NodeId {
    if let Some(ctx) = ast_context.find_enclosing(node.start_byte()) {
        if ctx.is_method {
            helper.ensure_method(&ctx.qualified_name, Some(ctx.span), false, false)
        } else {
            helper.ensure_function(&ctx.qualified_name, Some(ctx.span), false, false)
        }
    } else {
        module_id
    }
}

/// Walk AST and populate staging graph with ServiceNow-specific nodes and edges
#[allow(clippy::too_many_lines)] // Keeps ServiceNow traversal logic in one readable pass.
fn walk_ast(
    node: Node<'_>,
    content: &[u8],
    gr_context: &GlideRecordContext,
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) -> GraphResult<()> {
    match node.kind() {
        "function_declaration" => handle_function_declaration(&node, content, helper),
        "class_declaration" => handle_class_declaration(&node, content, helper),
        "method_definition" => handle_method_definition(&node, content, helper),
        "variable_declarator" => handle_variable_declarator(&node, content, helper),
        "new_expression" => handle_new_expression(&node, content, helper),
        "import_statement" => handle_import_statement(&node, content, helper, module_id),
        "call_expression" => {
            handle_call_expression(&node, content, gr_context, ast_context, helper, module_id);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ast(child, content, gr_context, ast_context, helper, module_id)?;
    }

    Ok(())
}

/// Create export edges for top-level symbols in `ServiceNow` Script Includes.
/// In `ServiceNow`, top-level classes and functions are automatically exported
/// and made available to other scripts.
fn create_export_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    // Only process direct children of the root (top-level declarations)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name) = extract_named_field(&child, "name", content) {
                    let symbol_id = helper.add_function(&name, None, false, false);
                    helper.add_export_edge(module_id, symbol_id);
                }
            }
            "class_declaration" => {
                if let Some(name) = extract_named_field(&child, "name", content) {
                    let symbol_id = helper.add_class(&name, None);
                    helper.add_export_edge(module_id, symbol_id);
                }
            }
            "variable_declaration" => {
                // Handle var MyClass = Class.create() pattern
                let mut var_cursor = child.walk();
                for var_child in child.children(&mut var_cursor) {
                    if var_child.kind() == "variable_declarator"
                        && let Some(name_node) = var_child.child_by_field_name("name")
                        && let Ok(name) = name_node.utf8_text(content)
                    {
                        let name = name.trim().to_string();
                        if let Some(value_node) = var_child.child_by_field_name("value")
                            && value_node.kind() == "call_expression"
                            && is_class_create_call(&value_node, content)
                        {
                            let symbol_id = helper.add_class(&name, None);
                            helper.add_export_edge(module_id, symbol_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_function_declaration(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    if let Some(name) = extract_named_field(node, "name", content) {
        let span = node_to_span(node);
        helper.add_function(&name, Some(span), false, false);
    }
}

fn handle_class_declaration(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    if let Some(name) = extract_named_field(node, "name", content) {
        let span = node_to_span(node);
        let child_id = helper.add_class(&name, Some(span));

        // Handle class inheritance (extends clause via class_heritage)
        let heritage = node
            .children(&mut node.walk())
            .find(|child| child.kind() == "class_heritage");

        if let Some(heritage_node) = heritage {
            // Extract parent class name from heritage node
            for child in heritage_node.children(&mut heritage_node.walk()) {
                if child.kind() == "identifier"
                    && let Ok(parent_name) = child.utf8_text(content)
                {
                    let parent_name = parent_name.trim().to_string();
                    let parent_id = helper.add_class(&parent_name, None);
                    helper.add_inherits_edge(child_id, parent_id);
                    break;
                }
            }
        }
    }
}

fn handle_method_definition(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    if let Some(name) = extract_named_field(node, "name", content) {
        let span = node_to_span(node);
        let qualified = find_enclosing_class_name(node, content)
            .map(|class_name| format!("{class_name}.{name}"))
            .unwrap_or(name);
        helper.add_method(&qualified, Some(span), false, false);
    }
}

fn handle_variable_declarator(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = name_node
        .utf8_text(content)
        .ok()
        .map(|s| s.trim().to_string())
    else {
        return;
    };

    let Some(value_node) = node.child_by_field_name("value") else {
        return;
    };

    if matches!(
        value_node.kind(),
        "function_expression" | "function" | "arrow_function"
    ) {
        let var_span = node_to_span(node);
        let fn_span = node_to_span(&value_node);
        helper.add_variable(&name, Some(var_span));
        helper.add_function(&name, Some(fn_span), false, false);
        return;
    }

    if value_node.kind() == "call_expression" && is_class_create_call(&value_node, content) {
        let span = node_to_span(&value_node);
        let child_id = helper.add_class(&name, Some(span));

        // Handle Class.create(BaseClass) inheritance
        if let Some(args_node) = value_node.child_by_field_name("arguments")
            && let Some(first_arg) = args_node.named_child(0)
            && first_arg.kind() == "identifier"
            && let Ok(parent_name) = first_arg.utf8_text(content)
        {
            let parent_name = parent_name.trim().to_string();
            let parent_id = helper.add_class(&parent_name, None);
            helper.add_inherits_edge(child_id, parent_id);
        }
    }
}

fn handle_new_expression(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let Some(constructor_text) = new_expression_constructor(node, content) else {
        return;
    };
    if is_glide_record_constructor(&constructor_text) {
        if let Some(table_name) = extract_new_expression_string_arg(node, content) {
            let span = node_to_span(node);
            let synthetic_name = format!("GlideRecord:{table_name}");
            helper.add_function(&synthetic_name, Some(span), false, false);
        }
        return;
    }
    if constructor_text == "GlideAjax"
        && let Some(script_include) = extract_new_expression_string_arg(node, content)
    {
        let span = node_to_span(node);
        let synthetic_name = format!("ScriptInclude:{script_include}");
        helper.add_class(&synthetic_name, Some(span));
    }
}

/// Handle ES6 `import` statements.
///
/// Creates a dual model matching the TypeScript plugin pattern:
/// - **Import edge**: `module_id → Import(module_source)` for cross-file navigation
/// - **Binding Import nodes**: `Import(specifier_name)` for `imports:Symbol` queryability (no edge)
///
/// Uses raw module source text (no `resolve_import_path`) because ServiceNow scripts
/// are database-stored — relative paths are the canonical module identifier.
fn handle_import_statement(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    // Get the module source path (e.g., './utils' from `import { x } from './utils'`)
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let Ok(source_text) = source_node.utf8_text(content) else {
        return;
    };
    let module_path = source_text
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    if module_path.is_empty() {
        return;
    }

    // Create Import edge: module → Import(module_source)
    let span = node_to_span(node);
    let to_id = helper.add_import(&module_path, Some(span));
    helper.add_import_edge(module_id, to_id);

    // Extract specifier binding nodes for queryability (no edges)
    extract_import_specifiers(*node, content, span, helper);
}

/// Extract individual import specifiers and create Import nodes (no edges).
///
/// Walks the import clause to find named imports, default imports,
/// and namespace imports. Creates an Import node for each imported name
/// to support `imports:SymbolName` queries.
fn extract_import_specifiers(
    import_node: Node<'_>,
    content: &[u8],
    span: Span,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let mut cursor = import_node.walk();
    for child in import_node.children(&mut cursor) {
        match child.kind() {
            "import_clause" => {
                extract_from_import_clause(child, content, span, helper);
            }
            "named_imports" => {
                extract_from_named_imports(child, content, span, helper);
            }
            _ => {}
        }
    }
}

/// Extract imports from an `import_clause` node.
fn extract_from_import_clause(
    clause_node: Node<'_>,
    content: &[u8],
    span: Span,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let mut cursor = clause_node.walk();
    for child in clause_node.children(&mut cursor) {
        match child.kind() {
            // Default import: import foo from 'module'
            "identifier" => {
                if let Ok(name) = child.utf8_text(content) {
                    helper.add_import(name, Some(span));
                }
            }
            // Named imports: import { a, b } from 'module'
            "named_imports" => {
                extract_from_named_imports(child, content, span, helper);
            }
            // Namespace import: import * as ns from 'module'
            "namespace_import" => {
                let mut ns_cursor = child.walk();
                for ns_child in child.children(&mut ns_cursor) {
                    if ns_child.kind() == "identifier"
                        && let Ok(name) = ns_child.utf8_text(content)
                    {
                        helper.add_import(name, Some(span));
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract imports from a `named_imports` node (the `{ a, b, c }` part).
fn extract_from_named_imports(
    named_imports: Node<'_>,
    content: &[u8],
    span: Span,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let mut cursor = named_imports.walk();
    for child in named_imports.children(&mut cursor) {
        if child.kind() == "import_specifier" {
            // Use the "name" field (original name, not alias)
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(content) {
                    helper.add_import(name, Some(span));
                }
            } else {
                // Fallback: get first identifier child
                let mut spec_cursor = child.walk();
                for spec_child in child.children(&mut spec_cursor) {
                    if spec_child.kind() == "identifier"
                        && let Ok(name) = spec_child.utf8_text(content)
                    {
                        helper.add_import(name, Some(span));
                        break;
                    }
                }
            }
        }
    }
}

/// Handle `require()` calls — creates Import edge without a Call edge.
///
/// Uses raw module source text as the import target, consistent with the
/// ES6 import handler and ServiceNow's database-stored script model.
fn handle_require_call(
    node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    let Some(args_node) = node.child_by_field_name("arguments") else {
        return;
    };
    let Some(module_path) = extract_string_argument(args_node, content) else {
        return;
    };
    if module_path.is_empty() {
        return;
    }

    let span = node_to_span(node);
    let to_id = helper.add_import(&module_path, Some(span));
    helper.add_import_edge(module_id, to_id);
}

fn handle_call_expression(
    node: &Node,
    content: &[u8],
    gr_context: &GlideRecordContext,
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    let Some(func_node) = node.child_by_field_name("function") else {
        return;
    };

    // Handle member expression calls (obj.method())
    if func_node.kind() == "member_expression" {
        handle_member_api_call(node, &func_node, content, helper);
        handle_gliderecord_member_call(
            node,
            &func_node,
            content,
            gr_context,
            ast_context,
            helper,
            module_id,
        );
        handle_member_method_call(node, &func_node, content, ast_context, helper, module_id);
    } else if func_node.kind() == "identifier" {
        // require() → Import edge (terminal — no Call edge)
        if let Ok("require") = func_node.utf8_text(content) {
            handle_require_call(node, content, helper, module_id);
        } else {
            // Handle simple function calls (foo())
            handle_simple_function_call(node, &func_node, content, ast_context, helper, module_id);
        }
    }
}

fn handle_member_api_call(
    node: &Node,
    func_node: &Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
) {
    let Some((object, property)) = member_expression_parts(func_node, content) else {
        return;
    };
    if object == "gs" {
        let span = node_to_span(node);
        let api_name = format!("gs.{property}");
        helper.add_function(&api_name, Some(span), false, false);
        return;
    }
    if object == "Class" && property == "create" {
        let span = node_to_span(node);
        helper.add_function("Class.create", Some(span), false, false);
    }
}

fn handle_gliderecord_member_call(
    node: &Node,
    func_node: &Node,
    content: &[u8],
    gr_context: &GlideRecordContext,
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    let Some(var_name) = member_expression_object_text(func_node, content) else {
        return;
    };
    let Some(table_name) = gr_context.var_to_table.get(var_name) else {
        return;
    };
    let Some(method) = member_expression_property_text(func_node, content) else {
        return;
    };

    let span = node_to_span(node);
    let table_node_id = ensure_table_node(helper, table_name);
    let caller_id = get_caller_id(node, ast_context, helper, module_id);

    match method {
        "query" | "get" | "next" | "hasNext" | "getValue" => {
            helper.add_table_read_edge_with_span(
                caller_id,
                table_node_id,
                table_name,
                None,
                vec![span],
            );
        }
        "insert" => {
            add_table_write_op(
                helper,
                caller_id,
                table_node_id,
                table_name,
                TableWriteOp::Insert,
                span,
            );
        }
        "update" => {
            add_table_write_op(
                helper,
                caller_id,
                table_node_id,
                table_name,
                TableWriteOp::Update,
                span,
            );
        }
        "deleteRecord" => {
            add_table_write_op(
                helper,
                caller_id,
                table_node_id,
                table_name,
                TableWriteOp::Delete,
                span,
            );
        }
        _ => {}
    }
}

fn add_table_write_op(
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    caller_id: NodeId,
    table_node_id: NodeId,
    table_name: &str,
    operation: TableWriteOp,
    span: Span,
) {
    helper.add_table_write_edge_with_span(
        caller_id,
        table_node_id,
        table_name,
        None,
        operation,
        vec![span],
    );
}

/// Handle simple function calls like `foo()`
fn handle_simple_function_call(
    call_node: &Node,
    func_node: &Node,
    content: &[u8],
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    let Some(callee_name) = func_node
        .utf8_text(content)
        .ok()
        .map(|s| s.trim().to_string())
    else {
        return;
    };

    // Get the caller context
    let caller_node_id = get_caller_id(call_node, ast_context, helper, module_id);

    // Find or create the callee node
    let callee_id = helper.add_function(&callee_name, None, false, false);

    // Create call edge from caller to callee
    helper.add_call_edge(caller_node_id, callee_id);
}

/// Handle member expression method calls like `obj.method()` or `this.method()`
fn handle_member_method_call(
    call_node: &Node,
    func_node: &Node,
    content: &[u8],
    ast_context: &ASTContext,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    module_id: NodeId,
) {
    let Some((object, property)) = member_expression_parts(func_node, content) else {
        return;
    };

    // Skip special cases already handled (gs.*, Class.create, GlideRecord)
    if object == "gs" || (object == "Class" && property == "create") {
        return;
    }

    // Get the caller context
    let caller_node_id = get_caller_id(call_node, ast_context, helper, module_id);

    // Determine the callee name
    let method_name = if object == "this" {
        // For `this.method()`, find the enclosing class
        if let Some(class_name) = find_enclosing_class_name(call_node, content) {
            format!("{class_name}.{property}")
        } else {
            property.to_string()
        }
    } else {
        // For `obj.method()`, assume obj is a class instance
        format!("{object}.{property}")
    };

    // Find or create the callee method node
    let callee_id = helper.add_method(&method_name, None, false, false);

    // Create call edge from caller to callee
    helper.add_call_edge(caller_node_id, callee_id);
}

fn ensure_table_node(
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    table_name: &str,
) -> NodeId {
    let table_node_name = format!("servicenow_table:{table_name}");
    helper.add_class(&table_node_name, None)
}

fn find_enclosing_class_name(node: &Node, content: &[u8]) -> Option<String> {
    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            return parent
                .child_by_field_name("name")
                .and_then(|name_node| name_node.utf8_text(content).ok())
                .map(|text| text.trim().to_string());
        }
        current = parent;
    }
    None
}

fn new_expression_constructor(node: &Node, content: &[u8]) -> Option<String> {
    node.child_by_field_name("constructor")
        .and_then(|constructor| constructor.utf8_text(content).ok())
        .map(ToString::to_string)
}

fn extract_new_expression_string_arg(node: &Node, content: &[u8]) -> Option<String> {
    node.child_by_field_name("arguments")
        .and_then(|args| extract_string_argument(args, content))
}

fn is_glide_record_constructor(constructor_text: &str) -> bool {
    constructor_text == "GlideRecord" || constructor_text == "GlideRecordSecure"
}

fn is_class_create_call(node: &Node, content: &[u8]) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(function_node) = node.child_by_field_name("function") else {
        return false;
    };
    let Some((object, property)) = member_expression_parts(&function_node, content) else {
        return false;
    };
    object == "Class" && property == "create"
}

fn member_expression_parts(func_node: &Node, content: &[u8]) -> Option<(String, String)> {
    let object = member_expression_object_text(func_node, content)?;
    let property = member_expression_property_text(func_node, content)?;
    Some((object.to_string(), property.to_string()))
}

fn member_expression_object_text<'a>(
    func_node: &'a Node<'a>,
    content: &'a [u8],
) -> Option<&'a str> {
    func_node
        .child_by_field_name("object")
        .and_then(|node| node.utf8_text(content).ok())
}

fn member_expression_property_text<'a>(
    func_node: &'a Node<'a>,
    content: &'a [u8],
) -> Option<&'a str> {
    func_node
        .child_by_field_name("property")
        .and_then(|node| node.utf8_text(content).ok())
}

fn extract_named_field(node: &Node, field: &str, content: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|name_node| name_node.utf8_text(content).ok())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use sqry_core::graph::unified::{StagingOp, edge::EdgeKind};

    fn parse_servicenow(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    /// Helper to count TableRead edges in the staging graph
    fn count_table_read_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TableRead { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper to count TableWrite edges in the staging graph
    fn count_table_write_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TableWrite { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper to check if a TableWrite edge with a specific operation exists
    fn has_table_write_edge_with_op(staging: &StagingGraph, expected_op: TableWriteOp) -> bool {
        staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TableWrite { operation, .. },
                    ..
                } if *operation == expected_op
            )
        })
    }

    #[test]
    fn test_creates_module_node() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.query();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");
    }

    #[test]
    fn test_graph_builder_language() {
        let builder = ServiceNowGraphBuilder::new();
        assert_eq!(builder.language(), Language::ServiceNow);
    }

    #[test]
    fn test_empty_file() {
        let source = "";
        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("empty.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle empty file");
    }

    // ========================================================================
    // GlideRecord Table Edge Tests
    // ========================================================================

    #[test]
    fn test_gliderecord_query_creates_table_read_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.query();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        assert_eq!(
            table_read_count, 1,
            "Should create exactly one TableRead edge for gr.query()"
        );
    }

    #[test]
    fn test_gliderecord_get_creates_table_read_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.get('sys_id_value');
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        assert_eq!(
            table_read_count, 1,
            "Should create exactly one TableRead edge for gr.get()"
        );
    }

    #[test]
    fn test_gliderecord_insert_creates_table_write_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.initialize();
gr.short_description = 'Test incident';
gr.insert();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_write_count = count_table_write_edges(&staging);
        assert_eq!(
            table_write_count, 1,
            "Should create exactly one TableWrite edge for gr.insert()"
        );

        assert!(
            has_table_write_edge_with_op(&staging, TableWriteOp::Insert),
            "Should have TableWrite edge with Insert operation"
        );
    }

    #[test]
    fn test_gliderecord_update_creates_table_write_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.get('sys_id_value');
gr.short_description = 'Updated description';
gr.update();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        // Should have one read (get) and one write (update)
        let table_read_count = count_table_read_edges(&staging);
        let table_write_count = count_table_write_edges(&staging);
        assert_eq!(
            table_read_count, 1,
            "Should create one TableRead edge for gr.get()"
        );
        assert_eq!(
            table_write_count, 1,
            "Should create one TableWrite edge for gr.update()"
        );

        assert!(
            has_table_write_edge_with_op(&staging, TableWriteOp::Update),
            "Should have TableWrite edge with Update operation"
        );
    }

    #[test]
    fn test_gliderecord_delete_record_creates_table_write_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.get('sys_id_value');
gr.deleteRecord();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        // Should have one read (get) and one write (deleteRecord)
        let table_read_count = count_table_read_edges(&staging);
        let table_write_count = count_table_write_edges(&staging);
        assert_eq!(
            table_read_count, 1,
            "Should create one TableRead edge for gr.get()"
        );
        assert_eq!(
            table_write_count, 1,
            "Should create one TableWrite edge for gr.deleteRecord()"
        );

        assert!(
            has_table_write_edge_with_op(&staging, TableWriteOp::Delete),
            "Should have TableWrite edge with Delete operation"
        );
    }

    #[test]
    fn test_gliderecord_multiple_operations() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.addQuery('active', true);
gr.query();
while (gr.next()) {
    gr.update();
}

var gr2 = new GlideRecord('change_request');
gr2.initialize();
gr2.short_description = 'Test';
gr2.insert();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        // Count all table edges
        let table_read_count = count_table_read_edges(&staging);
        let table_write_count = count_table_write_edges(&staging);

        // gr.query() -> TableRead
        // gr.next() -> TableRead (inside while loop)
        // gr.update() -> TableWrite (Update)
        // gr2.insert() -> TableWrite (Insert)
        assert!(
            table_read_count >= 2,
            "Should have at least 2 TableRead edges (query + next)"
        );
        assert!(
            table_write_count >= 2,
            "Should have at least 2 TableWrite edges (update + insert)"
        );
    }

    #[test]
    fn test_gliderecord_secure_variant_creates_edges() {
        let source = r#"
var gr = new GlideRecordSecure('incident');
gr.query();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        assert_eq!(
            table_read_count, 1,
            "GlideRecordSecure should also create TableRead edge"
        );
    }

    #[test]
    fn test_gliderecord_set_value_does_not_create_table_edge() {
        // setValue only stages data in memory - it doesn't actually write to the database
        // until update() or insert() is called. So it should NOT create a TableWrite edge.
        let source = r#"
var gr = new GlideRecord('incident');
gr.get('sys_id_value');
gr.setValue('priority', 1);
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        // setValue should NOT create a TableWrite edge (only update/insert do)
        assert!(
            !has_table_write_edge_with_op(&staging, TableWriteOp::Update),
            "setValue should NOT create TableWrite edge - it only stages data in memory"
        );

        // But gr.get() should still create a read edge
        assert!(
            count_table_read_edges(&staging) >= 1,
            "gr.get() should create TableRead edge"
        );
    }

    #[test]
    fn test_gliderecord_get_value_creates_table_read_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.get('sys_id_value');
var priority = gr.getValue('priority');
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        // gr.get() and gr.getValue() should both create TableRead edges
        assert_eq!(
            table_read_count, 2,
            "Should create TableRead edges for both get() and getValue()"
        );
    }

    #[test]
    fn test_gliderecord_has_next_creates_table_read_edge() {
        let source = r#"
var gr = new GlideRecord('incident');
gr.query();
if (gr.hasNext()) {
    gs.info('Has records');
}
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        // query() and hasNext() both create TableRead edges
        assert_eq!(
            table_read_count, 2,
            "Should create TableRead edges for both query() and hasNext()"
        );
    }

    #[test]
    fn test_gliderecord_add_query_does_not_create_edge() {
        // addQuery is a preparatory method that doesn't actually access the table
        let source = r#"
var gr = new GlideRecord('incident');
gr.addQuery('active', true);
gr.addQuery('priority', 1);
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        let table_write_count = count_table_write_edges(&staging);
        assert_eq!(
            table_read_count, 0,
            "addQuery should not create TableRead edge"
        );
        assert_eq!(
            table_write_count, 0,
            "addQuery should not create TableWrite edge"
        );
    }

    #[test]
    fn test_gliderecord_context_tracks_multiple_variables() {
        let source = r#"
var incidentGR = new GlideRecord('incident');
var taskGR = new GlideRecord('sc_task');
var changeGR = new GlideRecord('change_request');

incidentGR.query();
taskGR.query();
changeGR.insert();
"#;

        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("business_rule.snjs");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");

        let table_read_count = count_table_read_edges(&staging);
        let table_write_count = count_table_write_edges(&staging);

        // incidentGR.query() -> TableRead
        // taskGR.query() -> TableRead
        // changeGR.insert() -> TableWrite
        assert_eq!(
            table_read_count, 2,
            "Should have TableRead edges for incident and sc_task"
        );
        assert_eq!(
            table_write_count, 1,
            "Should have TableWrite edge for change_request"
        );
    }

    // ========================================================================
    // Import Edge Helpers
    // ========================================================================

    use std::collections::HashMap as TestHashMap;

    /// Build string ID → string value lookup from staging operations
    fn build_string_lookup(staging: &StagingGraph) -> TestHashMap<u32, String> {
        let mut lookup = TestHashMap::new();
        for op in staging.operations() {
            if let StagingOp::InternString { local_id, value } = op {
                lookup.insert(local_id.index(), value.clone());
            }
        }
        lookup
    }

    /// Build node ID → (name, kind) lookup from staging operations
    fn build_node_lookup(
        staging: &StagingGraph,
    ) -> TestHashMap<
        sqry_core::graph::unified::node::NodeId,
        (String, sqry_core::graph::unified::node::NodeKind),
    > {
        let strings = build_string_lookup(staging);
        let mut nodes = TestHashMap::new();
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
            {
                let name = strings
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                nodes.insert(*node_id, (name, entry.kind));
            }
        }
        nodes
    }

    /// Check if an Import edge exists from source_name to target_name
    fn has_import_edge_unit(staging: &StagingGraph, from: &str, to: &str) -> bool {
        let nodes = build_node_lookup(staging);
        staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Imports { .. },
                    ..
                } if nodes.get(source).map(|(name, _)| name.as_str()) == Some(from)
                  && nodes.get(target).map(|(name, _)| name.as_str()) == Some(to)
            )
        })
    }

    /// Check if an Import node (NodeKind::Import) with a given name exists
    fn has_import_node(staging: &StagingGraph, name: &str) -> bool {
        let nodes = build_node_lookup(staging);
        nodes.values().any(|(n, kind)| {
            n == name && *kind == sqry_core::graph::unified::node::NodeKind::Import
        })
    }

    /// Count Import edges in the staging graph
    fn count_import_edges(staging: &StagingGraph) -> usize {
        staging
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
            .count()
    }

    /// Count Call edges in staging graph
    fn count_call_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn build_graph_unit(source: &str) -> StagingGraph {
        let tree = parse_servicenow(source);
        let mut staging = StagingGraph::new();
        let builder = ServiceNowGraphBuilder;
        let file = PathBuf::from("test.snjs");
        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");
        staging
    }

    // ========================================================================
    // Import Edge Tests
    // ========================================================================

    #[test]
    fn test_es6_import_creates_import_edge() {
        let staging = build_graph_unit(r"import { processData } from './utils';");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
        assert!(
            has_import_node(&staging, "processData"),
            "Expected Import binding node for 'processData'"
        );
    }

    #[test]
    fn test_require_creates_import_edge() {
        let staging = build_graph_unit(r"var utils = require('./utils');");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
    }

    #[test]
    fn test_es6_import_default() {
        let staging = build_graph_unit(r"import utils from './utils';");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
        assert!(
            has_import_node(&staging, "utils"),
            "Expected Import binding node for default import 'utils'"
        );
    }

    #[test]
    fn test_es6_import_namespace() {
        let staging = build_graph_unit(r"import * as utils from './utils';");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
        assert!(
            has_import_node(&staging, "utils"),
            "Expected Import binding node for namespace import 'utils'"
        );
    }

    #[test]
    fn test_es6_import_alias() {
        let staging = build_graph_unit(r"import { processData as pd } from './utils';");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
        // Binding node uses original name, not alias
        assert!(
            has_import_node(&staging, "processData"),
            "Expected Import binding node for original name 'processData'"
        );
    }

    #[test]
    fn test_es6_side_effect_import() {
        let staging = build_graph_unit(r"import './polyfill';");
        assert!(
            has_import_edge_unit(&staging, "test", "./polyfill"),
            "Expected Import edge from module to './polyfill'"
        );
        // Side-effect import has no binding nodes
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Should have exactly one Import edge for side-effect import"
        );
    }

    #[test]
    fn test_require_does_not_create_call_edge() {
        let source = r"var utils = require('./utils');";
        let staging = build_graph_unit(source);

        // Should have Import edge, not Call edge for require()
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge for require()"
        );
        // No Call edge to "require"
        let nodes = build_node_lookup(&staging);
        let has_require_call = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    target,
                    kind: EdgeKind::Calls { .. },
                    ..
                } if nodes.get(target).map(|(name, _)| name.as_str()) == Some("require")
            )
        });
        assert!(!has_require_call, "require() should NOT create a Call edge");
    }

    #[test]
    fn test_require_dynamic_path_no_import_edge() {
        // Dynamic require() with a variable (not a string literal) should
        // not create an Import edge or panic — it silently no-ops.
        let staging = build_graph_unit(r"var mod = require(dynamicPath);");
        assert_eq!(
            count_import_edges(&staging),
            0,
            "Dynamic require() should NOT create an Import edge"
        );
    }

    #[test]
    fn test_require_destructured() {
        let staging = build_graph_unit(r"const { processData } = require('./utils');");
        assert!(
            has_import_edge_unit(&staging, "test", "./utils"),
            "Expected Import edge from module to './utils'"
        );
    }

    #[test]
    fn test_top_level_import_with_functions() {
        let source = r#"
import { helper } from './helpers';

function processRequest() {
    helper();
    return 42;
}
"#;
        let staging = build_graph_unit(source);

        assert!(
            has_import_edge_unit(&staging, "test", "./helpers"),
            "Expected Import edge from module to './helpers'"
        );
        assert!(
            has_import_node(&staging, "helper"),
            "Expected Import binding node for 'helper'"
        );
        // Function call to helper() should still create a Call edge
        assert!(
            count_call_edges(&staging) >= 1,
            "Should have at least one Call edge for helper() invocation"
        );
    }
}
// Nested conditionals kept for readability in member/function traversal
