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
    let content_len = content.len();
    let mut tree = RustScopeTree::new(content_len);

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
    tree: &mut RustScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

    match node.kind() {
        "function_item" => {
            // Determine if this is a method (inside an impl block)
            let kind = if is_inside_impl(node) {
                ScopeKind::Method
            } else {
                ScopeKind::Function
            };
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) =
                    tree.add_scope(kind, node.start_byte(), node.end_byte(), current_scope)
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "closure_expression" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Closure,
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
        "for_expression" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::ForLoop,
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
        "while_expression" => {
            let kind = if has_let_condition(node) {
                ScopeKind::WhileLet
            } else {
                ScopeKind::WhileLoop
            };
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) =
                    tree.add_scope(kind, node.start_byte(), node.end_byte(), current_scope)
            {
                recurse_children(tree, body, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "loop_expression" => {
            if let Some(body) = node.child_by_field_name("body")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::Loop,
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
        "if_expression" => {
            let kind = if has_let_condition(node) {
                ScopeKind::IfLet
            } else {
                ScopeKind::IfBranch
            };
            // Then branch (consequence)
            if let Some(consequence) = node.child_by_field_name("consequence")
                && let Some(scope_id) = tree.add_scope(
                    kind,
                    consequence.start_byte(),
                    consequence.end_byte(),
                    current_scope,
                )
            {
                recurse_children(tree, consequence, content, Some(scope_id), guard)?;
            }
            // Else branch (alternative)
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
        "match_expression" => {
            // Each match arm is its own scope; we don't create a scope for
            // the entire match expression itself since the arms handle it
            recurse_children(tree, node, content, current_scope, guard)?;
            guard.exit();
            return Ok(());
        }
        "match_arm" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::MatchArm,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            ) {
                recurse_children(tree, node, content, Some(scope_id), guard)?;
            }
            guard.exit();
            return Ok(());
        }
        "unsafe_block" => {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::UnsafeBlock,
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
            // Avoid double-scoping: don't create a new scope for blocks that
            // are direct children of already-scoped constructs.
            let parent_kind = node.parent().map(|p| p.kind());
            let already_scoped = matches!(
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
        .map_err(|e| local_scopes::recursion_error_to_graph_error(e, node))?;

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
        "for_expression" => {
            if let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                && let Some(pattern) = node.child_by_field_name("pattern")
            {
                bind_pattern(tree, scope_id, pattern, pattern.end_byte(), None, content);
            }
        }
        "match_arm" => {
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

/// Bind a pattern node (identifier, mut_pattern, tuple_pattern, etc.).
fn bind_pattern(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    match node.kind() {
        "identifier" => {
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
        "mut_pattern" | "ref_pattern" => {
            // `mut x` or `ref x` — descend to find the identifier
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "identifier" | "mut_pattern" | "ref_pattern" => {
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
        "tuple_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !matches!(child.kind(), "(" | ")" | ",") {
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
        "tuple_struct_pattern" => {
            // `Some(x)` or `Ok(val)` — the first identifier/scoped_identifier
            // is the type name; bind the remaining patterns.
            // Use `child_by_field_name("type")` to identify the type name node,
            // then bind everything else.
            let type_node_id = node.child_by_field_name("type").map(|n| n.id());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // Skip the type name, parentheses, and commas
                if matches!(child.kind(), "(" | ")" | ",") {
                    continue;
                }
                if type_node_id.is_some_and(|id| child.id() == id) {
                    continue;
                }
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
        "struct_pattern" => {
            // `Point { x, y }` or `Point { x: a, y: b }`
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "field_pattern" {
                    bind_field_pattern(
                        tree,
                        scope_id,
                        child,
                        declarator_end,
                        initializer_start,
                        content,
                    );
                } else if child.kind() == "remaining_field_pattern" {
                    // `..` — rest pattern, skip
                }
            }
        }
        "slice_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !matches!(child.kind(), "[" | "]" | ",") {
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
        "or_pattern" => {
            // `A | B` — bind identifiers from the first arm only
            // (both arms should bind the same names)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "|" {
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
        "reference_pattern" => {
            // `&x` or `&mut x`
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !matches!(child.kind(), "&" | "mutable_specifier") {
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
        "rest_pattern" => {
            // `..` in patterns — no binding
        }
        "_" => {
            // Wildcard — no binding
        }
        _ => {}
    }
}

/// Bind a field pattern from a struct pattern.
///
/// Handles both `{ x }` (shorthand) and `{ x: name }` (renamed).
fn bind_field_pattern(
    tree: &mut RustScopeTree,
    scope_id: ScopeId,
    field_node: Node,
    declarator_end: usize,
    initializer_start: Option<usize>,
    content: &[u8],
) {
    // field_pattern can be:
    // 1. Shorthand: just an identifier `x`
    // 2. Named: `field_name: pattern`
    if let Some(pattern) = field_node.child_by_field_name("pattern") {
        // Named field: `x: name` — bind the pattern (not the field name)
        bind_pattern(
            tree,
            scope_id,
            pattern,
            declarator_end,
            initializer_start,
            content,
        );
    } else {
        // Shorthand: `x` — the field_pattern's name is both the field and variable name
        if let Some(name_node) = field_node.child_by_field_name("name") {
            bind_pattern(
                tree,
                scope_id,
                name_node,
                declarator_end,
                initializer_start,
                content,
            );
        }
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
        match child.kind() {
            "parameter" => {
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    bind_pattern(tree, scope_id, pattern, child.end_byte(), None, content);
                }
            }
            "self_parameter" => {
                // `self`, `&self`, `&mut self` — skip, not a local variable
            }
            _ => {}
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
        "mut_pattern" | "ref_pattern" | "reference_pattern" => true,
        // Field patterns (struct destructuring)
        "field_pattern" => true,
        // Struct/enum name in definition
        "struct_item" | "enum_item" | "type_item" | "trait_item" | "impl_item" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Use declarations (imports)
        "use_declaration" | "use_as_clause" | "scoped_use_list" | "use_list" | "use_wildcard" => {
            true
        }
        // Label definitions
        "label" => true,
        // Enum variant name
        "enum_variant" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Macro definition name
        "macro_definition" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Closure parameters
        "closure_parameters" => true,
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
        | "qualified_type" => true,
        // Path segments (module paths like `std::mem::drop`)
        "scoped_identifier" => true,
        // Field access in struct literals (not a variable usage)
        "field_initializer" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Attribute names
        "attribute" | "attribute_item" | "meta_item" => true,
        // Lifetime names
        "lifetime" => true,
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
