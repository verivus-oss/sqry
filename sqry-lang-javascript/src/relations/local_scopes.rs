//! Local scope tracking and reference resolution for JavaScript.
//!
//! JavaScript has block-scoped variables (`let`/`const`) and function-scoped
//! `var`. Variable sources:
//! - `lexical_declaration` (`let x = 5;`, `const x = expr;`)
//! - `variable_declaration` (`var x = 5;`)
//! - Parameters (identifiers or patterns in `formal_parameters`)
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
// JavaScript-specific ScopeKind
// ============================================================================

/// JavaScript scope kinds. Block-scoped for `let`/`const`, function-scoped for `var`.
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
    /// C-style for loop.
    ForLoop,
    /// For-in loop.
    ForInLoop,
    /// For-of loop.
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
        false
    }

    fn is_overlap_boundary(&self) -> bool {
        false
    }

    fn allows_nested_shadowing(&self) -> bool {
        true
    }
}

/// Type alias for the JavaScript-specialized scope tree.
pub(crate) type JavaScriptScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a JavaScript source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<JavaScriptScopeTree> {
    let content_len = content.len();
    let mut tree = JavaScriptScopeTree::new(content_len);

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

#[allow(clippy::too_many_lines)] // JS scope resolution covers all AST node types
fn build_scopes_recursive(
    tree: &mut JavaScriptScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "function_declaration"
        | "function_expression"
        | "generator_function_declaration"
        | "generator_function" => {
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
        #[allow(clippy::match_same_arms)]
        // for_in and for_of have same binding logic but distinct AST semantics
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
            let parent_kind = node.parent().map(|p| p.kind());
            let already_scoped = matches!(
                parent_kind,
                Some(
                    "function_declaration"
                        | "function_expression"
                        | "generator_function_declaration"
                        | "generator_function"
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

    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

fn recurse_children(
    tree: &mut JavaScriptScopeTree,
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
    tree: &mut JavaScriptScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                bind_variable_declarators(tree, scope_id, node, content);
            }
        }
        #[allow(clippy::match_same_arms)]
        // Arms separated by AST node type for documentation clarity
        "function_declaration"
        | "function_expression"
        | "generator_function_declaration"
        | "generator_function" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_parameters(tree, scope_id, params, content);
            }
        }

        "arrow_function" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                if let Some(params) = node.child_by_field_name("parameters") {
                    if params.kind() == "formal_parameters" {
                        bind_parameters(tree, scope_id, params, content);
                    } else {
                        // Single identifier parameter: `x => x + 1`
                        bind_pattern(tree, scope_id, params, params.end_byte(), None, content);
                    }
                }
                if let Some(param) = node.child_by_field_name("parameter") {
                    bind_pattern(tree, scope_id, param, param.end_byte(), None, content);
                }
            }
        }

        "method_definition" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_parameters(tree, scope_id, params, content);
            }
        }

        #[allow(clippy::match_same_arms)]
        // for_in and for_of have same binding logic but distinct AST semantics
        "for_in_statement" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(left) = node.child_by_field_name("left")
            {
                bind_for_loop_variable(tree, scope_id, left, content);
            }
        }

        "for_of_statement" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(left) = node.child_by_field_name("left")
            {
                bind_for_loop_variable(tree, scope_id, left, content);
            }
        }

        "catch_clause" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                bind_catch_parameter(tree, scope_id, node, content);
            }
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

// ============================================================================
// Binding helpers
// ============================================================================

/// Bind all `variable_declarator` children.
fn bind_variable_declarators(
    tree: &mut JavaScriptScopeTree,
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
///
/// In JavaScript, `formal_parameters` directly contains identifiers, patterns,
/// `assignment_pattern`s, and `rest_pattern`s, with no `required_parameter` wrapper.
fn bind_parameters(
    tree: &mut JavaScriptScopeTree,
    scope_id: ScopeId,
    params: Node,
    content: &[u8],
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "identifier" | "array_pattern" | "object_pattern" | "assignment_pattern"
            | "rest_pattern" => {
                bind_pattern(tree, scope_id, child, child.end_byte(), None, content);
            }
            _ => {}
        }
    }
}

/// Bind a pattern node such as `identifier`, `array_pattern`, or `object_pattern`.
#[allow(clippy::too_many_lines)] // Pattern binding covers all JS destructuring forms
fn bind_pattern(
    tree: &mut JavaScriptScopeTree,
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
                    "rest_element" | "rest_pattern" => {
                        bind_rest_inner(
                            tree,
                            scope_id,
                            child,
                            declarator_end,
                            initializer_start,
                            content,
                        );
                    }
                    _ => {}
                }
            }
        }
        "object_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
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
                    "rest_element" | "rest_pattern" => {
                        bind_rest_inner(
                            tree,
                            scope_id,
                            child,
                            declarator_end,
                            initializer_start,
                            content,
                        );
                    }
                    _ => {}
                }
            }
        }
        "assignment_pattern" => {
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
            bind_rest_inner(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
            );
        }
        _ => {}
    }
}

/// Extract and bind the inner identifier/pattern from a rest element.
fn bind_rest_inner(
    tree: &mut JavaScriptScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
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
                return;
            }
            _ => {}
        }
    }
}

/// Bind for-in/for-of loop variable.
fn bind_for_loop_variable(
    tree: &mut JavaScriptScopeTree,
    scope_id: ScopeId,
    left: Node,
    content: &[u8],
) {
    match left.kind() {
        "identifier" => {
            if let Ok(name) = left.utf8_text(content)
                && !name.is_empty()
            {
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
                    bind_pattern(tree, scope_id, name_node, left.end_byte(), None, content);
                }
            }
        }
        _ => {}
    }
}

/// Bind catch clause parameter.
fn bind_catch_parameter(
    tree: &mut JavaScriptScopeTree,
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
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut JavaScriptScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier = node.utf8_text(content).unwrap_or("");
    if identifier.is_empty() || identifier == "_" {
        return;
    }

    if is_declaration_context(node) {
        return;
    }

    if is_call_context(node) {
        return;
    }

    if is_member_access(node) {
        return;
    }

    if !is_inside_function(node) {
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
                helper.mark_definition(var_id);
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

fn add_reference_edge(
    usage_node: Node,
    identifier: &str,
    target_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let usage_span = Span::from_node(&usage_node);
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

fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    match parent.kind() {
        "variable_declarator" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        "function_declaration"
        | "function_expression"
        | "generator_function_declaration"
        | "generator_function" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // In JS, parameters are directly in formal_parameters as identifiers
        "formal_parameters" => true,
        "shorthand_property_identifier_pattern" | "shorthand_property_identifier" => true,
        "pair_pattern" => parent
            .child_by_field_name("key")
            .is_some_and(|n| n.id() == node.id()),
        "assignment_pattern" => parent
            .child_by_field_name("left")
            .is_some_and(|n| n.id() == node.id()),
        "pair" => parent
            .child_by_field_name("key")
            .is_some_and(|n| n.id() == node.id()),
        "for_in_statement" | "for_of_statement" => parent
            .child_by_field_name("left")
            .is_some_and(|n| n.id() == node.id()),
        "labeled_statement" => parent
            .child_by_field_name("label")
            .is_some_and(|n| n.id() == node.id()),
        "class_declaration" | "class" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        "method_definition" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        "catch_clause" => parent
            .child_by_field_name("parameter")
            .is_some_and(|n| n.id() == node.id()),
        "import_specifier" | "import_clause" | "namespace_import" => true,
        "export_specifier" => true,
        _ => false,
    }
}

fn is_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        "call_expression" => parent
            .child_by_field_name("function")
            .is_some_and(|n| n.id() == node.id()),
        "new_expression" => parent
            .child_by_field_name("constructor")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

fn is_member_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        "member_expression" => parent
            .child_by_field_name("property")
            .is_some_and(|n| n.id() == node.id()),
        "property_identifier" => true,
        _ => false,
    }
}

fn is_inside_function(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "generator_function"
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
