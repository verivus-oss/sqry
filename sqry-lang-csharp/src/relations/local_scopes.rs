//! Local scope tracking and reference resolution for C#.
//!
//! C# has block-scoped variables with class member resolution. Variable sources:
//! - `local_declaration_statement` (`int x = 5;`, `var x = expr;`)
//! - `parameter` (method/constructor/lambda parameters)
//! - `foreach_statement` (foreach loop variable)
//! - `for_statement` (for-loop init variables)
//! - `catch_clause` (catch exception variable)
//! - `using_statement` (using resource variable)
//! - `declaration_expression` (`out var x`, `is Type x`)
//!
//! Shadowing is allowed in nested scopes.

use sqry_core::graph::local_scopes::{self, ScopeId, ScopeKindTrait, ScopeTree};
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::{GraphResult, Span};
use tree_sitter::Node;

// ============================================================================
// C#-specific ScopeKind
// ============================================================================

/// C# scope kinds. Block-scoped with class member boundaries.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    /// Method body.
    Method,
    /// Constructor body.
    Constructor,
    /// Generic block `{ ... }`.
    Block,
    /// If-statement branch.
    IfBranch,
    /// For loop (C-style).
    ForLoop,
    /// Foreach loop.
    ForeachLoop,
    /// While loop.
    WhileLoop,
    /// Do-while loop.
    DoWhileLoop,
    /// Try block.
    TryBlock,
    /// Catch block.
    CatchBlock,
    /// Finally block.
    FinallyBlock,
    /// Switch statement.
    SwitchBlock,
    /// Switch section (case/default).
    SwitchSection,
    /// Using statement block.
    UsingBlock,
    /// Lambda expression body.
    Lambda,
    /// Local function body.
    LocalFunction,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        false // C# local classes are rare; skip for now
    }

    fn is_overlap_boundary(&self) -> bool {
        false
    }

    fn allows_nested_shadowing(&self) -> bool {
        true // C# allows shadowing in nested scopes
    }
}

/// Type alias for the C#-specialized scope tree.
pub(crate) type CSharpScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a C# source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<CSharpScopeTree> {
    let content_len = content.len();
    let mut tree = CSharpScopeTree::new(content_len);

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

#[allow(clippy::too_many_lines)] // C# scope resolution covers all AST node types
fn build_scopes_recursive(
    tree: &mut CSharpScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "method_declaration" | "operator_declaration" | "conversion_operator_declaration" => {
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
        "constructor_declaration" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Constructor,
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
        "local_function_statement" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::LocalFunction,
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
        "lambda_expression" | "anonymous_method_expression" => {
            // Lambda body can be a block or an expression
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Lambda,
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
        "accessor_declaration" => {
            // Property getter/setter body
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
        "foreach_statement" | "for_each_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::ForeachLoop,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "while_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::WhileLoop,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "do_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::DoWhileLoop,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "try_statement" => {
            // Create scope for the try body
            if let Some(body) = local_scopes::first_child_of_kind(node, "block")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::TryBlock,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            // Process catch and finally clauses
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "catch_clause" => {
                        if let Some(body) = local_scopes::first_child_of_kind(child, "block")
                            && let Some(scope_id) = tree.add_scope(
                                ScopeKind::CatchBlock,
                                child.start_byte(),
                                body.end_byte(),
                                current_scope,
                            )
                        {
                            recurse_children(tree, child, content, Some(scope_id), guard)?;
                        }
                    }
                    "finally_clause" => {
                        if let Some(body) = local_scopes::first_child_of_kind(child, "block")
                            && let Some(scope_id) = tree.add_scope(
                                ScopeKind::FinallyBlock,
                                body.start_byte(),
                                body.end_byte(),
                                current_scope,
                            )
                        {
                            recurse_children(tree, body, content, Some(scope_id), guard)?;
                        }
                    }
                    _ => {}
                }
            }
            guard.exit();
            return Ok(());
        }
        "switch_statement" => {
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
        "switch_section" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::SwitchSection,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "using_statement" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::UsingBlock,
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
            // Create block scope only if not already scoped by parent handler
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
        }
        _ => {}
    }

    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

/// Build if-statement scopes.
fn build_if_statement_scopes(
    tree: &mut CSharpScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    let if_scope = tree.add_scope(
        ScopeKind::IfBranch,
        node.start_byte(),
        node.end_byte(),
        current_scope,
    );
    let scope = if_scope.unwrap_or(current_scope.unwrap_or(0));

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "if_statement" {
            // else-if chain
            build_if_statement_scopes(tree, child, content, Some(scope), guard)?;
        } else {
            build_scopes_recursive(tree, child, content, Some(scope), guard)?;
        }
    }

    Ok(())
}

/// Check if a block node is a function/method/constructor/lambda body.
fn is_function_body(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "method_declaration"
                | "constructor_declaration"
                | "local_function_statement"
                | "lambda_expression"
                | "anonymous_method_expression"
                | "accessor_declaration"
                | "operator_declaration"
                | "conversion_operator_declaration"
        ) && parent
            .child_by_field_name("body")
            .is_some_and(|body| body.id() == node.id())
    })
}

/// Recurse into all children.
fn recurse_children(
    tree: &mut CSharpScopeTree,
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
    tree: &mut CSharpScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "local_declaration_statement" => {
            bind_local_declaration(tree, node, content);
        }
        "for_statement" => {
            bind_for_variables(tree, node, content);
        }
        "foreach_statement" | "for_each_statement" => {
            bind_foreach_variable(tree, node, content);
        }
        "method_declaration" | "operator_declaration" | "conversion_operator_declaration" => {
            bind_method_parameters(tree, node, content);
        }
        "constructor_declaration" => {
            bind_constructor_parameters(tree, node, content);
        }
        "local_function_statement" => {
            bind_local_function_parameters(tree, node, content);
        }
        "lambda_expression" | "anonymous_method_expression" => {
            bind_lambda_parameters(tree, node, content);
        }
        "catch_clause" => {
            bind_catch_parameter(tree, node, content);
        }
        "using_statement" => {
            bind_using_variable(tree, node, content);
        }
        "declaration_expression" => {
            bind_declaration_expression(tree, node, content);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        bind_declarations_recursive(tree, child, content, guard)?;
    }

    guard.exit();
    Ok(())
}

/// Bind local variable declarations: `int x = 5;`, `var x, y = ...;`
fn bind_local_declaration(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    if !is_inside_method(node) {
        return;
    }

    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // local_declaration_statement → variable_declaration → variable_declarator(s)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            bind_variable_declaration(tree, scope_id, child, content);
        }
    }
}

/// Bind variable declarators from a `variable_declaration` node.
fn bind_variable_declaration(
    tree: &mut CSharpScopeTree,
    scope_id: ScopeId,
    decl: Node,
    content: &[u8],
) {
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
            && !name.is_empty()
        {
            let initializer_start = child
                .child_by_field_name("initializer")
                .map(|v| v.start_byte());
            tree.add_binding(
                scope_id,
                name,
                name_node.start_byte(),
                name_node.end_byte(),
                child.end_byte(),
                initializer_start,
            );
        }
    }
}

/// Bind for-loop init variables.
fn bind_for_variables(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // for_statement may have a variable_declaration in its initializer
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            bind_variable_declaration(tree, scope_id, child, content);
        }
    }
}

/// Bind foreach loop variable: `foreach (var item in collection)`
fn bind_foreach_variable(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // The loop variable is the identifier after the type
    if let Some(name_node) = node.child_by_field_name("left")
        && let Ok(name) = name_node.utf8_text(content)
        && !name.is_empty()
    {
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

/// Bind method parameters.
fn bind_method_parameters(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
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

/// Bind constructor parameters.
fn bind_constructor_parameters(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
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

/// Bind local function parameters.
fn bind_local_function_parameters(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
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

/// Bind lambda/anonymous method parameters.
fn bind_lambda_parameters(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    // Lambda can have parameter_list or a single parameter
    if let Some(params) = node.child_by_field_name("parameters") {
        bind_parameter_list(tree, scope_id, params, content);
    }
}

/// Bind catch clause exception variable: `catch (Exception ex)`
fn bind_catch_parameter(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // catch_clause → catch_declaration → identifier
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "catch_declaration"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
            && !name.is_empty()
        {
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

/// Bind using statement variable: `using (var stream = new FileStream(...))`
fn bind_using_variable(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // using_statement may contain a variable_declaration
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            bind_variable_declaration(tree, scope_id, child, content);
        }
    }
}

/// Bind declaration expression: `out var x`, `is Type x`
fn bind_declaration_expression(tree: &mut CSharpScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(content)
        && !name.is_empty()
    {
        tree.add_binding(
            scope_id,
            name,
            name_node.start_byte(),
            name_node.end_byte(),
            node.end_byte(),
            None,
        );
    }
}

// ============================================================================
// Binding helpers
// ============================================================================

/// Bind parameters from a `parameter_list`.
fn bind_parameter_list(
    tree: &mut CSharpScopeTree,
    scope_id: ScopeId,
    params: Node,
    content: &[u8],
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
            && !name.is_empty()
        {
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

/// Check if a node is inside a method/constructor body.
fn is_inside_method(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "local_function_statement"
            | "lambda_expression"
            | "anonymous_method_expression"
            | "accessor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration" => return true,
            "compilation_unit" => return false,
            _ => current = parent.parent(),
        }
    }
    false
}

// ============================================================================
// Resolution integration
// ============================================================================

/// Handle an identifier node for potential local variable reference.
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut CSharpScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let Ok(identifier) = node.utf8_text(content) else {
        return;
    };
    if identifier.is_empty() {
        return;
    }

    if is_declaration_context(node) {
        return;
    }

    if is_type_or_call_context(node) {
        return;
    }

    if is_member_access(node) {
        return;
    }

    if !is_inside_method(node) {
        return;
    }

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

/// Create a Reference edge from usage to declaration.
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

/// Check if an identifier is in a declaration context.
fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Variable declarator name
        "variable_declarator" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Parameter name
        "parameter" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Method/constructor/local function name
        "method_declaration"
        | "constructor_declaration"
        | "local_function_statement"
        | "property_declaration"
        | "event_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Class/struct/interface/enum name
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "enum_member_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Foreach loop variable
        "foreach_statement" | "for_each_statement" => parent
            .child_by_field_name("left")
            .is_some_and(|n| n.id() == node.id()),
        // Catch declaration name
        "catch_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Declaration expression name (out var x)
        "declaration_expression" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Namespace declaration name
        "namespace_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Label
        "labeled_statement" => true,
        _ => false,
    }
}

/// Check if an identifier is a type name or function call.
fn is_type_or_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    match parent.kind() {
        #[allow(clippy::match_same_arms)]
        // Arms separated by AST node type for documentation clarity
        // Type identifiers
        "predefined_type" | "generic_name" | "type_argument_list" => true,
        // Qualified name left side (namespace prefix)
        "qualified_name" => true,
        // Using directive
        "using_directive" => true,
        // Base list (inheritance)
        "base_list" | "simple_base_type" => true,
        // Attribute
        "attribute" => true,
        // Type in variable declaration
        "variable_declaration" if is_type_child(node, parent) => true,
        // Type in parameter
        "parameter" if is_type_child(node, parent) => true,
        // Type in object creation
        "object_creation_expression" => parent
            .child_by_field_name("type")
            .is_some_and(|t| t.id() == node.id()),
        // Type in cast/as/is
        "cast_expression" | "as_expression" | "is_expression" => parent
            .child_by_field_name("type")
            .is_some_and(|t| t.id() == node.id()),
        _ => false,
    }
}

/// Check if an identifier is a type child in a declaration.
fn is_type_child(node: Node, parent: Node) -> bool {
    parent
        .child_by_field_name("type")
        .is_some_and(|t| t.id() == node.id())
}

/// Check if an identifier is accessed via `.` (member access).
fn is_member_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    if parent.kind() == "member_access_expression" {
        // Skip the member name (right side of `.`)
        return parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id());
    }

    false
}
