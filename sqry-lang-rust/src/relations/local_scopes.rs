//! Local scope tracking and reference resolution for Rust.
//!
//! Rust has block-scoped variables with let bindings and idiomatic
//! shadowing (`let x = x + 1`). Variable sources:
//! - `let_declaration` (`let x = 5;`, `let mut x = expr;`)
//! - Function parameters (`fn foo(x: i32) { ... }`)
//! - Closure parameters (`|x| x + 1`)
//! - `for_expression` loop variables (`for x in iter { ... }`)
//! - `match_arm` pattern bindings
//! - `if_expression` with let pattern (`if let Some(x) = opt { ... }`)
//! - `while_expression` with let pattern (`while let Some(x) = iter.next() { ... }`)
//! - Destructuring patterns: `tuple_pattern`, `struct_pattern`, `slice_pattern`
//!
//! Shadowing is allowed and idiomatic in Rust.

use sqry_core::graph::local_scopes::{self, ScopeId, ScopeKindTrait, ScopeTree};
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::{GraphResult, Span};
use tree_sitter::Node;

// ============================================================================
// Rust-specific ScopeKind
// ============================================================================

/// Rust scope kinds. All scopes are block-scoped (no hoisting in Rust).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    /// Function body (`fn foo() { ... }`).
    Function,
    /// Method body (inside impl block).
    Method,
    /// Closure body (`|x| { ... }`).
    Closure,
    /// Generic block `{ ... }`.
    Block,
    /// If expression branch (then or else).
    IfBranch,
    /// If-let branch (creates bindings from pattern).
    IfLet,
    /// For loop.
    ForLoop,
    /// While loop.
    WhileLoop,
    /// While-let loop.
    WhileLet,
    /// Loop expression (infinite loop).
    Loop,
    /// Match arm.
    MatchArm,
    /// Unsafe block.
    UnsafeBlock,
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

/// Type alias for the Rust-specialized scope tree.
pub(crate) type RustScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a Rust source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<RustScopeTree> {
    let mut tree = RustScopeTree::new(content);

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

fn recurse_scoped_children(
    tree: &mut RustScopeTree,
    scope_kind: ScopeKind,
    scope_node: Node,
    child_root: Node,
    current_scope: Option<ScopeId>,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    if let Some(scope_id) = tree.add_scope(
        scope_kind,
        scope_node.start_byte(),
        scope_node.end_byte(),
        current_scope,
    ) {
        recurse_children(tree, child_root, content, Some(scope_id), guard)?;
    }
    Ok(())
}

fn recurse_scoped_children_and_exit(
    tree: &mut RustScopeTree,
    scope_kind: ScopeKind,
    scope_node: Node,
    child_root: Node,
    current_scope: Option<ScopeId>,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    recurse_scoped_children(
        tree,
        scope_kind,
        scope_node,
        child_root,
        current_scope,
        content,
        guard,
    )?;
    guard.exit();
    Ok(())
}

fn recurse_body_scope_and_exit(
    tree: &mut RustScopeTree,
    node: Node,
    body_scope_kind: ScopeKind,
    current_scope: Option<ScopeId>,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    if let Some(body) = node.child_by_field_name("body") {
        recurse_scoped_children_and_exit(
            tree,
            body_scope_kind,
            node,
            body,
            current_scope,
            content,
            guard,
        )?;
    } else {
        guard.exit();
    }
    Ok(())
}

fn recurse_same_node_scope_and_exit(
    tree: &mut RustScopeTree,
    scope_kind: ScopeKind,
    node: Node,
    current_scope: Option<ScopeId>,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    recurse_scoped_children_and_exit(tree, scope_kind, node, node, current_scope, content, guard)
}

fn recurse_children_and_exit(
    tree: &mut RustScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

fn recurse_if_expression_scopes_and_exit(
    tree: &mut RustScopeTree,
    node: Node,
    current_scope: Option<ScopeId>,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    let consequence_kind = if has_let_condition(node) {
        ScopeKind::IfLet
    } else {
        ScopeKind::IfBranch
    };
    if let Some(consequence) = node.child_by_field_name("consequence") {
        recurse_scoped_children(
            tree,
            consequence_kind,
            consequence,
            consequence,
            current_scope,
            content,
            guard,
        )?;
    }
    if let Some(alternative) = node.child_by_field_name("alternative") {
        recurse_scoped_children(
            tree,
            ScopeKind::IfBranch,
            alternative,
            alternative,
            current_scope,
            content,
            guard,
        )?;
    }
    guard.exit();
    Ok(())
}

fn scope_kind_for_body_node(node: Node) -> Option<ScopeKind> {
    match node.kind() {
        "function_item" => Some(if is_inside_impl(node) {
            ScopeKind::Method
        } else {
            ScopeKind::Function
        }),
        "closure_expression" => Some(ScopeKind::Closure),
        "for_expression" => Some(ScopeKind::ForLoop),
        "while_expression" => Some(if has_let_condition(node) {
            ScopeKind::WhileLet
        } else {
            ScopeKind::WhileLoop
        }),
        "loop_expression" => Some(ScopeKind::Loop),
        _ => None,
    }
}

fn should_create_block_scope(node: Node) -> bool {
    let parent_kind = node.parent().map(|parent| parent.kind());
    !matches!(
        parent_kind,
        Some(
            "function_item"
                | "closure_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "if_expression"
                | "match_arm"
                | "unsafe_block"
        )
    )
}

fn build_scopes_recursive(
    tree: &mut RustScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    if let Some(scope_kind) = scope_kind_for_body_node(node) {
        return recurse_body_scope_and_exit(tree, node, scope_kind, current_scope, content, guard);
    }

    match node.kind() {
        "if_expression" => {
            return recurse_if_expression_scopes_and_exit(
                tree,
                node,
                current_scope,
                content,
                guard,
            );
        }
        "match_expression" => {
            // Each match arm is its own scope; we don't create a scope for
            // the entire match expression itself since the arms handle it
            return recurse_children_and_exit(tree, node, content, current_scope, guard);
        }
        "match_arm" => {
            return recurse_same_node_scope_and_exit(
                tree,
                ScopeKind::MatchArm,
                node,
                current_scope,
                content,
                guard,
            );
        }
        "unsafe_block" => {
            return recurse_same_node_scope_and_exit(
                tree,
                ScopeKind::UnsafeBlock,
                node,
                current_scope,
                content,
                guard,
            );
        }
        "block" if should_create_block_scope(node) => {
            return recurse_same_node_scope_and_exit(
                tree,
                ScopeKind::Block,
                node,
                current_scope,
                content,
                guard,
            );
        }
        _ => {}
    }

    recurse_children(tree, node, content, current_scope, guard)?;
    guard.exit();
    Ok(())
}

fn recurse_children(
    tree: &mut RustScopeTree,
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
    tree: &mut RustScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "let_declaration" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(pattern) = node.child_by_field_name("pattern")
            {
                let initializer_start = node.child_by_field_name("value").map(|v| v.start_byte());
                bind_pattern(
                    tree,
                    scope_id,
                    pattern,
                    node.end_byte(),
                    initializer_start,
                    content,
                );
            }
        }
        "function_item" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_function_parameters(tree, scope_id, params, content);
            }
        }
        "closure_expression" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(params) = node.child_by_field_name("parameters")
            {
                bind_closure_parameters(tree, scope_id, params, content);
            }
        }
        "for_expression" | "match_arm" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(pattern) = node.child_by_field_name("pattern")
            {
                bind_pattern(tree, scope_id, pattern, pattern.end_byte(), None, content);
            }
        }
        "if_expression" | "while_expression" => {
            // Handle if-let / while-let patterns
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) {
                bind_let_condition(tree, scope_id, node, content);
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

fn bind_filtered_children<F>(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
    mut should_bind: F,
) where
    F: FnMut(Node) -> bool,
{
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if should_bind(child) {
            bind_pattern(
                tree,
                scope_id,
                child,
                declarator_end,
                initializer_start,
                content,
            );
        }
    }
}

fn bind_first_matching_child<F>(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
    mut should_bind: F,
) where
    F: FnMut(Node) -> bool,
{
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if should_bind(child) {
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
    }
}

fn bind_identifier_pattern(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    if let Ok(name) = node.utf8_text(content) {
        let name = name.trim();
        if !name.is_empty() && name != "_" {
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
}

fn bind_struct_pattern_fields(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    bind_filtered_children(
        tree,
        scope_id,
        node,
        declarator_end,
        initializer_start,
        content,
        |child| child.kind() == "field_pattern",
    );
}

/// Bind a pattern node (identifier, `mut_pattern`, `tuple_pattern`, and similar forms).
fn bind_pattern(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    match node.kind() {
        "identifier" => bind_identifier_pattern(
            tree,
            scope_id,
            node,
            declarator_end,
            initializer_start,
            content,
        ),
        "mut_pattern" | "ref_pattern" => {
            // `mut x` or `ref x` — descend to find the identifier
            bind_first_matching_child(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| matches!(child.kind(), "identifier" | "mut_pattern" | "ref_pattern"),
            );
        }
        "tuple_pattern" => {
            bind_filtered_children(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| !matches!(child.kind(), "(" | ")" | ","),
            );
        }
        "tuple_struct_pattern" => {
            // `Some(x)` or `Ok(val)` — the first identifier/scoped_identifier
            // is the type name; bind the remaining patterns.
            // Use `child_by_field_name("type")` to identify the type name node,
            // then bind everything else.
            let type_node_id = node.child_by_field_name("type").map(|n| n.id());
            bind_filtered_children(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| {
                    !matches!(child.kind(), "(" | ")" | ",")
                        && type_node_id.is_none_or(|id| child.id() != id)
                },
            );
        }
        "struct_pattern" => {
            bind_struct_pattern_fields(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
            );
        }
        "slice_pattern" => {
            bind_filtered_children(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| !matches!(child.kind(), "[" | "]" | ","),
            );
        }
        "or_pattern" => {
            // `A | B` — bind identifiers from the first arm only
            // (both arms should bind the same names)
            bind_first_matching_child(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| child.kind() != "|",
            );
        }
        "reference_pattern" => {
            // `&x` or `&mut x`
            bind_first_matching_child(
                tree,
                scope_id,
                node,
                declarator_end,
                initializer_start,
                content,
                |child| !matches!(child.kind(), "&" | "mutable_specifier"),
            );
        }
        _ => {}
    }
}

/// Bind function parameters.
fn bind_function_parameters(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    params: Node,
    content: &[u8],
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter"
            && let Some(pattern) = child.child_by_field_name("pattern")
        {
            bind_pattern(tree, scope_id, pattern, child.end_byte(), None, content);
        }
    }
}

/// Bind closure parameters (`|x, y|`).
fn bind_closure_parameters(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    params: Node,
    content: &[u8],
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "parameter" => {
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    bind_pattern(tree, scope_id, pattern, child.end_byte(), None, content);
                }
            }
            "identifier" => {
                // Bare identifier parameter in closures: `|x| x + 1`
                bind_pattern(tree, scope_id, child, child.end_byte(), None, content);
            }
            _ => {}
        }
    }
}

/// Bind let condition patterns (if let / while let).
fn bind_let_condition(tree: &mut RustScopeTree, scope_id: ScopeId, node: Node, content: &[u8]) {
    // Find the `let_condition` child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "let_condition"
            && let Some(pattern) = child.child_by_field_name("pattern")
        {
            bind_pattern(tree, scope_id, pattern, child.end_byte(), None, content);
        }
    }
}

// ============================================================================
// Identifier resolution + graph integration
// ============================================================================

/// Handle an identifier node encountered during edge building.
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut RustScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier = node.utf8_text(content).unwrap_or("");
    if identifier.is_empty() || identifier == "_" {
        return;
    }

    if is_declaration_context(node) {
        return;
    }

    if is_type_or_path_context(node) {
        return;
    }

    if is_call_context(node) {
        return;
    }

    if is_field_or_method_access(node) {
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
                let span = binding.decl_span;
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

/// Check if this identifier is in a declaration position (not a usage).
fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Let binding pattern
        "let_declaration" => parent
            .child_by_field_name("pattern")
            .is_some_and(|n| n.id() == node.id()),
        // Function name
        "function_item" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Parameter pattern
        "parameter" => parent
            .child_by_field_name("pattern")
            .is_some_and(|n| n.id() == node.id()),
        // Part of a pattern: `mut x`, `ref x`, tuple, struct, etc.
        "mut_pattern" | "ref_pattern" | "reference_pattern" | "field_pattern"
        | "use_declaration" | "use_as_clause" | "scoped_use_list" | "use_list" | "use_wildcard"
        | "label" | "closure_parameters" => true,
        // Struct/enum name in definition
        "struct_item" | "enum_item" | "type_item" | "trait_item" | "impl_item" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Enum variant name
        "enum_variant" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Macro definition name
        "macro_definition" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // For loop pattern
        "for_expression" => parent
            .child_by_field_name("pattern")
            .is_some_and(|n| n.id() == node.id()),
        // Match arm pattern
        "match_arm" => parent
            .child_by_field_name("pattern")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

/// Check if this identifier is in a type or path context (not a variable usage).
fn is_type_or_path_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Type annotations
        "type_identifier"
        | "primitive_type"
        | "generic_type"
        | "scoped_type_identifier"
        | "type_arguments"
        | "bounded_type"
        | "reference_type"
        | "pointer_type"
        | "array_type"
        | "tuple_type"
        | "function_type"
        | "impl_type"
        | "dyn_type"
        | "abstract_type"
        | "never_type"
        | "slice_type"
        | "qualified_type"
        | "scoped_identifier"
        | "attribute"
        | "attribute_item"
        | "meta_item"
        | "lifetime" => true,
        // Field access in struct literals (not a variable usage)
        "field_initializer" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Macro invocations (the macro name, not arguments)
        "macro_invocation" => parent
            .child_by_field_name("macro")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

/// Check if this identifier is being called as a function.
fn is_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        "call_expression" => parent
            .child_by_field_name("function")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

/// Check if this identifier is a field access or method call target.
fn is_field_or_method_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        "field_expression" => parent
            .child_by_field_name("field")
            .is_some_and(|n| n.id() == node.id()),
        _ => false,
    }
}

/// Check if this identifier is inside a function/closure/method body.
fn is_inside_function(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_item" | "closure_expression" => return true,
            "source_file" => return false,
            _ => {}
        }
        current = parent.parent();
    }
    false
}

// ============================================================================
// Utility helpers
// ============================================================================

/// Check if a node is inside an impl block.
fn is_inside_impl(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            return true;
        }
        if parent.kind() == "source_file" {
            return false;
        }
        current = parent.parent();
    }
    false
}

/// Check if an if/while expression has a `let` condition.
fn has_let_condition(node: Node) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "let_condition")
}
