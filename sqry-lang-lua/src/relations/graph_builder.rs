//! `GraphBuilder` implementation for Lua
//!
//! Builds the unified `CodeGraph` for Lua files by:
//! 1. Extracting function definitions (global, local, module methods)
//! 2. Detecting function call expressions
//! 3. Creating call edges between caller and callee
//! 4. Detecting `LuaJIT` FFI patterns and creating FFI edges

use std::{collections::HashMap, path::Path};

use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::{GraphBuildHelper, NodeKind, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span};
use tree_sitter::{Node, Tree};

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// File-level module name for exports.
/// In Lua, module methods (M.foo) are exported; local functions are not.
const FILE_MODULE_NAME: &str = "<file_module>";

/// FFI entity types for alias resolution
#[derive(Debug, Clone, PartialEq, Eq)]
enum FfiEntity {
    /// The ffi module itself (from require("ffi"))
    Module,
    /// ffi.C (C standard library)
    CLibrary,
    /// `ffi.load("library_name`")
    LoadedLibrary(String),
}

/// Type alias for FFI alias table
type FfiAliasTable = HashMap<String, FfiEntity>;

/// FFI call information extracted from AST
#[derive(Debug, Clone)]
struct FfiCallInfo {
    /// Name of the C function being called
    function_name: String,
    /// Optional library name (for ffi.load cases)
    library_name: Option<String>,
}

/// `GraphBuilder` for Lua files using manual tree walking approach
#[derive(Debug, Clone, Copy)]
pub struct LuaGraphBuilder {
    max_scope_depth: usize,
}

impl Default for LuaGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl LuaGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for LuaGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Lua);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Create recursion guard for tree walking
        let recursion_limits =
            sqry_core::config::RecursionLimits::load_or_default().map_err(|e| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason: format!("Failed to load recursion limits: {e}"),
                }
            })?;
        let file_ops_depth = recursion_limits.effective_file_ops_depth().map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Invalid file_ops_depth configuration: {e}"),
            }
        })?;
        let mut guard =
            sqry_core::query::security::RecursionGuard::new(file_ops_depth).map_err(|e| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason: format!("Failed to create recursion guard: {e}"),
                }
            })?;

        // Phase 1: Populate FFI alias table
        let mut ffi_aliases = FfiAliasTable::new();
        populate_ffi_aliases(tree.root_node(), content, &mut ffi_aliases);

        // Phase 2: Walk tree to find functions, calls, and FFI patterns
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &mut guard,
            &ffi_aliases,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Lua
    }
}

/// Check if a `function_declaration` or `assignment_statement` has the 'local' keyword.
fn is_local_function(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    // In tree-sitter-lua, local functions are represented as function_declaration nodes
    // that are children of their parent with field name "local_declaration"
    // e.g., `local function foo() end` creates a function_declaration with this field

    // Check if this node is the "local_declaration" field of its parent
    if let Some(index) = named_child_index(parent, node)
        && let Some(field) = parent.field_name_for_named_child(index)
    {
        return field == "local_declaration";
    }

    if let Some(index) = child_index(parent, node)
        && let Some(field) = parent.field_name_for_child(index)
    {
        return field == "local_declaration";
    }

    // Also check for local_variable_declaration parent (for assignment patterns)
    if parent.kind() == "local_variable_declaration" {
        return true;
    }

    false
}

/// Determine visibility based on function name convention.
/// In Lua, functions starting with underscore are considered private by convention.
#[allow(clippy::unnecessary_wraps)]
fn get_function_visibility(qualified_name: &str) -> Option<&'static str> {
    // Extract the last segment of the qualified name (e.g., "M::_foo" -> "_foo")
    let function_name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);

    if function_name.starts_with('_') {
        Some("private")
    } else {
        Some("public")
    }
}

/// Find the index of a named child in its parent
fn named_child_index(parent: Node<'_>, target: Node<'_>) -> Option<u32> {
    for i in 0..parent.named_child_count() {
        if let Some(child) = parent.named_child(i as u32)
            && child.id() == target.id()
        {
            let index = u32::try_from(i).ok()?;
            return Some(index);
        }
    }
    None
}

/// Find the index of a child in its parent (including anonymous children)
fn child_index(parent: Node<'_>, target: Node<'_>) -> Option<u32> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    let mut index = 0u32;
    loop {
        if cursor.node().id() == target.id() {
            return Some(index);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
        index += 1;
    }
    None
}

/// Walk the tree and populate the staging graph.
#[allow(clippy::too_many_lines)] // Single pass keeps extraction logic cohesive.
/// # Errors
///
/// Returns [`GraphBuilderError`] if graph operations fail or recursion depth exceeds the guard's limit.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    guard: &mut sqry_core::query::security::RecursionGuard,
    ffi_aliases: &FfiAliasTable,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!("Recursion limit exceeded: {e}"),
    })?;

    match node.kind() {
        // Note: tree-sitter-lua represents local functions as function_declaration nodes
        // with field name "local_declaration", NOT as separate "local_function" nodes.
        // The is_local_function() helper checks for this field name.
        "function_declaration" => {
            // Extract function context from AST graph
            if let Some(call_context) = ast_graph.get_callable_context(node.id()) {
                let span = span_from_node(node);

                // Check if this is a local function by looking for 'local' keyword
                let is_local = is_local_function(node);
                // Determine visibility based on function name convention (underscore prefix = private)
                let visibility = get_function_visibility(&call_context.qualified_name);

                // Add function or method node
                if call_context.is_method {
                    let node_id = helper.add_method_with_visibility(
                        &call_context.qualified_name,
                        Some(span),
                        false, // Lua doesn't have async
                        false, // is_static
                        visibility,
                    );
                    // Export module methods (M.foo style) - they're public API
                    // Local methods are not exported
                    if call_context.module_name.is_some() && !is_local {
                        let module_id = helper.add_module(FILE_MODULE_NAME, None);
                        helper.add_export_edge(module_id, node_id);
                    }
                } else {
                    let node_id = helper.add_function_with_visibility(
                        &call_context.qualified_name,
                        Some(span),
                        false, // Lua doesn't have async
                        false, // Lua doesn't have unsafe
                        visibility,
                    );
                    // Export module functions (M.foo style) and top-level global functions
                    // Local functions are NOT exported - they have local scope
                    let is_module_scoped = call_context.qualified_name.contains("::");
                    let is_global = !is_local && !is_module_scoped;

                    if (is_module_scoped || is_global) && !is_local {
                        let module_id = helper.add_module(FILE_MODULE_NAME, None);
                        helper.add_export_edge(module_id, node_id);
                    }
                }
            }
        }
        "assignment_statement" => {
            // Handle function assignments (local foo = function() end, M.foo = function() end)
            // Check if there's a function_definition child first
            let mut cursor = node.walk();
            let func_def = node
                .children(&mut cursor)
                .find(|child| child.kind() == "expression_list")
                .and_then(|expr_list| {
                    expr_list
                        .named_children(&mut expr_list.walk())
                        .find(|child| child.kind() == "function_definition")
                });

            if let Some(func_def_node) = func_def {
                // Get the context for the function definition (not the assignment itself)
                if let Some(call_context) = ast_graph.get_callable_context(func_def_node.id()) {
                    let span = span_from_node(node);

                    // Check if this is a local assignment
                    let is_local = is_local_function(node);
                    // Determine visibility based on function name convention (underscore prefix = private)
                    let visibility = get_function_visibility(&call_context.qualified_name);

                    // Add function or method node
                    if call_context.is_method {
                        let node_id = helper.add_method_with_visibility(
                            &call_context.qualified_name,
                            Some(span),
                            false,
                            false,
                            visibility,
                        );
                        // Export module methods (M.foo = function() style) - they're public API
                        // Don't export local assignments
                        if call_context.module_name.is_some() && !is_local {
                            let module_id = helper.add_module(FILE_MODULE_NAME, None);
                            helper.add_export_edge(module_id, node_id);
                        }
                    } else {
                        let node_id = helper.add_function_with_visibility(
                            &call_context.qualified_name,
                            Some(span),
                            false,
                            false,
                            visibility,
                        );
                        // Export module functions (M.foo = function() style) - they're public API
                        // Only export if function has module scope (qualified name contains "::")
                        // Local functions are NOT exported - they have local scope
                        if call_context.qualified_name.contains("::") && !is_local {
                            let module_id = helper.add_module(FILE_MODULE_NAME, None);
                            helper.add_export_edge(module_id, node_id);
                        }
                    }
                }
            }
        }
        "return_statement" => {
            // Handle return table exports: return { key = value, ... }
            // This is a common Lua module pattern
            handle_return_table_exports(node, content, helper);
        }
        "function_call" => {
            // Check for FFI patterns first (ffi.C.*, lib.*, ffi.load)
            if let Some(ffi_info) = extract_ffi_call_info(node, content, ffi_aliases) {
                emit_ffi_edge(ffi_info, node, content, ast_graph, helper);
            }
            // Check if this is a require() call
            else if is_require_call(node, content) {
                // Build import edge for require()
                build_require_import_edge(node, content, helper);
            }
            // Build call edge and call site (for non-require calls too)
            else if let Ok(Some((caller_qname, callee_qname, argument_count, span))) =
                build_call_for_staging(ast_graph, node, content)
            {
                // Ensure both nodes exist
                let call_context = ast_graph.get_callable_context(node.id());
                let is_method = call_context.is_some_and(|c| c.is_method);

                let source_id = if is_method {
                    helper.ensure_method(&caller_qname, None, false, false)
                } else {
                    helper.ensure_function(&caller_qname, None, false, false)
                };
                let target_id = helper.ensure_function(&callee_qname, None, false, false);

                // Add call edge
                let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                helper.add_call_edge_full_with_span(
                    source_id,
                    target_id,
                    argument_count,
                    false,
                    vec![span],
                );
            }
        }
        "table_constructor" => {
            // Handle table constructor: { key = value, ... }
            // Create property nodes for table fields
            build_table_fields(node, content, helper)?;
        }
        "dot_index_expression" | "bracket_index_expression" => {
            // Handle field access: table.field or table["field"]
            // Create property nodes for field accesses
            build_field_access(node, content, helper)?;
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(child, content, ast_graph, helper, guard, ffi_aliases)?;
    }

    guard.exit();
    Ok(())
}

/// Handle return table exports: `return { key = value, ... }`
///
/// This extracts exports from return statements with table constructors.
/// Pattern: `return { func1 = func1, func2 = M.func2, ... }`
fn handle_return_table_exports(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Find the expression_list child, then the table_constructor inside it
    // return { ... } has structure: return_statement -> expression_list -> table_constructor
    let Some(expr_list) = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "expression_list")
    else {
        return;
    };

    let Some(table_node) = expr_list
        .children(&mut expr_list.walk())
        .find(|child| child.kind() == "table_constructor")
    else {
        return;
    };

    let module_id = helper.add_module(FILE_MODULE_NAME, None);

    // Iterate over field entries in the table
    let mut cursor = table_node.walk();
    for field in table_node.children(&mut cursor) {
        if field.kind() != "field" {
            continue;
        }

        // Extract the key name (field name)
        let key_name = if let Some(name_node) = field.child_by_field_name("name") {
            // Pattern: { key = value }
            name_node
                .utf8_text(content)
                .ok()
                .map(|s| s.trim().to_string())
        } else {
            // Pattern: { [expr] = value } - skip dynamic keys
            continue;
        };

        let Some(key) = key_name else {
            continue;
        };

        // Extract the value (what's being exported)
        let Some(value_node) = field.child_by_field_name("value") else {
            continue;
        };

        // Try to resolve what the value references
        // Common patterns:
        // 1. Direct reference: { func = func }
        // 2. Module reference: { func = Module.func }
        // 3. Inline function: { func = function() end }

        let exported_name = match value_node.kind() {
            "identifier" => {
                // Direct reference: { func = func }
                value_node.utf8_text(content).ok().map(str::to_string)
            }
            "dot_index_expression" | "method_index_expression" => {
                // Module reference: { func = Module.func }
                // Extract the full qualified name
                extract_table_field_qualified_name(value_node, content).ok()
            }
            "function_definition" => {
                // Inline function: { func = function() end }
                // Use the key as the function name
                Some(key.clone())
            }
            _ => None,
        };

        if let Some(name) = exported_name {
            // Create export edge
            // The exported symbol should already exist from function_declaration/assignment handling
            // If it doesn't exist yet, create it as a function node using ensure_function
            let exported_id = helper.ensure_function(&name, None, false, false);

            helper.add_export_edge(module_id, exported_id);
        }
    }
}

/// Extract qualified name from a table field expression
fn extract_table_field_qualified_name(node: Node<'_>, content: &[u8]) -> Result<String, String> {
    match node.kind() {
        "identifier" => node
            .utf8_text(content)
            .map(str::to_string)
            .map_err(|_| "failed to read identifier".to_string()),
        "dot_index_expression" => {
            let table = node
                .child_by_field_name("table")
                .ok_or_else(|| "dot_index_expression missing table".to_string())?;
            let field = node
                .child_by_field_name("field")
                .ok_or_else(|| "dot_index_expression missing field".to_string())?;

            let table_text = extract_table_field_qualified_name(table, content)?;
            let field_text = field
                .utf8_text(content)
                .map_err(|_| "failed to read field".to_string())?;

            Ok(format!("{table_text}::{field_text}"))
        }
        "method_index_expression" => {
            let table = node
                .child_by_field_name("table")
                .ok_or_else(|| "method_index_expression missing table".to_string())?;
            let method = node
                .child_by_field_name("method")
                .ok_or_else(|| "method_index_expression missing method".to_string())?;

            let table_text = extract_table_field_qualified_name(table, content)?;
            let method_text = method
                .utf8_text(content)
                .map_err(|_| "failed to read method".to_string())?;

            Ok(format!("{table_text}::{method_text}"))
        }
        _ => node
            .utf8_text(content)
            .map(str::to_string)
            .map_err(|_| "failed to read node".to_string()),
    }
}

/// Check if a function call is a `require()` call
fn is_require_call(call_node: Node<'_>, content: &[u8]) -> bool {
    // Look for the function name
    if let Some(name_node) = call_node.child_by_field_name("name")
        && let Ok(text) = name_node.utf8_text(content)
    {
        return text.trim() == "require";
    }
    false
}

/// Build import edge from a `require()` call
fn build_require_import_edge(call_node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Extract the module name from the arguments
    // Pattern: require("module.name") or require 'module.name'
    let Some(args_node) = call_node.child_by_field_name("arguments") else {
        return;
    };

    // Find the first string argument
    let mut cursor = args_node.walk();
    let mut module_name: Option<String> = None;

    for child in args_node.children(&mut cursor) {
        if (child.kind() == "string" || child.kind() == "string_content")
            && let Ok(text) = child.utf8_text(content)
        {
            // Remove quotes from string literal
            let trimmed = text
                .trim()
                .trim_start_matches(['"', '\'', '['])
                .trim_end_matches(['"', '\'', ']'])
                .to_string();
            if !trimmed.is_empty() {
                module_name = Some(trimmed);
                break;
            }
        }
        // Also check for nested string content
        let mut inner_cursor = child.walk();
        for inner_child in child.children(&mut inner_cursor) {
            if inner_child.kind() == "string_content"
                && let Ok(text) = inner_child.utf8_text(content)
            {
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    module_name = Some(trimmed);
                    break;
                }
            }
        }
        if module_name.is_some() {
            break;
        }
    }

    // Create import edge if we found a module name
    if let Some(imported_module) = module_name {
        let span = span_from_node(call_node);

        // Create module node (importer) and import node (imported)
        let module_id = helper.add_module("<module>", None);
        let import_id = helper.add_import(&imported_module, Some(span));

        // Lua require() returns a table/module reference, not a wildcard import
        // is_wildcard: false because you get a specific module reference
        helper.add_import_edge_full(module_id, import_id, None, false);
    }
}

// ============================================================================
// FFI Detection - LuaJIT FFI patterns
// ============================================================================

/// Populate the FFI alias table by walking the AST and tracking:
/// - `require("ffi")` assignments
/// - `ffi.C` aliases
/// - `ffi.load("library")` assignments
fn populate_ffi_aliases(node: Node, content: &[u8], aliases: &mut FfiAliasTable) {
    match node.kind() {
        "local_variable_declaration" | "assignment_statement" => {
            // Try to extract FFI-related assignments
            extract_ffi_assignment(node, content, aliases);
        }
        _ => {}
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        populate_ffi_aliases(child, content, aliases);
    }
}

#[cfg(test)]
#[allow(dead_code)]
fn debug_node_structure(node: Node, content: &[u8], indent: usize) {
    let indent_str = "  ".repeat(indent);
    let text = node.utf8_text(content).ok().and_then(|t| {
        let trimmed = t.trim();
        if trimmed.len() > 50 || trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    eprintln!(
        "{}{}{}",
        indent_str,
        node.kind(),
        text.map(|t| format!(" [{t}]")).unwrap_or_default()
    );

    if indent < 10 {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            debug_node_structure(child, content, indent + 1);
        }
    }
}

/// Extract FFI assignments from local/assignment statements
fn extract_ffi_assignment(node: Node, content: &[u8], aliases: &mut FfiAliasTable) {
    // For variable_declaration, look for assignment_statement child
    // For assignment_statement, extract directly

    let assignment = if node.kind() == "variable_declaration" {
        // Find the assignment_statement child
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|c| c.kind() == "assignment_statement")
    } else if node.kind() == "assignment_statement" {
        Some(node)
    } else {
        None
    };

    if let Some(assign_node) = assignment {
        extract_from_assignment(assign_node, content, aliases);
    }
}

/// Extract FFI entity from `assignment_statement`
fn extract_from_assignment(assign_node: Node, content: &[u8], aliases: &mut FfiAliasTable) {
    // Get variable_list (LHS) and expression_list (RHS)
    let mut cursor = assign_node.walk();
    let children: Vec<_> = assign_node.children(&mut cursor).collect();

    let Some(var_list) = children.iter().find(|c| c.kind() == "variable_list") else {
        return;
    };
    let Some(expr_list) = children.iter().find(|c| c.kind() == "expression_list") else {
        return;
    };

    // Get first variable name
    let Some(var_name_node) = var_list.named_child(0) else {
        return;
    };
    let Ok(var_name) = var_name_node.utf8_text(content) else {
        return;
    };
    let var_name = var_name.trim().to_string();

    // Get first expression (value)
    let Some(value_node) = expr_list.named_child(0) else {
        return;
    };

    // Check different FFI patterns
    if is_require_ffi_call(value_node, content) {
        aliases.insert(var_name, FfiEntity::Module);
    } else if is_ffi_c_reference(value_node, content, aliases) {
        aliases.insert(var_name, FfiEntity::CLibrary);
    } else if let Some(lib_name) = extract_ffi_load_library(value_node, content, aliases) {
        aliases.insert(var_name, FfiEntity::LoadedLibrary(lib_name));
    }
    // Handle alias chains: local D = C (where C is already in alias table)
    else if value_node.kind() == "identifier"
        && let Ok(alias_name) = value_node.utf8_text(content)
    {
        let alias_name = alias_name.trim();
        if let Some(entity) = aliases.get(alias_name).cloned() {
            // Propagate the alias
            aliases.insert(var_name, entity);
        }
    }
}

/// Check if a node is a `require("ffi")` call
fn is_require_ffi_call(node: Node, content: &[u8]) -> bool {
    if node.kind() != "function_call" {
        return false;
    }

    // Check function name is "require"
    let Some(name_node) = node.child_by_field_name("name") else {
        return false;
    };
    let Ok(name_text) = name_node.utf8_text(content) else {
        return false;
    };
    if name_text.trim() != "require" {
        return false;
    }

    // Check first argument is "ffi"
    let Some(args_node) = node.child_by_field_name("arguments") else {
        return false;
    };
    let Some(first_arg) = args_node.named_child(0) else {
        return false;
    };

    // Use extract_string_content for exact equality check
    if let Some(content_str) = extract_string_content(first_arg, content) {
        return content_str == "ffi";
    }

    false
}

/// Check if a node is a reference to `ffi.C`
fn is_ffi_c_reference(node: Node, content: &[u8], aliases: &FfiAliasTable) -> bool {
    if node.kind() != "dot_index_expression" {
        return false;
    }

    let Some(table_node) = node.child_by_field_name("table") else {
        return false;
    };
    let Some(field_node) = node.child_by_field_name("field") else {
        return false;
    };

    let Ok(table_text) = table_node.utf8_text(content) else {
        return false;
    };
    let Ok(field_text) = field_node.utf8_text(content) else {
        return false;
    };

    let table_text = table_text.trim();
    let field_text = field_text.trim();

    // Check if table is an alias to ffi module and field is "C"
    aliases.get(table_text) == Some(&FfiEntity::Module) && field_text == "C"
}

/// Extract library name from `ffi.load("library")` call
fn extract_ffi_load_library(node: Node, content: &[u8], aliases: &FfiAliasTable) -> Option<String> {
    if node.kind() != "function_call" {
        return None;
    }

    // Check if it's ffi.load(...) pattern
    let name_node = node.child_by_field_name("name")?;
    if name_node.kind() != "dot_index_expression" {
        return None;
    }

    let table_node = name_node.child_by_field_name("table")?;
    let field_node = name_node.child_by_field_name("field")?;

    let table_text = table_node.utf8_text(content).ok()?;
    let field_text = field_node.utf8_text(content).ok()?;

    let table_text = table_text.trim();
    let field_text = field_text.trim();

    // Verify table is ffi module alias and field is "load"
    if aliases.get(table_text) != Some(&FfiEntity::Module) || field_text != "load" {
        return None;
    }

    // Extract library name from first argument
    let args_node = node.child_by_field_name("arguments")?;
    let first_arg = args_node.named_child(0)?;

    // Extract string content
    extract_string_content(first_arg, content)
}

/// Extract string content from a string node
fn extract_string_content(string_node: Node, content: &[u8]) -> Option<String> {
    if string_node.kind() == "string" {
        // Try to get string_content child first
        let mut cursor = string_node.walk();
        for child in string_node.children(&mut cursor) {
            if child.kind() == "string_content"
                && let Ok(text) = child.utf8_text(content)
            {
                return Some(text.trim().to_string());
            }
        }

        // Fallback: try to parse the string node itself
        if let Ok(text) = string_node.utf8_text(content) {
            let trimmed = text
                .trim()
                .trim_start_matches(['"', '\'', '['])
                .trim_end_matches(['"', '\'', ']'])
                .to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Extract FFI call information from a function call node
fn extract_ffi_call_info(
    call_node: Node,
    content: &[u8],
    aliases: &FfiAliasTable,
) -> Option<FfiCallInfo> {
    let name_node = call_node.child_by_field_name("name")?;

    // Handle different call patterns
    match name_node.kind() {
        "dot_index_expression" => {
            // First check if this is ffi.load(...) - emit edge for the load call
            if is_ffi_load_call(name_node, content, aliases) {
                return extract_ffi_load_call_info(call_node, content);
            }
            // Then handle normal FFI call patterns
            extract_ffi_from_dot_expression(name_node, content, aliases)
        }
        _ => None,
    }
}

/// Check if a dot expression is ffi.load(...)
fn is_ffi_load_call(dot_expr: Node, content: &[u8], aliases: &FfiAliasTable) -> bool {
    let Some(table_node) = dot_expr.child_by_field_name("table") else {
        return false;
    };
    let Some(field_node) = dot_expr.child_by_field_name("field") else {
        return false;
    };

    let Ok(table_text) = table_node.utf8_text(content) else {
        return false;
    };
    let Ok(field_text) = field_node.utf8_text(content) else {
        return false;
    };

    aliases.get(table_text.trim()) == Some(&FfiEntity::Module) && field_text.trim() == "load"
}

/// Extract `FfiCallInfo` from ffi.load("mylib") call
fn extract_ffi_load_call_info(call_node: Node, content: &[u8]) -> Option<FfiCallInfo> {
    let args_node = call_node.child_by_field_name("arguments")?;
    let first_arg = args_node.named_child(0)?;

    let lib_name = extract_string_content(first_arg, content)?;

    // Create FfiCallInfo for the load call
    // Target: native::<library> (library_name is None so it formats as native::{function_name})
    Some(FfiCallInfo {
        function_name: lib_name,
        library_name: None,
    })
}

/// Extract FFI info from dot index expression (e.g., ffi.C.printf, C.malloc, lib.func)
fn extract_ffi_from_dot_expression(
    dot_expr: Node,
    content: &[u8],
    aliases: &FfiAliasTable,
) -> Option<FfiCallInfo> {
    let table_node = dot_expr.child_by_field_name("table")?;
    let field_node = dot_expr.child_by_field_name("field")?;

    let function_name = field_node.utf8_text(content).ok()?.trim().to_string();

    // Check if table is nested dot expression (ffi.C.func)
    if table_node.kind() == "dot_index_expression" {
        let inner_table = table_node.child_by_field_name("table")?;
        let inner_field = table_node.child_by_field_name("field")?;

        let base_text = inner_table.utf8_text(content).ok()?.trim();
        let mid_text = inner_field.utf8_text(content).ok()?.trim();

        // Pattern: ffi.C.function
        if aliases.get(base_text) == Some(&FfiEntity::Module) && mid_text == "C" {
            return Some(FfiCallInfo {
                function_name,
                library_name: None,
            });
        }
    } else {
        // Pattern: C.function or lib.function (direct alias)
        let table_text = table_node.utf8_text(content).ok()?.trim();

        match aliases.get(table_text) {
            Some(FfiEntity::CLibrary) => {
                return Some(FfiCallInfo {
                    function_name,
                    library_name: None,
                });
            }
            Some(FfiEntity::LoadedLibrary(lib_name)) => {
                return Some(FfiCallInfo {
                    function_name,
                    library_name: Some(lib_name.clone()),
                });
            }
            _ => {}
        }
    }

    None
}

/// Emit an FFI edge for a detected FFI call
fn emit_ffi_edge(
    ffi_info: FfiCallInfo,
    call_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Get caller (enclosing function or file-level)
    let caller_id = get_ffi_caller_node_id(call_node, content, ast_graph, helper);

    // Build target name
    let target_name = if let Some(lib) = ffi_info.library_name {
        format!("native::{}::{}", lib, ffi_info.function_name)
    } else {
        format!("native::{}", ffi_info.function_name)
    };

    // Ensure target node exists
    let target_id = helper.ensure_function(&target_name, None, false, false);

    // Add FfiCall edge with C convention
    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Get the caller node ID for an FFI call (enclosing function or file-level)
fn get_ffi_caller_node_id(
    call_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> sqry_core::graph::unified::node::NodeId {
    // Try to get context from AST graph
    if let Some(call_context) = ast_graph.get_callable_context(call_node.id()) {
        if call_context.is_method {
            return helper.ensure_method(&call_context.qualified_name, None, false, false);
        }
        return helper.ensure_function(&call_context.qualified_name, None, false, false);
    }

    // Walk up tree to find enclosing function
    let mut current = call_node.parent();
    while let Some(node) = current {
        if node.kind() == "function_declaration" || node.kind() == "function_definition" {
            // Extract function name and ensure node exists
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name_text) = name_node.utf8_text(content)
            {
                return helper.ensure_function(name_text, None, false, false);
            }
        }
        current = node.parent();
    }

    // Fallback: file-level context
    helper.ensure_function("<file_level>", None, false, false)
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<(String, String, usize, Span)>> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: (0, content.len()),
            is_method: false,
            module_name: None,
        };
        &module_context
    };

    // Extract the call target (the function being called)
    let Some(name_node) = call_node.child_by_field_name("name") else {
        return Ok(None);
    };

    let callee_text = name_node
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

    // Extract qualified callee name based on call syntax, resolving self if needed
    let mut target_qualified = extract_call_target(name_node, content, call_context)?;

    // CRITICAL: Resolve bare identifiers against the current scope
    if !target_qualified.contains("::") {
        // Try to resolve in the parent scope of the caller
        let scoped_name = if call_context.qualified_name == "<module>" {
            target_qualified.clone()
        } else {
            format!("{}::{}", call_context.qualified_name, &target_qualified)
        };

        // Check if this scoped name exists in the AST graph
        if ast_graph
            .contexts()
            .iter()
            .any(|ctx| ctx.qualified_name == scoped_name)
        {
            target_qualified = scoped_name;
        }
        // If not found in immediate scope, also try sibling scope (parent's scope)
        else if let Some(parent_scope) = extract_parent_scope(&call_context.qualified_name) {
            let sibling_name = format!("{}::{}", parent_scope, &target_qualified);
            if ast_graph
                .contexts()
                .iter()
                .any(|ctx| ctx.qualified_name == sibling_name)
            {
                target_qualified = sibling_name;
            }
        }
    }

    let source_qualified = call_context.qualified_name();

    let span = span_from_node(call_node);
    let argument_count = count_arguments(call_node);

    Ok(Some((
        source_qualified,
        target_qualified,
        argument_count,
        span,
    )))
}

// ============================================================================
// Helper Functions (extracted from old implementation, now used with GraphBuildHelper)
// ============================================================================

/// Extract the qualified call target from a function call name node
fn extract_call_target(
    name_node: Node<'_>,
    content: &[u8],
    call_context: &CallContext,
) -> GraphResult<String> {
    match name_node.kind() {
        "identifier" => {
            // Simple call: foo()
            // Just return the bare name - scoped resolution will happen in build_call_edge
            get_node_text(name_node, content)
        }
        "dot_index_expression" => {
            // Module.function() or obj.method()
            flatten_dotted_name(name_node, content)
        }
        "method_index_expression" => {
            // obj:method() (colon syntax with implicit self)
            flatten_method_name(name_node, content, call_context)
        }
        "bracket_index_expression" => {
            // table["field"]() style access
            flatten_bracket_name(name_node, content, call_context)
        }
        "function_call" => {
            // Chained call: getFn()()
            // Use the whole expression as the target
            get_node_text(name_node, content)
        }
        _ => get_node_text(name_node, content),
    }
}

/// Flatten a `dot_index_expression` (Module.function) into `Module::function`
fn flatten_dotted_name(node: Node<'_>, content: &[u8]) -> GraphResult<String> {
    let table = node
        .child_by_field_name("table")
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "dot_index_expression missing table".to_string(),
        })?;
    let field = node
        .child_by_field_name("field")
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "dot_index_expression missing field".to_string(),
        })?;

    let table_text = collect_table_path(table, content)?;
    let field_text = get_node_text(field, content)?;

    Ok(format!("{table_text}::{field_text}"))
}

/// Flatten a `method_index_expression` (Module:method) into `Module::method`
/// Resolves `self` to the containing module name if available
fn flatten_method_name(
    node: Node<'_>,
    content: &[u8],
    call_context: &CallContext,
) -> GraphResult<String> {
    let table = node
        .child_by_field_name("table")
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "method_index_expression missing table".to_string(),
        })?;
    let method =
        node.child_by_field_name("method")
            .ok_or_else(|| GraphBuilderError::ParseError {
                span: span_from_node(node),
                reason: "method_index_expression missing method".to_string(),
            })?;

    let mut table_text = collect_table_path(table, content)?;
    let method_text = get_node_text(method, content)?;

    // Resolve `self` to the containing module name
    if table_text == "self"
        && let Some(ref module_name) = call_context.module_name
    {
        table_text.clone_from(module_name);
    }
    // If no module_name, keep "self" as-is (might be a top-level method)

    Ok(format!("{table_text}::{method_text}"))
}

/// Flatten a `bracket_index_expression` (e.g. `Module["method"]`) into `Module::method`
fn flatten_bracket_name(
    node: Node<'_>,
    content: &[u8],
    call_context: &CallContext,
) -> GraphResult<String> {
    let table = node
        .child_by_field_name("table")
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "bracket_index_expression missing table".to_string(),
        })?;
    let field = node
        .child_by_field_name("field")
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "bracket_index_expression missing field".to_string(),
        })?;

    let mut table_text = collect_table_path(table, content)?;
    if table_text == "self"
        && let Some(ref module_name) = call_context.module_name
    {
        table_text.clone_from(module_name);
    }

    let field_text = normalize_field_value(field, content)?;

    Ok(format!("{table_text}::{field_text}"))
}

/// Recursively collect table path for nested expressions (A.B.C -> `A::B::C`)
fn collect_table_path(node: Node<'_>, content: &[u8]) -> GraphResult<String> {
    match node.kind() {
        "bracket_index_expression" => {
            let table =
                node.child_by_field_name("table")
                    .ok_or_else(|| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "bracket_index_expression missing table".to_string(),
                    })?;
            let field =
                node.child_by_field_name("field")
                    .ok_or_else(|| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "bracket_index_expression missing field".to_string(),
                    })?;

            let table_text = collect_table_path(table, content)?;
            let field_text = normalize_field_value(field, content)?;

            Ok(format!("{table_text}::{field_text}"))
        }
        "dot_index_expression" => {
            let table =
                node.child_by_field_name("table")
                    .ok_or_else(|| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "nested dot_index_expression missing table".to_string(),
                    })?;
            let field =
                node.child_by_field_name("field")
                    .ok_or_else(|| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "nested dot_index_expression missing field".to_string(),
                    })?;

            let table_text = collect_table_path(table, content)?;
            let field_text = get_node_text(field, content)?;

            Ok(format!("{table_text}::{field_text}"))
        }
        _ => get_node_text(node, content),
    }
}

/// Get text content of a node
fn get_node_text(node: Node<'_>, content: &[u8]) -> GraphResult<String> {
    node.utf8_text(content)
        .map(|text| text.trim().to_string())
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "failed to read node text".to_string(),
        })
}

/// Create a Span from a tree-sitter node
fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

/// Count the number of arguments in a function call
fn count_arguments(call_node: Node<'_>) -> usize {
    call_node
        .child_by_field_name("arguments")
        .map_or(0, |args| {
            args.named_children(&mut args.walk())
                .filter(|child| !matches!(child.kind(), "," | "(" | ")"))
                .count()
        })
}

/// Extract parent scope from a qualified name
/// e.g., "`outer::inner::deep`" -> `Some("outer::inner`")
/// e.g., "outer" -> None
fn extract_parent_scope(qualified_name: &str) -> Option<String> {
    let parts: Vec<&str> = qualified_name.split("::").collect();
    if parts.len() > 1 {
        Some(parts[..parts.len() - 1].join("::"))
    } else {
        None
    }
}

fn normalize_field_value(node: Node<'_>, content: &[u8]) -> GraphResult<String> {
    normalize_field_value_simple(node, content).map_err(|reason| GraphBuilderError::ParseError {
        span: span_from_node(node),
        reason,
    })
}

fn normalize_field_value_simple(node: Node<'_>, content: &[u8]) -> Result<String, String> {
    let raw = node
        .utf8_text(content)
        .map_err(|_| "failed to read field value".to_string())?
        .trim()
        .to_string();

    if raw.is_empty() {
        return Err("empty field value".to_string());
    }

    match node.kind() {
        "string" => Ok(strip_string_literal(&raw)),
        _ => Ok(raw),
    }
}

fn strip_string_literal(raw: &str) -> String {
    if raw.is_empty() {
        return raw.to_string();
    }

    let bytes = raw.as_bytes();
    if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
        || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
    {
        return raw[1..raw.len() - 1].to_string();
    }

    if raw.starts_with('[') && raw.ends_with(']') {
        let mut start = 1usize;
        while start < raw.len() && raw.as_bytes()[start] == b'=' {
            start += 1;
        }
        if start < raw.len() && raw.as_bytes()[start] == b'[' {
            let mut end = raw.len() - 1;
            while end > 0 && raw.as_bytes()[end - 1] == b'=' {
                end -= 1;
            }
            if end > start + 1 {
                return raw[start + 1..end - 1].to_string();
            }
        }
    }

    raw.to_string()
}

fn strip_env_prefix(name: String) -> String {
    name.strip_prefix("_ENV::")
        .map(std::string::ToString::to_string)
        .unwrap_or(name)
}

/// Build property nodes for table constructor fields
fn build_table_fields(
    table_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut cursor = table_node.walk();

    for child in table_node.children(&mut cursor) {
        if child.kind() != "field" {
            continue;
        }

        // Extract field name
        if let Some(name_node) = child.child_by_field_name("name") {
            let field_name = name_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: span_from_node(child),
                    reason: "failed to read table field name".to_string(),
                })?
                .trim();

            // Create property node for the field
            let span = span_from_node(child);
            helper.add_node(field_name, Some(span), NodeKind::Property);
        }
    }

    Ok(())
}

/// Build property nodes for field access expressions
#[allow(clippy::unnecessary_wraps)]
fn build_field_access(
    access_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract field name based on access type
    let field_name = match access_node.kind() {
        "dot_index_expression" => {
            // table.field
            access_node
                .child_by_field_name("field")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
        }
        "bracket_index_expression" => {
            // table["field"] or table[expression]
            access_node.child_by_field_name("field").and_then(|n| {
                // Try to extract string literal
                if n.kind() == "string" {
                    n.utf8_text(content)
                        .ok()
                        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                } else {
                    // For dynamic keys, use the expression text
                    n.utf8_text(content).ok().map(|s| s.trim().to_string())
                }
            })
        }
        _ => None,
    };

    if let Some(name) = field_name {
        // Create property node for the field access
        let span = span_from_node(access_node);
        helper.add_node(&name, Some(span), NodeKind::Property);
    }

    Ok(())
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, closures)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    #[allow(dead_code)] // Reserved for scope analysis
    span: (usize, usize),
    is_method: bool,
    /// Module/class name for resolving self (e.g., "`MyModule`" from "`MyModule::method`")
    module_name: Option<String>,
}

impl CallContext {
    fn qualified_name(&self) -> String {
        self.qualified_name.clone()
    }
}

struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();

        let mut state = WalkerState::new(&mut contexts, &mut node_to_context, max_depth);

        walk_ast(tree.root_node(), content, &mut state)?;

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

/// State passed through AST walking to track scope and context
struct WalkerState<'a> {
    contexts: &'a mut Vec<CallContext>,
    node_to_context: &'a mut HashMap<usize, usize>,
    parent_qualified: Option<String>,
    module_context: Option<String>,
    lexical_depth: usize,
    max_depth: usize,
}

impl<'a> WalkerState<'a> {
    fn new(
        contexts: &'a mut Vec<CallContext>,
        node_to_context: &'a mut HashMap<usize, usize>,
        max_depth: usize,
    ) -> Self {
        Self {
            contexts,
            node_to_context,
            parent_qualified: None,
            module_context: None,
            lexical_depth: 0,
            max_depth,
        }
    }
}

/// Walk the AST to find function definitions and build context mappings
fn walk_ast(node: Node, content: &[u8], state: &mut WalkerState) -> Result<(), String> {
    // CRITICAL FIX #5: Check lexical depth, not namespace depth
    // lexical_depth tracks actual function nesting, excluding module namespace segments
    if state.lexical_depth > state.max_depth {
        return Ok(());
    }

    match node.kind() {
        "local_function" => {
            // Handle: local function foo() end
            handle_local_function(node, content, state)?;
        }
        "function_declaration" => {
            handle_function_declaration(node, content, state)?;
        }
        "assignment_statement" => {
            // Handle: local foo = function() end
            handle_function_assignment(node, content, state)?;
        }
        _ => {
            // Recurse into other nodes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, content, state)?;
            }
        }
    }

    Ok(())
}

/// Handle `local_function` nodes
fn handle_local_function(
    node: Node,
    content: &[u8],
    state: &mut WalkerState,
) -> Result<(), String> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| "local_function missing name".to_string())?;

    // Get the function name (local functions are always simple identifiers)
    let base_name = name_node
        .utf8_text(content)
        .map_err(|_| "failed to read local function name".to_string())?
        .to_string();

    // Build qualified name with parent scope
    let qualified_name = if let Some(parent) = state.parent_qualified.as_ref() {
        format!("{parent}::{base_name}")
    } else {
        base_name
    };

    // Record this function as a callable context
    let context_idx = state.contexts.len();
    state.contexts.push(CallContext {
        qualified_name: qualified_name.clone(),
        span: (node.start_byte(), node.end_byte()),
        is_method: false, // Local functions are never methods
        module_name: state.module_context.clone(),
    });

    // Map all descendant nodes to this context
    map_descendants_to_context(node, state.node_to_context, context_idx);

    // Save the current parent and module context
    let saved_parent = state.parent_qualified.clone();
    let saved_module_context = state.module_context.clone();

    // Update parent for nested functions
    state.parent_qualified = Some(qualified_name);

    // Increment lexical depth for nested functions
    state.lexical_depth += 1;

    // Recurse into function body
    if let Some(body) = node.named_child(node.named_child_count().saturating_sub(1) as u32) {
        walk_ast(body, content, state)?;
    }

    // Restore the previous lexical depth, parent, and module context
    state.lexical_depth -= 1;
    state.parent_qualified = saved_parent;
    state.module_context = saved_module_context;

    Ok(())
}

/// Handle `function_declaration` nodes
fn handle_function_declaration(
    node: Node,
    content: &[u8],
    state: &mut WalkerState,
) -> Result<(), String> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| "function_declaration missing name".to_string())?;

    // Get the base name from the AST node
    let (base_name, is_method) = extract_function_base_name(name_node, content)?;

    // Build qualified name: if there's a parent, prefix it; otherwise use base name
    // CRITICAL FIX #6: Don't prefix if base_name is already fully qualified
    // (e.g., "MyModule::inner" defined inside another function)
    let qualified_name = if base_name.contains("::") {
        base_name.clone()
    } else if let Some(parent) = state.parent_qualified.as_ref() {
        format!("{parent}::{base_name}")
    } else {
        base_name.clone()
    };
    let qualified_name = strip_env_prefix(qualified_name);

    // CRITICAL FIX #4: Module name should be inherited from parent context for nested functions,
    // or extracted from the qualified name only for top-level methods
    let module_name = if is_method {
        // For a method like "MyModule:foo", extract the module part
        if qualified_name.contains("::") {
            let parts: Vec<&str> = qualified_name.split("::").collect();
            if parts.len() > 1 {
                Some(parts[..parts.len() - 1].join("::"))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        // For non-methods (regular functions), inherit the module context from parent
        state.module_context.clone()
    };

    // Record this function as a callable context
    let context_idx = state.contexts.len();
    state.contexts.push(CallContext {
        qualified_name: qualified_name.clone(),
        span: (node.start_byte(), node.end_byte()),
        is_method,
        module_name: module_name.clone(),
    });

    // Map all descendant nodes to this context
    map_descendants_to_context(node, state.node_to_context, context_idx);

    // Save the current parent and module context
    let saved_parent = state.parent_qualified.clone();
    let saved_module_context = state.module_context.clone();

    // Update parent for nested functions
    state.parent_qualified = Some(qualified_name);

    // Update module context if this is a method
    if is_method && module_name.is_some() {
        state.module_context = module_name;
    }

    // CRITICAL FIX #5: Increment lexical depth for nested functions
    state.lexical_depth += 1;

    // Recurse into function body
    if let Some(body) = node.named_child(node.named_child_count().saturating_sub(1) as u32) {
        walk_ast(body, content, state)?;
    }

    // Restore the previous lexical depth, parent, and module context
    state.lexical_depth -= 1;
    state.parent_qualified = saved_parent;
    state.module_context = saved_module_context;

    Ok(())
}

/// Handle `assignment_statement` nodes that assign functions
fn handle_function_assignment(
    node: Node,
    content: &[u8],
    state: &mut WalkerState,
) -> Result<(), String> {
    // Check if this is a function assignment
    let Some(expr_list) = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "expression_list")
    else {
        return Ok(());
    };

    let Some(func_def) = expr_list
        .named_children(&mut expr_list.walk())
        .find(|child| child.kind() == "function_definition")
    else {
        return Ok(());
    };

    // Extract the variable name
    let Some(var_list) = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "variable_list")
    else {
        return Ok(());
    };

    let Some(var_node) = var_list.named_child(0) else {
        return Ok(());
    };

    // Get the base name from the AST node
    let (base_name, is_method) = extract_assignment_base_name(var_node, content)?;

    // Build qualified name: if there's a parent, prefix it; otherwise use base name
    // CRITICAL FIX #6: Don't prefix if base_name is already fully qualified
    let qualified_name = if base_name.contains("::") {
        base_name.clone()
    } else if let Some(parent) = state.parent_qualified.as_ref() {
        format!("{parent}::{base_name}")
    } else {
        base_name.clone()
    };
    let qualified_name = strip_env_prefix(qualified_name);

    // CRITICAL FIX #4: Module name should be inherited from parent context for nested functions,
    // or extracted from the qualified name only for top-level methods
    let module_name = if is_method {
        // For a method like "MyModule:foo", extract the module part
        if qualified_name.contains("::") {
            let parts: Vec<&str> = qualified_name.split("::").collect();
            if parts.len() > 1 {
                Some(parts[..parts.len() - 1].join("::"))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        // For non-methods (regular functions), inherit the module context from parent
        state.module_context.clone()
    };

    // Record this function as a callable context
    let context_idx = state.contexts.len();
    state.contexts.push(CallContext {
        qualified_name: qualified_name.clone(),
        span: (func_def.start_byte(), func_def.end_byte()),
        is_method,
        module_name: module_name.clone(),
    });

    // Map all descendant nodes to this context
    map_descendants_to_context(func_def, state.node_to_context, context_idx);

    // Save the current parent and module context
    let saved_parent = state.parent_qualified.clone();
    let saved_module_context = state.module_context.clone();

    // Update parent for nested functions
    state.parent_qualified = Some(qualified_name);

    // Update module context if this is a method
    if is_method && module_name.is_some() {
        state.module_context = module_name;
    }

    // CRITICAL FIX #5: Increment lexical depth for nested functions
    state.lexical_depth += 1;

    // Recurse into function body
    if let Some(body) = func_def.named_child(func_def.named_child_count().saturating_sub(1) as u32)
    {
        walk_ast(body, content, state)?;
    }

    // Restore the previous lexical depth, parent, and module context
    state.lexical_depth -= 1;
    state.parent_qualified = saved_parent;
    state.module_context = saved_module_context;

    Ok(())
}

/// Extract base name from a `function_declaration` name node (without parent context)
/// Returns (`base_name`, `is_method`)
fn extract_function_base_name(
    name_node: Node<'_>,
    content: &[u8],
) -> Result<(String, bool), String> {
    match name_node.kind() {
        "identifier" => {
            // function foo() end OR local function foo() end
            let name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read function name".to_string())?;
            Ok((name.to_string(), false))
        }
        "dot_index_expression" => {
            // function Module.foo() end
            let table = name_node
                .child_by_field_name("table")
                .ok_or_else(|| "dot_index_expression missing table".to_string())?;
            let field = name_node
                .child_by_field_name("field")
                .ok_or_else(|| "dot_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = field
                .utf8_text(content)
                .map_err(|_| "failed to read field name".to_string())?;

            Ok((format!("{table_text}::{field_text}"), false))
        }
        "method_index_expression" => {
            // function Module:method() end (implicit self parameter)
            let table = name_node
                .child_by_field_name("table")
                .ok_or_else(|| "method_index_expression missing table".to_string())?;
            let method = name_node
                .child_by_field_name("method")
                .ok_or_else(|| "method_index_expression missing method".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let method_text = method
                .utf8_text(content)
                .map_err(|_| "failed to read method name".to_string())?;

            Ok((format!("{table_text}::{method_text}"), true))
        }
        "bracket_index_expression" => {
            // function Module["foo"]() end
            let table = name_node
                .child_by_field_name("table")
                .ok_or_else(|| "bracket_index_expression missing table".to_string())?;
            let field = name_node
                .child_by_field_name("field")
                .ok_or_else(|| "bracket_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = normalize_field_value_simple(field, content)?;

            Ok((format!("{table_text}::{field_text}"), false))
        }
        _ => Err(format!(
            "unsupported function name kind: {}",
            name_node.kind()
        )),
    }
}

/// Extract base name from an assignment variable node (without parent context)
/// Returns (`base_name`, `is_method`)
fn extract_assignment_base_name(
    var_node: Node<'_>,
    content: &[u8],
) -> Result<(String, bool), String> {
    match var_node.kind() {
        "identifier" => {
            // local foo = function() end
            let name = var_node
                .utf8_text(content)
                .map_err(|_| "failed to read identifier".to_string())?;
            Ok((name.to_string(), false))
        }
        "dot_index_expression" => {
            // Module.foo = function() end
            let table = var_node
                .child_by_field_name("table")
                .ok_or_else(|| "dot_index_expression missing table".to_string())?;
            let field = var_node
                .child_by_field_name("field")
                .ok_or_else(|| "dot_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = field
                .utf8_text(content)
                .map_err(|_| "failed to read field".to_string())?;

            Ok((format!("{table_text}::{field_text}"), false))
        }
        "method_index_expression" => {
            // Module:method = function() end
            let table = var_node
                .child_by_field_name("table")
                .ok_or_else(|| "method_index_expression missing table".to_string())?;
            let method = var_node
                .child_by_field_name("method")
                .ok_or_else(|| "method_index_expression missing method".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let method_text = method
                .utf8_text(content)
                .map_err(|_| "failed to read method".to_string())?;

            Ok((format!("{table_text}::{method_text}"), true))
        }
        "bracket_index_expression" => {
            // Module["foo"] = function() end or commands[1] = function() end
            let table = var_node
                .child_by_field_name("table")
                .ok_or_else(|| "bracket_index_expression missing table".to_string())?;
            let field = var_node
                .child_by_field_name("field")
                .ok_or_else(|| "bracket_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = normalize_field_value_simple(field, content)?;

            Ok((format!("{table_text}::{field_text}"), false))
        }
        _ => Err(format!(
            "unsupported assignment target kind: {}",
            var_node.kind()
        )),
    }
}

/// Simplified table path collection (doesn't use `GraphResult`)
fn collect_table_path_simple(node: Node<'_>, content: &[u8]) -> Result<String, String> {
    match node.kind() {
        "identifier" => node
            .utf8_text(content)
            .map(std::string::ToString::to_string)
            .map_err(|_| "failed to read identifier".to_string()),
        "bracket_index_expression" => {
            let table = node
                .child_by_field_name("table")
                .ok_or_else(|| "bracket_index_expression missing table".to_string())?;
            let field = node
                .child_by_field_name("field")
                .ok_or_else(|| "bracket_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = normalize_field_value_simple(field, content)?;

            Ok(format!("{table_text}::{field_text}"))
        }
        "dot_index_expression" => {
            let table = node
                .child_by_field_name("table")
                .ok_or_else(|| "dot_index_expression missing table".to_string())?;
            let field = node
                .child_by_field_name("field")
                .ok_or_else(|| "dot_index_expression missing field".to_string())?;

            let table_text = collect_table_path_simple(table, content)?;
            let field_text = field
                .utf8_text(content)
                .map_err(|_| "failed to read field".to_string())?;

            Ok(format!("{table_text}::{field_text}"))
        }
        _ => node
            .utf8_text(content)
            .map(std::string::ToString::to_string)
            .map_err(|_| "failed to read node".to_string()),
    }
}

/// Map all descendant nodes to a context index
fn map_descendants_to_context(
    node: Node,
    node_to_context: &mut HashMap<usize, usize>,
    context_idx: usize,
) {
    node_to_context.insert(node.id(), context_idx);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        map_descendants_to_context(child, node_to_context, context_idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuaPlugin;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::*;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use sqry_core::plugin::LanguagePlugin;
    use std::path::PathBuf;

    /// Helper to extract Import edges from staging operations
    fn extract_import_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Imports { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    fn parse_lua(source: &str) -> Tree {
        let plugin = LuaPlugin::default();
        plugin.parse_ast(source.as_bytes()).unwrap()
    }

    #[test]
    fn test_extracts_global_functions() {
        let source = r"
            function foo()
            end

            function bar()
            end
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert!(staging.node_count() >= 2);
        assert_has_node(&staging, "foo");
        assert_has_node(&staging, "bar");
    }

    #[test]
    fn test_creates_call_edges() {
        let source = r"
            function caller()
                callee()
            end

            function callee()
            end
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_node(&staging, "caller");
        assert_has_node(&staging, "callee");

        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected at least one call edge");
    }

    #[test]
    fn test_handles_module_methods() {
        let source = r"
            local MyModule = {}

            function MyModule.method1()
                MyModule.method2()
            end

            function MyModule:method2()
            end
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_node(&staging, "method1");
        assert_has_node(&staging, "method2");
        assert_has_call_edge(&staging, "MyModule::method1", "MyModule::method2");
    }

    #[test]
    fn test_nested_functions_scope_resolution() {
        // HIGH FIX #1: Scope stack accumulation
        // Verifies that nested functions don't create "outer::outer::inner" paths
        let source = r"
            function outer()
                local function inner()
                    local function deep()
                    end
                end
            end
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Check that nested functions have correct qualified names
        assert_has_node(&staging, "outer");
        assert_has_node(&staging, "outer::inner");
        assert_has_node(&staging, "outer::inner::deep");

        // The main fix is verified: nested scopes have correct names
        // Note: Local function call resolution (e.g., deep() -> outer::inner::deep)
        // is a separate concern and not covered by this HIGH fix
    }

    #[test]
    fn test_self_resolution_in_methods() {
        // HIGH FIX #2: Self resolution in method calls
        // Verifies that self:method() resolves to Module::method, not self::method
        let source = r"
            local MyModule = {}

            function MyModule:method1()
                self:method2()
            end

            function MyModule:method2()
                self:method3()
            end

            function MyModule:method3()
                -- Empty
            end
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify all methods exist
        assert_has_node(&staging, "MyModule::method1");
        assert_has_node(&staging, "MyModule::method2");
        assert_has_node(&staging, "MyModule::method3");

        // Verify self:method2() resolves to MyModule::method2, not self::method2
        assert_has_call_edge(&staging, "MyModule::method1", "MyModule::method2");

        // Verify self:method3() resolves correctly
        assert_has_call_edge(&staging, "MyModule::method2", "MyModule::method3");
    }

    #[test]
    fn test_local_function_call_resolution() {
        // HIGH FIX #3: Local function call resolution
        // Verifies that calls to nested/local functions resolve to scoped names,
        // not bare identifiers that create synthetic nodes
        let source = r"
function outer()
    local function inner()
        local function deep()
        end
        deep()
    end
    inner()
end
";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify all functions exist with correct qualified names
        assert_has_node(&staging, "outer");
        assert_has_node(&staging, "outer::inner");
        assert_has_node(&staging, "outer::inner::deep");

        // CRITICAL: Verify that inner() call from outer resolves to outer::inner
        assert_has_call_edge(&staging, "outer", "outer::inner");

        // CRITICAL: Verify that deep() call from inner resolves to outer::inner::deep
        assert_has_call_edge(&staging, "outer::inner", "outer::inner::deep");
    }

    #[test]
    fn test_nested_helper_self_resolution() {
        // HIGH FIX #4: Nested helpers inside colon methods should inherit module context
        // Verifies that self:inner() inside a helper defined within MyModule:outer()
        // resolves to MyModule::inner, not outer::inner
        let source = r"
MyModule = {}

function MyModule:outer()
    local function helper()
        self:inner()
    end
    helper()
end

function MyModule:inner()
end
";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify all functions exist
        assert_has_node(&staging, "MyModule::outer");
        assert_has_node(&staging, "MyModule::outer::helper");
        assert_has_node(&staging, "MyModule::inner");

        // CRITICAL: Verify that self:inner() inside helper resolves to MyModule::inner
        assert_has_call_edge(&staging, "MyModule::outer::helper", "MyModule::inner");

        // Verify outer calls helper
        assert_has_call_edge(&staging, "MyModule::outer", "MyModule::outer::helper");
    }

    #[test]
    fn test_bracket_string_key_assignment() {
        let source = r#"
    local Module = {}

    Module["string-key"] = function()
        return true
    end

    local function call_it()
        Module["string-key"]()
    end
    "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify the bracket-indexed function exists
        assert_has_node(&staging, "Module::string-key");
        assert_has_node(&staging, "call_it");

        // Verify the call edge
        assert_has_call_edge(&staging, "call_it", "Module::string-key");
    }

    #[test]
    fn test_numeric_command_table_assignment() {
        let source = r#"
    local commands = {}

    commands[1] = function()
        return "cmd"
    end

    local function run()
        commands[1]()
    end
    "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify the numeric-indexed function exists
        assert_has_node(&staging, "commands::1");
        assert_has_node(&staging, "run");

        // Verify the call edge
        assert_has_call_edge(&staging, "run", "commands::1");
    }

    #[test]
    fn test_env_driven_name_injection() {
        let source = r#"
    _ENV["init"] = function()
        return true
    end

    local function boot()
        init()
    end
    "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify the _ENV-prefixed function is stripped to plain "init"
        assert_has_node(&staging, "init");
        assert_has_node(&staging, "boot");

        // Verify the call edge
        assert_has_call_edge(&staging, "boot", "init");
    }

    #[test]
    fn test_deep_namespace_lexical_depth() {
        // HIGH FIX #5: Lexical depth tracking separate from namespace depth
        // Verifies that methods with deep namespace paths don't hit depth limit prematurely
        let source = r"
Company = {}
Company.Product = {}
Company.Product.Component = {}

function Company.Product.Component:outer()
    local function helper1()
        local function helper2()
            self:inner()
        end
        helper2()
    end
    helper1()
end

function Company.Product.Component:inner()
end
";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default(); // max_scope_depth = 4
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify all functions exist (including deeply nested ones)
        assert_has_node(&staging, "Company::Product::Component::outer");
        assert_has_node(&staging, "Company::Product::Component::outer::helper1");
        assert_has_node(
            &staging,
            "Company::Product::Component::outer::helper1::helper2",
        );
        assert_has_node(&staging, "Company::Product::Component::inner");

        // CRITICAL: Verify that self:inner() inside helper2 resolves correctly
        // despite deep namespace path (3 namespace segments shouldn't count as lexical depth)
        assert_has_call_edge(
            &staging,
            "Company::Product::Component::outer::helper1::helper2",
            "Company::Product::Component::inner",
        );

        // Verify the helper call chain
        assert_has_call_edge(
            &staging,
            "Company::Product::Component::outer",
            "Company::Product::Component::outer::helper1",
        );
        assert_has_call_edge(
            &staging,
            "Company::Product::Component::outer::helper1",
            "Company::Product::Component::outer::helper1::helper2",
        );
    }

    #[test]
    fn test_nested_method_redefinition() {
        // HIGH FIX #6: Fully-qualified method redefinition inside another method
        // Verifies that "function MyModule:inner()" defined inside MyModule:outer()
        // creates MyModule::inner (not MyModule::outer::MyModule::inner)
        let source = r"
MyModule = {}

function MyModule:outer()
    function MyModule:inner()
        -- redefined inside outer
    end
    self:inner()
end
";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify both functions exist with correct qualified names
        assert_has_node(&staging, "MyModule::outer");
        assert_has_node(&staging, "MyModule::inner");

        // CRITICAL: Verify that self:inner() call from outer resolves to MyModule::inner
        assert_has_call_edge(&staging, "MyModule::outer", "MyModule::inner");
    }

    // ============================================================================
    // Import Edge Tests (Wave 7)
    // ============================================================================

    #[test]
    fn test_require_import_edge_double_quotes() {
        let source = r#"
            local json = require("cjson")
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected at least one import edge"
        );

        // Lua require() returns a module reference, NOT a wildcard import
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "Lua require returns module reference, not wildcard"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_require_import_edge_single_quotes() {
        let source = r"
            local socket = require('socket')
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected require import edge");

        // Verify it's an Import edge with correct metadata
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "Lua require returns module reference, not wildcard"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_require_dotted_module() {
        let source = r#"
            local util = require("luasocket.util")
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected dotted module import edge"
        );

        // Verify it's an Import edge
        let edge = import_edges[0];
        assert!(
            matches!(edge, UnifiedEdgeKind::Imports { .. }),
            "Expected Imports edge kind"
        );
    }

    #[test]
    fn test_multiple_requires() {
        let source = r#"
            local json = require("cjson")
            local socket = require("socket")
            local lpeg = require("lpeg")
            local lfs = require("lfs")
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert_eq!(import_edges.len(), 4, "Expected 4 import edges");

        // Verify all are EdgeKind::Imports
        for edge in &import_edges {
            assert!(
                matches!(edge, UnifiedEdgeKind::Imports { .. }),
                "All edges should be Imports"
            );
        }
    }
}
