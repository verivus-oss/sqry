//! Local scope tracking and reference resolution for TypeScript.
//!
//! TypeScript has block-scoped variables (`let`/`const`) and function-scoped
//! `var`. Variable sources:
//! - `lexical_declaration` (`let x = 5;`, `const x = expr;`)
//! - `variable_declaration` (`var x = 5;`)
//! - `required_parameter` / `optional_parameter` / `rest_parameter`
//! - `for_in_statement` / `for_of_statement` loop variables
//! - `for_statement` initialiser variables
//! - `catch_clause` exception variable
//! - Destructuring: `array_pattern` and `object_pattern`
//!
//! Shadowing is allowed in nested scopes.

use sqry_core::graph::local_scopes::{self, ScopeId, ScopeKindTrait, ScopeTree};
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::{GraphResult, Span};
use tree_sitter::Node;

// ============================================================================
// TypeScript-specific ScopeKind
// ============================================================================

/// TypeScript scope kinds. Block-scoped for `let`/`const`, function-scoped for `var`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    /// Function body (`function foo() { ... }`).
    Function,
    /// Arrow function body (`const f = () => { ... }`).
    ArrowFunction,
    /// Method body (`class C { method() { ... } }`), including constructors.
    Method,
    /// Generic block `{ ... }`.
    Block,
    /// If-statement branch (then or else).
    IfBranch,
    /// C-style for loop (`for (let i = 0; ...; ...) { ... }`).
    ForLoop,
    /// For-in loop (`for (const key in obj) { ... }`).
    ForInLoop,
    /// For-of loop (`for (const item of arr) { ... }`).
    ForOfLoop,
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
    /// Switch case.
    SwitchCase,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        false // TypeScript class members are separate from local scopes
    }

    fn is_overlap_boundary(&self) -> bool {
        false // No overlap boundaries
    }

    fn allows_nested_shadowing(&self) -> bool {
        true // TypeScript allows shadowing in nested scopes
    }
}

/// Type alias for the TypeScript-specialized scope tree.
pub(crate) type TypeScriptScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a TypeScript source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<TypeScriptScopeTree> {
    let content_len = content.len();
    let mut tree = TypeScriptScopeTree::new(content_len);

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
    tree: &mut TypeScriptScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

    match node.kind() {
        "function_declaration" | "function_expression" | "generator_function_declaration" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Function,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "arrow_function" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::ArrowFunction,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "method_definition" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Method,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        // For loops — scope covers the entire for_statement so init vars are in scope for the body
        "for_statement" => {
            if node.child_by_field_name("body").is_some()
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::ForLoop,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "for_in_statement" => {
            if node.child_by_field_name("body").is_some()
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::ForInLoop,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "for_of_statement" => {
            if node.child_by_field_name("body").is_some()
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::ForOfLoop,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "while_statement" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::WhileLoop,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "do_statement" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::DoWhileLoop,
                    node.start_byte(),
                    node.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "if_statement" => {
            // Create scope for the then branch (consequence)
            if let Some(consequence) = node.child_by_field_name("consequence")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::IfBranch,
                    consequence.start_byte(),
                    consequence.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, consequence, content, Some(scope_id), guard)?;
            }
            // Create scope for the else branch (alternative)
            if let Some(alternative) = node.child_by_field_name("alternative")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::IfBranch,
                    alternative.start_byte(),
                    alternative.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, alternative, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "switch_statement" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::SwitchBlock,
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
        "switch_case" | "switch_default" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::SwitchCase,
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
            // Process try body
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::TryBlock,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            // Process catch handler
            if let Some(handler) = node.child_by_field_name("handler")
                && let Some(body) = handler.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::CatchBlock,
                    handler.start_byte(),
                    handler.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            // Process finally
            if let Some(finalizer) = node.child_by_field_name("finalizer")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::FinallyBlock,
                    finalizer.start_byte(),
                    finalizer.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, finalizer, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "statement_block" => {
            // Only create a block scope if the parent isn't already creating one
            let parent_kind = node.parent().map(|p| p.kind());
            let already_scoped = matches!(
                parent_kind,
                Some(
                    "function_declaration"
                        | "function_expression"
                        | "generator_function_declaration"
                        | "arrow_function"
                        | "method_definition"
                        | "for_statement"
                        | "for_in_statement"
                        | "for_of_statement"
                        | "while_statement"
                        | "do_statement"
                        | "if_statement"
                        | "try_statement"
                        | "catch_clause"
                        | "finally_clause"
                        | "switch_statement"
                )
            );
            if !already_scoped
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

    // Default: recurse into children with current scope
    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

fn recurse_children(
    tree: &mut TypeScriptScopeTree,
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

#[allow(clippy::too_many_lines)]
fn bind_declarations_recursive(
    tree: &mut TypeScriptScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

    match node.kind() {
        // let/const declarations
        "lexical_declaration" | "variable_declaration" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                bind_variable_declarators(tree, scope_id, node, content);
            }
        }

        // Function declarations — bind parameters
        "function_declaration" | "function_expression" | "generator_function_declaration" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_parameters(tree, scope_id, params, content);
            }
        }

        // Arrow functions — bind parameters
        "arrow_function" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                // Arrow functions can have `(x, y)` formal_parameters or a single `x` parameter
                if let Some(params) = node.child_by_field_name("parameters") {
                    if params.kind() == "formal_parameters" {
                        bind_parameters(tree, scope_id, params, content);
                    } else {
                        // Single identifier parameter: `x => x + 1`
                        bind_identifier_as_param(tree, scope_id, params, content);
                    }
                }
                // Also handle the case where the parameter is the first child as identifier
                if let Some(param) = node.child_by_field_name("parameter") {
                    bind_identifier_as_param(tree, scope_id, param, content);
                }
            }
        }

        // Method definitions — bind parameters
        "method_definition" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_parameters(tree, scope_id, params, content);
            }
        }

        // For-in loop: `for (const key in obj)`
        "for_in_statement" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(left) = node.child_by_field_name("left")
            {
                bind_for_loop_variable(tree, scope_id, left, node, content);
            }
        }

        // For-of loop: `for (const item of arr)`
        "for_of_statement" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(left) = node.child_by_field_name("left")
            {
                bind_for_loop_variable(tree, scope_id, left, node, content);
            }
        }

        // Catch clause: `catch (error)`
        "catch_clause" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                bind_catch_parameter(tree, scope_id, node, content);
            }
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

// ============================================================================
// Binding helpers
// ============================================================================

/// Bind all `variable_declarator` children in a `lexical_declaration` or `variable_declaration`.
fn bind_variable_declarators(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    decl: Node,
    content: &[u8],
) {
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let initializer_start = child.child_by_field_name("value").map(|v| v.start_byte());
            // Handle destructuring patterns or simple identifiers
            bind_pattern(
                tree,
                scope_id,
                name_node,
                child.end_byte(),
                initializer_start,
                content,
            );
        }
    }
}

/// Bind all parameters from a `formal_parameters` node.
fn bind_parameters(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    params: Node,
    content: &[u8],
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "required_parameter" | "optional_parameter" => {
                // Parameters can have a "pattern" field (for destructuring) or direct "name" field
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    bind_pattern(tree, scope_id, pattern, child.end_byte(), None, content);
                } else if let Some(name_node) = child.child_by_field_name("name") {
                    // name can be: identifier, rest_pattern, array_pattern, object_pattern
                    bind_pattern(tree, scope_id, name_node, child.end_byte(), None, content);
                } else {
                    // Fallback: search for identifier child directly
                    bind_identifier_as_param(tree, scope_id, child, content);
                }
            }
            _ => {}
        }
    }
}

/// Bind a single identifier node as a parameter.
fn bind_identifier_as_param(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    node: Node,
    content: &[u8],
) {
    // Look for the identifier within the node
    if node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(content)
            && !name.is_empty()
        {
            tree.add_binding(
                scope_id,
                name,
                node.start_byte(),
                node.end_byte(),
                node.end_byte(),
                None,
            );
        }
        return;
    }
    // Search children for an identifier
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            if let Ok(name) = child.utf8_text(content)
                && !name.is_empty()
            {
                tree.add_binding(
                    scope_id,
                    name,
                    child.start_byte(),
                    child.end_byte(),
                    child.end_byte(),
                    None,
                );
            }
            return;
        }
    }
}

/// Bind a pattern node (identifier, array_pattern, or object_pattern).
fn bind_pattern(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    match node.kind() {
        "identifier" => {
            if let Ok(name) = node.utf8_text(content)
                && !name.is_empty()
            {
                tree.add_binding(
                    scope_id,
                    name,
                    node.start_byte(),
                    node.end_byte(),
                    declarator_end,
                    initializer_start,
                );
            }
        }
        "array_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "identifier" | "array_pattern" | "object_pattern" | "assignment_pattern" => {
                        bind_pattern(
                            tree,
                            scope_id,
                            child,
                            declarator_end,
                            initializer_start,
                            content,
                        );
                    }
                    "rest_element" => {
                        // `...rest` inside array destructuring
                        let mut inner = child.walk();
                        for item in child.children(&mut inner) {
                            if item.kind() == "identifier"
                                || item.kind() == "array_pattern"
                                || item.kind() == "object_pattern"
                            {
                                bind_pattern(
                                    tree,
                                    scope_id,
                                    item,
                                    declarator_end,
                                    initializer_start,
                                    content,
                                );
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "object_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    // `{ x }` shorthand — identifier is both property name and binding
                    "shorthand_property_identifier_pattern" | "shorthand_property_identifier" => {
                        if let Ok(name) = child.utf8_text(content)
                            && !name.is_empty()
                        {
                            tree.add_binding(
                                scope_id,
                                name,
                                child.start_byte(),
                                child.end_byte(),
                                declarator_end,
                                initializer_start,
                            );
                        }
                    }
                    // `{ x: renamed }` — bind the value part (renamed), not the key (x)
                    "pair_pattern" => {
                        if let Some(value) = child.child_by_field_name("value") {
                            bind_pattern(
                                tree,
                                scope_id,
                                value,
                                declarator_end,
                                initializer_start,
                                content,
                            );
                        }
                    }
                    "rest_element" => {
                        // `{ ...rest }`
                        let mut inner = child.walk();
                        for item in child.children(&mut inner) {
                            if item.kind() == "identifier"
                                || item.kind() == "object_pattern"
                                || item.kind() == "array_pattern"
                            {
                                bind_pattern(
                                    tree,
                                    scope_id,
                                    item,
                                    declarator_end,
                                    initializer_start,
                                    content,
                                );
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "assignment_pattern" => {
            // `x = defaultValue` — bind the left side
            if let Some(left) = node.child_by_field_name("left") {
                bind_pattern(
                    tree,
                    scope_id,
                    left,
                    declarator_end,
                    initializer_start,
                    content,
                );
            }
        }
        "rest_pattern" | "rest_element" => {
            // `...rest` — bind the inner identifier/pattern
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "identifier" | "array_pattern" | "object_pattern" => {
                        bind_pattern(
                            tree,
                            scope_id,
                            child,
                            declarator_end,
                            initializer_start,
                            content,
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Bind for-in/for-of loop variable.
fn bind_for_loop_variable(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    left: Node,
    _for_node: Node,
    content: &[u8],
) {
    // left is either an identifier or a lexical_declaration/variable_declaration
    // containing variable_declarator(s)
    match left.kind() {
        "identifier" => {
            if let Ok(name) = left.utf8_text(content)
                && !name.is_empty()
            {
                // Use the left node (not the for_node) as the declarator to avoid
                // self-reference prevention blocking usages in the loop body
                tree.add_binding(
                    scope_id,
                    name,
                    left.start_byte(),
                    left.end_byte(),
                    left.end_byte(),
                    None,
                );
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = left.walk();
            for child in left.children(&mut cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    // Use left (the declaration) as the declarator — NOT for_node
                    bind_pattern(tree, scope_id, name_node, left.end_byte(), None, content);
                }
            }
        }
        _ => {}
    }
}

/// Bind catch clause parameter: `catch (error)`.
fn bind_catch_parameter(
    tree: &mut TypeScriptScopeTree,
    scope_id: ScopeId,
    catch_node: Node,
    content: &[u8],
) {
    if let Some(param) = catch_node.child_by_field_name("parameter") {
        bind_pattern(tree, scope_id, param, param.end_byte(), None, content);
    }
}

// ============================================================================
// Identifier resolution + graph integration
// ============================================================================

/// Handle an identifier node encountered during edge building.
///
/// Resolves the identifier to a local variable declaration and creates a
/// `References` edge from the usage site to the declaration.
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut TypeScriptScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier = node.utf8_text(content).unwrap_or("");
    if identifier.is_empty() || identifier == "_" {
        return;
    }

    // Skip if this is a declaration context (left side of let/const/var)
    if is_declaration_context(node) {
        return;
    }

    // Skip type identifiers, function names, class names
    if is_type_or_call_context(node) {
        return;
    }

    // Skip member access (after `.`)
    if is_member_access(node) {
        return;
    }

    // Skip identifiers not inside a function (module-level)
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
        // Left side of variable_declarator: `let x = ...`
        "variable_declarator" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Function/method name
        "function_declaration" | "function_expression" | "generator_function_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Parameter name
        "required_parameter" | "optional_parameter" | "rest_parameter" => true,
        // Destructuring patterns
        "shorthand_property_identifier_pattern" | "shorthand_property_identifier" => true,
        // Object property key (not the value)
        "pair_pattern" => parent
            .child_by_field_name("key")
            .is_some_and(|n| n.id() == node.id()),
        // Assignment pattern left side
        "assignment_pattern" => parent
            .child_by_field_name("left")
            .is_some_and(|n| n.id() == node.id()),
        // Property name in object literal (key position)
        "pair" => parent
            .child_by_field_name("key")
            .is_some_and(|n| n.id() == node.id()),
        // For-in/for-of left side — single identifier (not wrapped in declaration)
        "for_in_statement" | "for_of_statement" => parent
            .child_by_field_name("left")
            .is_some_and(|n| n.id() == node.id()),
        // Label name
        "labeled_statement" => parent
            .child_by_field_name("label")
            .is_some_and(|n| n.id() == node.id()),
        // Class/interface name
        "class_declaration" | "class" | "interface_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Type alias name
        "type_alias_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Enum name
        "enum_declaration" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Method name
        "method_definition" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Catch parameter
        "catch_clause" => parent
            .child_by_field_name("parameter")
            .is_some_and(|n| n.id() == node.id()),
        // Import specifier
        "import_specifier" | "import_clause" | "namespace_import" => true,
        // Export specifier
        "export_specifier" => true,
        _ => false,
    }
}

/// Check if an identifier is a type or call context (should not be resolved as local var).
fn is_type_or_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Type annotations
        "type_annotation" | "type_identifier" | "predefined_type" | "generic_type"
        | "type_arguments" | "type_parameter" | "constraint" => true,
        // Extends/implements clauses
        "extends_clause" | "implements_clause" | "extends_type_clause" => true,
        // Call expression function name
        "call_expression" => parent
            .child_by_field_name("function")
            .is_some_and(|n| n.id() == node.id()),
        // new expression constructor name
        "new_expression" => parent
            .child_by_field_name("constructor")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

/// Check if an identifier is after a dot (member access — not a local reference).
fn is_member_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // `obj.field` — only the `field` (property) part should be skipped, not the `obj`
        "member_expression" => parent
            .child_by_field_name("property")
            .is_some_and(|n| n.id() == node.id()),
        // Property name in object literal `{ key: value }`
        "property_identifier" => true,
        _ => false,
    }
}

/// Check if an identifier is inside a function (not at module level).
fn is_inside_function(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "arrow_function"
            | "method_definition" => {
                return true;
            }
            "program" => return false,
            _ => {}
        }
        current = parent.parent();
    }
    false
}
