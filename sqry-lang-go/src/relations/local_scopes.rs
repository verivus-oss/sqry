//! Local scope tracking and reference resolution for Go.
//!
//! Go has simple block-scoped variables with no class members, hoisting, or
//! destructuring. Variable sources are:
//! - `short_var_declaration` (`:=`)
//! - `var_spec` (from `var_declaration`)
//! - `range_clause` (for-range loop variables)
//! - `parameter_declaration` (function/method parameters)
//! - Receiver parameters (method receivers)
//!
//! Shadowing is allowed: a nested scope can redeclare a variable with the
//! same name as an outer scope.

use sqry_core::graph::local_scopes::{self, ScopeId, ScopeKindTrait, ScopeTree};
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::{GraphResult, Span};
use tree_sitter::Node;

// ============================================================================
// Go-specific ScopeKind
// ============================================================================

/// Go scope kinds. Go has block scoping only — no class members or hoisting.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    /// Function body (`func foo() { ... }`).
    Function,
    /// Method body (`func (r Recv) foo() { ... }`).
    Method,
    /// Generic block (`{ ... }`).
    Block,
    /// If-statement branch (then or else).
    IfBranch,
    /// For loop (`for i := 0; ...; ... { ... }`).
    ForLoop,
    /// Switch block (`switch { ... }`).
    SwitchBlock,
    /// Case clause body (`case x: ...`).
    CaseClause,
    /// Select block (`select { ... }`).
    SelectBlock,
    /// Communication case body (`case <-ch: ...`).
    CommClause,
    /// Function literal / closure (`func() { ... }`).
    FuncLiteral,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        false // Go has no class-like scopes
    }

    fn is_overlap_boundary(&self) -> bool {
        false // Go has no overlap boundaries
    }

    fn allows_nested_shadowing(&self) -> bool {
        true // Go allows shadowing in nested scopes
    }
}

/// Type alias for the Go-specialized scope tree.
pub(crate) type GoScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a Go source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<GoScopeTree> {
    let content_len = content.len();
    let mut tree = GoScopeTree::new(content_len);

    let mut guard = local_scopes::load_recursion_guard();
    build_scopes_recursive(&mut tree, root, content, None, &mut guard)?;
    tree.rebuild_index();
    bind_declarations_recursive(&mut tree, root, content, &mut guard)?;
    tree.rebuild_index();
    Ok(tree)
}

// ============================================================================
// Phase 1: Build scopes
// ============================================================================

fn build_scopes_recursive(
    tree: &mut GoScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

    match node.kind() {
        "function_declaration" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Function,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "method_declaration" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Method,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "func_literal" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::FuncLiteral,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "if_statement" => {
            build_if_statement_scopes(tree, node, content, current_scope, guard)?;
            guard.exit();
            return Ok(());
        }
        "for_statement" => {
            // For loop scope covers the entire for statement (init vars are scoped to loop)
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::ForLoop,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "expression_switch_statement" | "type_switch_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::SwitchBlock,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "expression_case" | "type_case" | "default_case" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::CaseClause,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "select_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::SelectBlock,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "communication_case" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::CommClause,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "block" => {
            // Create block scope only if not a function/method/closure body
            // (those are already scoped by their parent handler).
            if !is_function_body(node)
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Block,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
                guard.exit();
                return Ok(());
            }
            // Fall through to default recursion
        }
        _ => {}
    }

    // Default: recurse into children with current scope
    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

/// Build if-statement scopes with proper then/else branching.
fn build_if_statement_scopes(
    tree: &mut GoScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    // Go if-statement can have an init statement: `if x := f(); x > 0 { ... }`
    // The scope for the if covers init + condition + both branches.
    let if_scope = tree.add_scope(
        ScopeKind::IfBranch,
        node.start_byte(),
        node.end_byte(),
        current_scope,
    );
    let scope = if_scope.unwrap_or(current_scope.unwrap_or(0));

    // Process all children in the if scope
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "if_statement" => {
                // else-if chain
                build_if_statement_scopes(tree, child, content, Some(scope), guard)?;
            }
            _ => {
                build_scopes_recursive(tree, child, content, Some(scope), guard)?;
            }
        }
    }

    Ok(())
}

/// Check if a block node is the direct body of a function/method/closure.
fn is_function_body(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) && parent
            .child_by_field_name("body")
            .is_some_and(|body| body.id() == node.id())
    })
}

/// Recurse into all children of a node.
fn recurse_children(
    tree: &mut GoScopeTree,
    node: Node,
    content: &[u8],
    scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        build_scopes_recursive(tree, child, content, scope, guard)?;
    }
    Ok(())
}

// ============================================================================
// Phase 2: Bind declarations
// ============================================================================

fn bind_declarations_recursive(
    tree: &mut GoScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

    match node.kind() {
        "short_var_declaration" => {
            bind_short_var_declaration(tree, node, content);
        }
        "var_declaration" => {
            bind_var_declaration(tree, node, content);
        }
        "for_statement" => {
            bind_for_range_variables(tree, node, content);
        }
        "function_declaration" => {
            bind_function_parameters(tree, node, content);
        }
        "method_declaration" => {
            bind_method_parameters(tree, node, content);
        }
        "func_literal" => {
            bind_func_literal_parameters(tree, node, content);
        }
        "type_switch_statement" => {
            bind_type_switch_variable(tree, node, content);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        bind_declarations_recursive(tree, child, content, guard)?;
    }

    guard.exit();
    Ok(())
}

/// Bind variables from `:=` short declarations: `x, y := expr1, expr2`
fn bind_short_var_declaration(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    // Only bind function-local short declarations (not package-level)
    if is_package_level(node) {
        return;
    }

    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // Left side has the names in an `expression_list`
    if let Some(left) = node.child_by_field_name("left") {
        bind_expression_list_names(tree, scope_id, left, node, content);
    }
}

/// Bind variables from `var` declarations inside functions.
fn bind_var_declaration(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    if is_package_level(node) {
        return;
    }

    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // Process all var_spec children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "var_spec" {
            bind_var_spec(tree, scope_id, child, content);
        }
    }
}

/// Bind names from a single `var_spec` node.
fn bind_var_spec(tree: &mut GoScopeTree, scope_id: ScopeId, spec: Node, content: &[u8]) {
    let mut cursor = spec.walk();
    let initializer_start = spec.child_by_field_name("value").map(|v| v.start_byte());

    for name_node in spec.children_by_field_name("name", &mut cursor) {
        let name = name_node.utf8_text(content).unwrap_or("");
        if !name.is_empty() && name != "_" {
            tree.add_binding(
                scope_id,
                name,
                name_node.start_byte(),
                name_node.end_byte(),
                spec.end_byte(),
                initializer_start,
            );
        }
    }
}

/// Bind for-range loop variables: `for k, v := range expr { ... }`
fn bind_for_range_variables(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // Look for range_clause child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "range_clause" {
            // Range clause has `left` and `right` fields.
            // Use the range_clause as declarator (not the for_statement) so
            // self-reference prevention does not cover the entire loop body.
            if let Some(left) = child.child_by_field_name("left") {
                bind_expression_list_names(tree, scope_id, left, child, content);
            }
        }
    }

    // Also handle C-style for loop init: `for i := 0; i < n; i++ { ... }`
    // The init is a short_var_declaration which is handled by bind_short_var_declaration
}

/// Bind function parameters.
fn bind_function_parameters(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    if let Some(params) = node.child_by_field_name("parameters") {
        bind_parameter_list(tree, scope_id, params, content);
    }
}

/// Bind method parameters (including receiver).
fn bind_method_parameters(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    // Bind receiver parameter
    if let Some(receiver) = node.child_by_field_name("receiver") {
        bind_parameter_list(tree, scope_id, receiver, content);
    }

    // Bind regular parameters
    if let Some(params) = node.child_by_field_name("parameters") {
        bind_parameter_list(tree, scope_id, params, content);
    }
}

/// Bind function literal parameters.
fn bind_func_literal_parameters(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    if let Some(params) = node.child_by_field_name("parameters") {
        bind_parameter_list(tree, scope_id, params, content);
    }
}

/// Bind type switch variable: `switch x := expr.(type) { ... }`
fn bind_type_switch_variable(tree: &mut GoScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // Look for short_var_declaration or assignment as the value field
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // The alias is the left side of `:=` in the type switch header
        if child.kind() == "expression_list" {
            bind_expression_list_names(tree, scope_id, child, node, content);
        }
    }
}

// ============================================================================
// Binding helpers
// ============================================================================

/// Bind all identifier names from an expression_list (used by := and range).
fn bind_expression_list_names(
    tree: &mut GoScopeTree,
    scope_id: ScopeId,
    list: Node,
    declarator: Node,
    content: &[u8],
) {
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if child.kind() == "identifier" {
            let name = child.utf8_text(content).unwrap_or("");
            if !name.is_empty() && name != "_" {
                tree.add_binding(
                    scope_id,
                    name,
                    child.start_byte(),
                    child.end_byte(),
                    declarator.end_byte(),
                    None, // Short declarations don't have separate initializer tracking
                );
            }
        }
    }
}

/// Bind parameters from a parameter_list.
fn bind_parameter_list(tree: &mut GoScopeTree, scope_id: ScopeId, params: Node, content: &[u8]) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            // parameter_declaration has one or more names and a type
            let mut name_cursor = child.walk();
            for name_node in child.children_by_field_name("name", &mut name_cursor) {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() && name != "_" {
                    tree.add_binding(
                        scope_id,
                        name,
                        name_node.start_byte(),
                        name_node.end_byte(),
                        name_node.end_byte(),
                        None,
                    );
                }
            }
        } else if child.kind() == "variadic_parameter_declaration"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = name_node.utf8_text(content).unwrap_or("");
            if !name.is_empty() && name != "_" {
                tree.add_binding(
                    scope_id,
                    name,
                    name_node.start_byte(),
                    name_node.end_byte(),
                    name_node.end_byte(),
                    None,
                );
            }
        }
    }
}

/// Check if a node is at package level (parent is source_file).
fn is_package_level(node: Node) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "source_file")
}

// ============================================================================
// Resolution integration
// ============================================================================

/// Handle an identifier node for potential local variable reference.
///
/// This is the integration point called from `graph_builder.rs`. It:
/// 1. Extracts the identifier text
/// 2. Skips declaration contexts, type identifiers, function names, etc.
/// 3. Resolves via the scope tree
/// 4. Creates a Reference edge if matched
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut GoScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier = node.utf8_text(content).unwrap_or("");
    if identifier.is_empty() || identifier == "_" {
        return;
    }

    // Skip if this is a declaration context (left side of := or var)
    if is_declaration_context(node) {
        return;
    }

    // Skip type identifiers and function/method names
    if is_type_or_call_context(node) {
        return;
    }

    // Skip field access (after `.`)
    if is_field_access(node) {
        return;
    }

    // Skip package-level identifiers (not inside a function)
    if !is_inside_function(node) {
        return;
    }

    // Resolve via scope tree
    let usage_byte = node.start_byte();
    match scope_tree.resolve_identifier(usage_byte, identifier) {
        local_scopes::ResolutionOutcome::Local(binding) => {
            let target_id = if let Some(node_id) = binding.node_id {
                node_id
            } else {
                let span = Span::from_bytes(binding.decl_start_byte, binding.decl_end_byte);
                let qualified_var = format!("{identifier}@{}", binding.decl_start_byte);
                let var_id = helper.add_variable(&qualified_var, Some(span));
                scope_tree.attach_node_id(identifier, binding.decl_start_byte, var_id);
                var_id
            };
            add_reference_edge(node, identifier, target_id, helper);
        }
        local_scopes::ResolutionOutcome::Member { .. }
        | local_scopes::ResolutionOutcome::Ambiguous
        | local_scopes::ResolutionOutcome::NoMatch => {}
    }
}

/// Create a Reference edge from a usage site to a variable declaration.
fn add_reference_edge(
    usage_node: Node,
    identifier: &str,
    target_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let usage_span = Span::from_bytes(usage_node.start_byte(), usage_node.end_byte());
    let usage_id = helper.add_node(
        &format!("{identifier}@{}", usage_node.start_byte()),
        Some(usage_span),
        sqry_core::graph::unified::node::NodeKind::Variable,
    );
    helper.add_reference_edge(usage_id, target_id);
}

// ============================================================================
// Context detection helpers
// ============================================================================

/// Check if an identifier is in a declaration context (should not be resolved).
fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Left side of short var declaration
        "expression_list" => {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "short_var_declaration" {
                    return grandparent
                        .child_by_field_name("left")
                        .is_some_and(|left| left.id() == parent.id());
                }
                // Range clause left side
                if grandparent.kind() == "range_clause" {
                    return grandparent
                        .child_by_field_name("left")
                        .is_some_and(|left| left.id() == parent.id());
                }
            }
            false
        }
        // var_spec name field
        "var_spec" | "const_spec" => {
            parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == node.id())
                || is_name_child_of_spec(node, parent)
        }
        // Parameter declaration name
        "parameter_declaration" | "variadic_parameter_declaration" => {
            parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == node.id())
                || is_name_child_of_param(node, parent)
        }
        // Function/method name
        "function_declaration" | "method_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id()),
        // Label identifier
        "label_name" | "labeled_statement" => true,
        _ => false,
    }
}

/// Check if an identifier is a type name or function call name.
fn is_type_or_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Type identifier context
        "type_identifier" => true,
        // Function name in call expression
        "call_expression" => parent
            .child_by_field_name("function")
            .is_some_and(|f| f.id() == node.id()),
        // Package name in qualified identifier
        "qualified_type" | "package_identifier" => true,
        // Import spec
        "import_spec" => true,
        // Type conversion
        "type_conversion_expression" => parent
            .child_by_field_name("type")
            .is_some_and(|t| t.id() == node.id()),
        _ => false,
    }
}

/// Check if an identifier is accessed via `.` (field/method access).
fn is_field_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    if parent.kind() == "selector_expression" {
        // Skip the field name (right side of `.`)
        return parent
            .child_by_field_name("field")
            .is_some_and(|f| f.id() == node.id());
    }

    false
}

/// Check if a node is inside a function/method body.
fn is_inside_function(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" | "method_declaration" | "func_literal" => return true,
            "source_file" => return false,
            _ => current = parent.parent(),
        }
    }
    false
}

/// Check if node is a name child in a var_spec/const_spec with multiple names.
fn is_name_child_of_spec(node: Node, spec: Node) -> bool {
    let mut cursor = spec.walk();
    for name_node in spec.children_by_field_name("name", &mut cursor) {
        if name_node.id() == node.id() {
            return true;
        }
    }
    false
}

/// Check if node is a name child of a parameter_declaration.
fn is_name_child_of_param(node: Node, param: Node) -> bool {
    let mut cursor = param.walk();
    for name_node in param.children_by_field_name("name", &mut cursor) {
        if name_node.id() == node.id() {
            return true;
        }
    }
    false
}
