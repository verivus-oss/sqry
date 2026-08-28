//! Local scope tracking and reference resolution for Python.
//!
//! Python's scoping model (LEGB: Local, Enclosing, Global, Built-in) differs
//! significantly from block-scoped languages:
//!
//! - **No block scope**: `if`, `for`, `while`, `try`, `with` do NOT create scopes.
//!   Variables bind to the enclosing function/module scope.
//! - **Assignment = declaration**: `x = 5` both declares and assigns. There is no
//!   `let`/`var` keyword.
//! - **`global`/`nonlocal`**: Pre-pass collects these to exclude names from local binding.
//! - **Comprehension scope**: Python 3 comprehension/generator variables are scoped
//!   to the comprehension expression.
//! - **Class scope**: NOT accessible in nested functions (only via `self`).
//! - **Walrus operator**: `:=` creates binding in enclosing function scope, NOT
//!   comprehension scope.

use std::collections::{HashMap, HashSet};

use sqry_core::graph::local_scopes::{self, ResolutionOutcome, ScopeId, ScopeKindTrait, ScopeTree};
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::{GraphResult, Span};
use tree_sitter::Node;

// ============================================================================
// Python-specific ScopeKind
// ============================================================================

/// Python scope kinds.
///
/// Only functions, lambdas, classes, comprehensions, and the module create
/// variable scopes. Block statements (`if`/`for`/`while`/`try`/`with`) do NOT
/// create scopes — variables bind to the nearest enclosing function or module.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    /// Module scope (implicit top-level).
    Module,
    /// Function body (`def foo(): ...`).
    Function,
    /// Lambda expression (`lambda x: x + 1`).
    Lambda,
    /// Class body (`class Foo: ...`).
    ///
    /// Class scopes are NOT accessible from nested functions — they act as
    /// a boundary. Variables in a class body are only accessible within the
    /// class body itself, not in nested `def` blocks.
    Class,
    /// Comprehension/generator scope.
    ///
    /// In Python 3, the iteration variable of a comprehension is scoped to
    /// the comprehension expression. The walrus operator (`:=`) does NOT
    /// bind in the comprehension scope but in the enclosing function scope.
    Comprehension,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        *self == ScopeKind::Class
    }

    fn is_overlap_boundary(&self) -> bool {
        *self == ScopeKind::Class
    }

    fn allows_nested_shadowing(&self) -> bool {
        true // Python allows reassignment / shadowing freely
    }
}

/// Type alias for the Python-specialized scope tree.
pub(crate) type PythonScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build pipeline
// ============================================================================

/// Build a scope tree for a Python source file.
pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<PythonScopeTree> {
    let content_len = content.len();
    let mut tree = PythonScopeTree::new(content_len);

    let mut guard = local_scopes::load_recursion_guard();

    // Phase 0: Create module scope for the entire file
    let module_scope = tree.add_scope(ScopeKind::Module, 0, content_len, None);

    // Phase 1: Build scopes (functions, classes, comprehensions)
    build_scopes_recursive(&mut tree, root, content, module_scope, &mut guard)?;
    tree.rebuild_index();

    // Phase 1.5: Collect per-function `global`/`nonlocal` exclusions. Must run
    // after `rebuild_index()` (it relies on the rebuilt interval index via
    // `innermost_scope_at`) and before Phase 2 so declaration binding can skip
    // names that refer to outer scopes.
    let exclusions = collect_scope_exclusions(&tree, root, content);

    // Phase 2: Bind declarations (assignments, for-loop variables, parameters, etc.)
    bind_declarations_recursive(&mut tree, root, content, &exclusions, &mut guard)?;
    tree.rebuild_index();

    Ok(tree)
}

// ============================================================================
// Phase 1: Build scopes
// ============================================================================

#[allow(clippy::only_used_in_recursion)]
fn build_scopes_recursive(
    tree: &mut PythonScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    let new_scope = match node.kind() {
        "function_definition" => {
            // `def foo():` or `async def foo():`
            tree.add_scope(
                ScopeKind::Function,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            )
        }
        "lambda" => {
            // `lambda x: x + 1`
            tree.add_scope(
                ScopeKind::Lambda,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            )
        }
        "class_definition" => {
            // `class Foo:`
            tree.add_scope(
                ScopeKind::Class,
                node.start_byte(),
                node.end_byte(),
                current_scope,
            )
        }
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => tree.add_scope(
            ScopeKind::Comprehension,
            node.start_byte(),
            node.end_byte(),
            current_scope,
        ),
        _ => None,
    };

    let scope_for_children = new_scope.or(current_scope);

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        build_scopes_recursive(tree, child, content, scope_for_children, guard)?;
    }

    guard.exit();
    Ok(())
}

// ============================================================================
// Phase 2: Bind declarations
// ============================================================================

fn bind_declarations_recursive(
    tree: &mut PythonScopeTree,
    node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard
        .enter()
        .map_err(|e| local_scopes::recursion_error_to_graph_error(&e, node))?;

    match node.kind() {
        "function_definition" => {
            // Bind parameters to the function scope
            bind_parameters(tree, node, content);
        }
        "lambda" => {
            // Bind lambda parameters
            bind_lambda_parameters(tree, node, content);
        }
        "for_statement" => {
            // `for x in iterable:` — bind x to enclosing function/module scope
            bind_for_variable(tree, node, content, exclusions);
        }
        "expression_statement" => {
            // Check for assignment inside expression_statement
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "assignment" {
                    bind_assignment(tree, child, content, exclusions);
                }
            }
        }
        "assignment" => {
            // Direct assignment (may appear outside expression_statement in some contexts)
            bind_assignment(tree, node, content, exclusions);
        }
        "named_expression" => {
            // Walrus operator: `(x := expr)` — binds to enclosing function scope
            bind_walrus(tree, node, content, exclusions);
        }
        "except_clause" => {
            // `except Exception as e:` — bind e to enclosing function scope
            bind_except_variable(tree, node, content, exclusions);
        }
        "with_statement" => {
            // `with expr as x:` — bind x to enclosing function scope
            bind_with_variable(tree, node, content, exclusions);
        }
        "for_in_clause" => {
            // Comprehension variable: `for x in iterable`
            // Bind to the comprehension scope
            bind_comprehension_variable(tree, node, content, exclusions);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        bind_declarations_recursive(tree, child, content, exclusions, guard)?;
    }

    guard.exit();
    Ok(())
}

// ============================================================================
// Binding helpers
// ============================================================================

/// Bind function parameters to the function's scope.
fn bind_parameters(tree: &mut PythonScopeTree, func_node: Node, content: &[u8]) {
    let Some(params_node) = func_node.child_by_field_name("parameters") else {
        return;
    };

    let Some(scope_id) = tree.innermost_scope_at(func_node.start_byte()) else {
        return;
    };

    for param in params_node.children(&mut params_node.walk()) {
        match param.kind() {
            "identifier" => {
                // Simple parameter: `def foo(x):`
                bind_identifier_param(tree, scope_id, param, content);
            }
            "typed_parameter" => {
                // Typed parameter: `def foo(x: int):`
                // The name is either in the "name" field or as a direct identifier child
                if let Some(name_node) = param.child_by_field_name("name") {
                    bind_identifier_param(tree, scope_id, name_node, content);
                } else {
                    // Check for identifier child (for *args, **kwargs patterns)
                    let mut cursor = param.walk();
                    for child in param.children(&mut cursor) {
                        if child.kind() == "identifier" {
                            bind_identifier_param(tree, scope_id, child, content);
                            break;
                        }
                        if child.kind() == "list_splat_pattern"
                            || child.kind() == "dictionary_splat_pattern"
                        {
                            if let Some(id) = local_scopes::first_child_of_kind(child, "identifier")
                            {
                                bind_identifier_param(tree, scope_id, id, content);
                            }
                            break;
                        }
                    }
                }
            }
            "default_parameter" => {
                // Default parameter: `def foo(x=5):`
                if let Some(name_node) = param.child_by_field_name("name") {
                    bind_identifier_param(tree, scope_id, name_node, content);
                }
            }
            "typed_default_parameter" => {
                // Typed default: `def foo(x: int = 5):`
                if let Some(name_node) = param.child_by_field_name("name") {
                    bind_identifier_param(tree, scope_id, name_node, content);
                }
            }
            "list_splat_pattern" => {
                // *args pattern
                if let Some(id) = local_scopes::first_child_of_kind(param, "identifier") {
                    bind_identifier_param(tree, scope_id, id, content);
                }
            }
            "dictionary_splat_pattern" => {
                // **kwargs pattern
                if let Some(id) = local_scopes::first_child_of_kind(param, "identifier") {
                    bind_identifier_param(tree, scope_id, id, content);
                }
            }
            _ => {}
        }
    }
}

/// Bind lambda parameters to the lambda's scope.
fn bind_lambda_parameters(tree: &mut PythonScopeTree, lambda_node: Node, content: &[u8]) {
    let Some(params_node) = lambda_node.child_by_field_name("parameters") else {
        return;
    };

    let Some(scope_id) = tree.innermost_scope_at(lambda_node.start_byte()) else {
        return;
    };

    // Lambda parameters use `lambda_parameters` node type
    for param in params_node.children(&mut params_node.walk()) {
        match param.kind() {
            "identifier" => {
                bind_identifier_param(tree, scope_id, param, content);
            }
            "default_parameter" => {
                if let Some(name_node) = param.child_by_field_name("name") {
                    bind_identifier_param(tree, scope_id, name_node, content);
                }
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                if let Some(id) = local_scopes::first_child_of_kind(param, "identifier") {
                    bind_identifier_param(tree, scope_id, id, content);
                }
            }
            _ => {}
        }
    }
}

/// Bind a single identifier as a parameter.
fn bind_identifier_param(
    tree: &mut PythonScopeTree,
    scope_id: ScopeId,
    name_node: Node,
    content: &[u8],
) {
    let Ok(name) = name_node.utf8_text(content) else {
        return;
    };
    let name = name.trim();
    if name.is_empty() || name == "self" || name == "cls" {
        return;
    }
    tree.add_binding(
        scope_id,
        name,
        name_node.start_byte(),
        name_node.end_byte(),
        name_node.end_byte(), // parameters have no initializer
        None,
    );
}

/// Bind assignment targets.
///
/// In Python, `x = expr` both declares and assigns. The left-hand side
/// can be a simple identifier, a tuple/list pattern, or an attribute/subscript
/// (which we skip — not local variable declarations).
fn bind_assignment(
    tree: &mut PythonScopeTree,
    assignment_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    let Some(left) = assignment_node.child_by_field_name("left") else {
        return;
    };

    // Find the enclosing function scope (NOT class or comprehension scope).
    // In Python, assignments bind to the nearest enclosing function/module scope.
    let Some(scope_id) = find_binding_scope(tree, assignment_node.start_byte()) else {
        return;
    };

    // Determine initializer start byte for self-reference prevention
    let init_start = assignment_node
        .child_by_field_name("right")
        .map(|r| r.start_byte());

    bind_pattern(
        tree,
        scope_id,
        left,
        content,
        init_start,
        assignment_node,
        exclusions,
    );
}

/// Bind for-loop variables.
///
/// `for x in iterable:` — binds x to the enclosing function/module scope
/// (NOT a new scope, since Python has no block scoping).
fn bind_for_variable(
    tree: &mut PythonScopeTree,
    for_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    let Some(left) = for_node.child_by_field_name("left") else {
        return;
    };

    // For loop variables bind to enclosing function scope
    let Some(scope_id) = find_binding_scope(tree, for_node.start_byte()) else {
        return;
    };

    bind_pattern(tree, scope_id, left, content, None, for_node, exclusions);
}

/// Bind walrus operator target.
///
/// `(x := expr)` binds to the enclosing function scope, even when inside
/// a comprehension (unlike regular assignment which would bind to the
/// comprehension scope in list/dict/set comprehensions).
fn bind_walrus(
    tree: &mut PythonScopeTree,
    walrus_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    let Some(name_node) = walrus_node.child_by_field_name("name") else {
        return;
    };

    // Walrus binds to enclosing function/module scope, skipping comprehensions
    let Some(scope_id) = find_function_scope(tree, walrus_node.start_byte()) else {
        return;
    };

    let Ok(name) = name_node.utf8_text(content) else {
        return;
    };
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    // Skip names declared `global`/`nonlocal` in this scope.
    if is_excluded(exclusions, scope_id, name) {
        return;
    }

    let init_start = walrus_node
        .child_by_field_name("value")
        .map(|v| v.start_byte());

    tree.add_binding(
        scope_id,
        name,
        name_node.start_byte(),
        name_node.end_byte(),
        walrus_node.end_byte(),
        init_start,
    );
}

/// Bind except clause variable (`except Exception as e:`)
///
/// tree-sitter-python AST structure:
/// ```text
/// except_clause
///   except
///   as_pattern
///     identifier (exception type, e.g., "ValueError")
///     as
///     as_pattern_target
///       identifier (binding name, e.g., "e")
///   :
///   block
/// ```
fn bind_except_variable(
    tree: &mut PythonScopeTree,
    except_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    let mut cursor = except_node.walk();
    for child in except_node.children(&mut cursor) {
        if child.kind() == "as_pattern" {
            // Find the as_pattern_target child
            let mut inner_cursor = child.walk();
            for inner_child in child.children(&mut inner_cursor) {
                if inner_child.kind() == "as_pattern_target" {
                    if let Some(id) = local_scopes::first_child_of_kind(inner_child, "identifier") {
                        let Some(scope_id) = find_binding_scope(tree, except_node.start_byte())
                        else {
                            return;
                        };
                        let Ok(name) = id.utf8_text(content) else {
                            return;
                        };
                        let name = name.trim();
                        if !name.is_empty() && !is_excluded(exclusions, scope_id, name) {
                            tree.add_binding(
                                scope_id,
                                name,
                                id.start_byte(),
                                id.end_byte(),
                                id.end_byte(),
                                None,
                            );
                        }
                    }
                    return;
                }
            }
        }
    }
}

/// Bind with-statement variable (`with expr as x:`)
///
/// tree-sitter-python AST structure:
/// ```text
/// with_statement
///   with
///   with_clause
///     with_item
///       as_pattern
///         call (the context manager expression)
///         as
///         as_pattern_target
///           identifier (binding name, e.g., "f")
///   :
///   block
/// ```
fn bind_with_variable(
    tree: &mut PythonScopeTree,
    with_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    let mut cursor = with_node.walk();
    for child in with_node.children(&mut cursor) {
        if child.kind() == "with_clause" {
            let mut clause_cursor = child.walk();
            for item in child.children(&mut clause_cursor) {
                if item.kind() == "with_item" {
                    bind_with_item(tree, item, content, with_node.start_byte(), exclusions);
                }
            }
        }
    }
}

/// Bind a single `with_item`'s `as` target via `as_pattern` → `as_pattern_target`.
fn bind_with_item(
    tree: &mut PythonScopeTree,
    item: Node,
    content: &[u8],
    context_byte: usize,
    exclusions: &GlobalNonlocalExclusions,
) {
    let mut cursor = item.walk();
    for child in item.children(&mut cursor) {
        if child.kind() == "as_pattern" {
            let mut inner_cursor = child.walk();
            for inner_child in child.children(&mut inner_cursor) {
                if inner_child.kind() == "as_pattern_target" {
                    let Some(scope_id) = find_binding_scope(tree, context_byte) else {
                        return;
                    };
                    // as_pattern_target contains an identifier or pattern
                    if let Some(id) = local_scopes::first_child_of_kind(inner_child, "identifier") {
                        let Ok(name) = id.utf8_text(content) else {
                            return;
                        };
                        let name = name.trim();
                        if !name.is_empty() && !is_excluded(exclusions, scope_id, name) {
                            tree.add_binding(
                                scope_id,
                                name,
                                id.start_byte(),
                                id.end_byte(),
                                id.end_byte(),
                                None,
                            );
                        }
                    } else {
                        // Tuple pattern in as target
                        bind_pattern(
                            tree,
                            scope_id,
                            inner_child,
                            content,
                            None,
                            inner_child,
                            exclusions,
                        );
                    }
                    return;
                }
            }
        }
    }
}

/// Bind comprehension iteration variable.
///
/// `for x in iterable` inside a comprehension — binds to the comprehension scope.
fn bind_comprehension_variable(
    tree: &mut PythonScopeTree,
    for_in_node: Node,
    content: &[u8],
    exclusions: &GlobalNonlocalExclusions,
) {
    // for_in_clause has a "left" field with the iteration variable(s)
    let Some(left) = for_in_node.child_by_field_name("left") else {
        return;
    };

    // Comprehension variables bind to the comprehension scope itself
    let Some(scope_id) = tree.innermost_scope_at(for_in_node.start_byte()) else {
        return;
    };

    bind_pattern(tree, scope_id, left, content, None, for_in_node, exclusions);
}

// ============================================================================
// Pattern binding — handles destructuring
// ============================================================================

/// Bind identifiers from a pattern (LHS of assignment/for).
///
/// Handles:
/// - `identifier` — simple variable
/// - `pattern_list` / `tuple_pattern` — tuple unpacking: `a, b = ...`
/// - `list_pattern` — list unpacking: `[a, b] = ...`
/// - `*rest` — star expressions in unpacking
fn bind_pattern(
    tree: &mut PythonScopeTree,
    scope_id: ScopeId,
    pattern: Node,
    content: &[u8],
    init_start: Option<usize>,
    declarator_node: Node,
    exclusions: &GlobalNonlocalExclusions,
) {
    match pattern.kind() {
        "identifier" => {
            let Ok(name) = pattern.utf8_text(content) else {
                return;
            };
            let name = name.trim();
            // Skip _ wildcard and special names
            if name.is_empty() || name == "_" {
                return;
            }
            // Skip names declared `global`/`nonlocal` in this scope: they refer
            // to an outer binding, not a new local.
            if is_excluded(exclusions, scope_id, name) {
                return;
            }
            tree.add_binding(
                scope_id,
                name,
                pattern.start_byte(),
                pattern.end_byte(),
                declarator_node.end_byte(),
                init_start,
            );
        }
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            // Tuple/list unpacking: `a, b = ...` or `[a, b] = ...`
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                bind_pattern(
                    tree,
                    scope_id,
                    child,
                    content,
                    init_start,
                    declarator_node,
                    exclusions,
                );
            }
        }
        "list_splat_pattern" => {
            // `*rest` in unpacking
            if let Some(id) = local_scopes::first_child_of_kind(pattern, "identifier") {
                let Ok(name) = id.utf8_text(content) else {
                    return;
                };
                let name = name.trim();
                if !name.is_empty() && name != "_" && !is_excluded(exclusions, scope_id, name) {
                    tree.add_binding(
                        scope_id,
                        name,
                        id.start_byte(),
                        id.end_byte(),
                        declarator_node.end_byte(),
                        init_start,
                    );
                }
            }
        }
        _ => {
            // attribute, subscript, etc. — not local variable bindings, skip
        }
    }
}

// ============================================================================
// Scope finding helpers
// ============================================================================

/// Find the scope where a variable should be bound.
///
/// In Python, variables bind to the nearest enclosing function, lambda, or module scope.
/// Class and comprehension scopes are skipped for regular assignments (but NOT for
/// comprehension iteration variables, which bind to the comprehension scope).
fn find_binding_scope(tree: &PythonScopeTree, byte: usize) -> Option<ScopeId> {
    let innermost = tree.innermost_scope_at(byte)?;
    let chain = tree.scope_chain(innermost);

    for scope_id in &chain {
        let kind = tree.scopes[*scope_id].kind;
        match kind {
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Module => {
                return Some(*scope_id);
            }
            ScopeKind::Class | ScopeKind::Comprehension => {
                // Skip — assignments don't bind here
            }
        }
    }
    None
}

/// Find the nearest enclosing function/module scope, skipping both
/// class AND comprehension scopes.
///
/// Used for walrus operator (`:=`) which explicitly skips comprehension scopes.
fn find_function_scope(tree: &PythonScopeTree, byte: usize) -> Option<ScopeId> {
    let innermost = tree.innermost_scope_at(byte)?;
    let chain = tree.scope_chain(innermost);

    for scope_id in &chain {
        let kind = tree.scopes[*scope_id].kind;
        match kind {
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Module => {
                return Some(*scope_id);
            }
            ScopeKind::Class | ScopeKind::Comprehension => {}
        }
    }
    None
}

// ============================================================================
// Identifier resolution — handle_identifier_for_reference
// ============================================================================

/// Handle an identifier node for potential local variable reference.
///
/// Called from the graph builder's walker for each `"identifier"` node.
/// Creates a `Variable` node and `References` edge if the identifier resolves
/// to a local variable or parameter.
pub(crate) fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    scope_tree: &mut PythonScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let Ok(name) = node.utf8_text(content) else {
        return;
    };
    let name = name.trim();

    // Skip empty, wildcard, self/cls, and common built-in names
    if name.is_empty() || name == "_" || name == "self" || name == "cls" {
        return;
    }

    // Skip if in a non-reference context
    if is_declaration_context(node) {
        return;
    }
    if is_decorator_or_import_context(node) {
        return;
    }
    if is_call_context(node) {
        return;
    }
    if is_attribute_access(node) {
        return;
    }
    if is_type_annotation_context(node) {
        return;
    }
    if !is_inside_scope(node) {
        return;
    }

    // Try to resolve the identifier
    let usage_byte = node.start_byte();
    match scope_tree.resolve_identifier(usage_byte, name) {
        ResolutionOutcome::Local(binding) => {
            let span = Span::from_node(&node);
            let qualified_name = format!("{}@{}", name, binding.decl_start_byte);
            let var_id = helper.add_variable(&qualified_name, Some(span));

            if let Some(decl_id) = binding.node_id {
                helper.add_reference_edge(var_id, decl_id);
            } else {
                // Create declaration node if not yet attached
                let decl_span = Span::from_bytes(binding.decl_start_byte, binding.decl_end_byte);
                let decl_id = helper.add_variable(&qualified_name, Some(decl_span));
                scope_tree.attach_node_id(name, binding.decl_start_byte, decl_id);
                helper.add_reference_edge(var_id, decl_id);
            }
        }
        ResolutionOutcome::Member { .. }
        | ResolutionOutcome::Ambiguous
        | ResolutionOutcome::NoMatch => {}
    }
}

// ============================================================================
// Context filters — prevent false positive References edges
// ============================================================================

/// Check if the identifier is in a declaration/definition context.
///
/// Returns `true` for identifiers that ARE the declaration, not a usage:
/// - Function/class name being defined
/// - Assignment left-hand side target
/// - For-loop variable
/// - Parameter name in definition
#[allow(clippy::match_same_arms)] // Arms separated for documentation clarity; each pattern is semantically distinct
fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // Function or class name being defined
        "function_definition" | "class_definition" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Assignment LHS: `x = 5` — the `x` is a declaration
        "assignment" | "augmented_assignment" => parent
            .child_by_field_name("left")
            .is_some_and(|left| contains_node(left, node)),
        // For-loop variable
        "for_statement" => parent
            .child_by_field_name("left")
            .is_some_and(|left| contains_node(left, node)),
        // Walrus operator name
        "named_expression" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        // Parameter names
        "typed_parameter" | "typed_default_parameter" | "default_parameter" => parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id()),
        #[allow(clippy::match_same_arms)]
        // Arms separated by AST node type for documentation clarity
        // Plain parameter identifier (not wrapped in typed_parameter)
        "parameters" | "lambda_parameters" => true,
        // Pattern list elements (e.g., `a, b = pair` — the `a` and `b`)
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            // Check if the grandparent is an assignment/for LHS
            if let Some(grandparent) = parent.parent() {
                match grandparent.kind() {
                    "assignment" | "augmented_assignment" => grandparent
                        .child_by_field_name("left")
                        .is_some_and(|left| contains_node(left, node)),
                    "for_statement" => grandparent
                        .child_by_field_name("left")
                        .is_some_and(|left| contains_node(left, node)),
                    _ => false,
                }
            } else {
                false
            }
        }
        // Splat patterns in assignments
        "list_splat_pattern" | "dictionary_splat_pattern" => true,
        // Comprehension iteration variable
        "for_in_clause" => parent
            .child_by_field_name("left")
            .is_some_and(|left| contains_node(left, node)),
        // Global/nonlocal declarations
        "global_statement" | "nonlocal_statement" => true,
        // as_pattern_target (except clause `as` binding target)
        "as_pattern_target" => true,
        _ => false,
    }
}

/// Check if the identifier is in a decorator or import context.
fn is_decorator_or_import_context(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "decorator" | "import_statement" | "import_from_statement" | "aliased_import" => {
                return true;
            }
            "function_definition" | "class_definition" | "module" => {
                return false;
            }
            _ => current = parent.parent(),
        }
    }
    false
}

/// Check if the identifier is a function/method call target.
///
/// `foo()` — the `foo` is a call target, not a variable reference.
fn is_call_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "call" {
        return parent
            .child_by_field_name("function")
            .is_some_and(|f| f.id() == node.id());
    }
    false
}

/// Check if the identifier is the attribute part of a member access.
///
/// `obj.attr` — the `attr` is member access, not a local variable reference.
/// But `obj` IS a reference (it could be a local variable).
fn is_attribute_access(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "attribute" {
        // `attribute` node has `object` and `attribute` fields.
        // The `attribute` field (right side) is NOT a local variable reference.
        // The `object` field (left side) IS a potential reference.
        return parent
            .child_by_field_name("attribute")
            .is_some_and(|a| a.id() == node.id());
    }
    false
}

/// Check if the identifier is in a type annotation context.
///
/// Type annotations reference types, not local variables:
/// - `x: int` → `int` is a type
/// - `def foo(x: str):` → `str` is a type
/// - `-> int` → `int` is a type
fn is_type_annotation_context(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "type" => return true,
            // Type annotation field in various contexts
            "assignment" | "typed_parameter" | "typed_default_parameter" => {
                if parent
                    .child_by_field_name("type")
                    .is_some_and(|t| contains_node(t, node))
                {
                    return true;
                }
                return false;
            }
            // Return type annotation
            "function_definition" => {
                if parent
                    .child_by_field_name("return_type")
                    .is_some_and(|r| contains_node(r, node))
                {
                    return true;
                }
                return false;
            }
            _ => current = parent.parent(),
        }
    }
    false
}

/// Check if the identifier is inside a function, class, or module scope.
///
/// We only track identifiers inside scope-creating constructs.
fn is_inside_scope(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_definition" | "class_definition" | "lambda" | "module" => return true,
            _ => current = parent.parent(),
        }
    }
    false
}

// ============================================================================
// Utility helpers
// ============================================================================

/// Check if a node contains another node (by ID).
fn contains_node(haystack: Node, needle: Node) -> bool {
    if haystack.id() == needle.id() {
        return true;
    }
    let mut cursor = haystack.walk();
    for child in haystack.children(&mut cursor) {
        if contains_node(child, needle) {
            return true;
        }
    }
    false
}

/// Per-function-scope set of names declared `global` or `nonlocal`.
///
/// Keyed by [`ScopeId`] (not by bare name) so that two functions declaring
/// the same name `global` do not cross-contaminate, and so module-level
/// assignments (whose scope never receives an entry) stay untouched.
pub(crate) type GlobalNonlocalExclusions = HashMap<ScopeId, HashSet<String>>;

/// Return `true` when `name` is declared `global` or `nonlocal` in `scope_id`.
///
/// Such names refer to a binding in an outer scope, so a local assignment,
/// walrus, `with ... as`, or `except ... as` inside that scope must NOT create
/// a new local binding for them. Scopes without an entry (module scope, and any
/// function that never used `global`/`nonlocal`) are never excluded.
fn is_excluded(exclusions: &GlobalNonlocalExclusions, scope_id: ScopeId, name: &str) -> bool {
    exclusions
        .get(&scope_id)
        .is_some_and(|names| names.contains(name))
}

/// Build the per-function exclusion map by walking every `function_definition`
/// node in the tree.
///
/// For each function, resolve its own scope via
/// `tree.innermost_scope_at(func_node.start_byte())` (the same idiom
/// `bind_parameters` uses, which returns the function scope, not its parent),
/// then collect the `global`/`nonlocal` names from the function **body block**
/// (`child_by_field_name("body")`), not the `function_definition` node itself.
/// Passing the definition node would hit the `function_definition` boundary arm
/// in [`collect_global_nonlocal_recursive`] on the first match and return an
/// empty set, silently turning the whole fix into a no-op.
///
/// Only `function_definition` scopes are collected: lambdas cannot hold
/// statements, and a module-level `global` is a Python no-op. The module scope
/// therefore never receives an entry. Class-body `global`/`nonlocal` is an
/// explicit out-of-scope follow-up (see the module design doc).
///
/// Must run after Phase 1 `rebuild_index()`, because `innermost_scope_at`
/// requires the rebuilt interval index.
pub(crate) fn collect_scope_exclusions(
    tree: &PythonScopeTree,
    root: Node,
    content: &[u8],
) -> GlobalNonlocalExclusions {
    let mut exclusions = GlobalNonlocalExclusions::new();
    collect_scope_exclusions_recursive(tree, root, content, &mut exclusions);
    exclusions
}

fn collect_scope_exclusions_recursive(
    tree: &PythonScopeTree,
    node: Node,
    content: &[u8],
    exclusions: &mut GlobalNonlocalExclusions,
) {
    if node.kind() == "function_definition"
        && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
        && let Some(body) = node.child_by_field_name("body")
    {
        let names = collect_global_nonlocal_names(body, content);
        if !names.is_empty() {
            exclusions.entry(scope_id).or_default().extend(names);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_scope_exclusions_recursive(tree, child, content, exclusions);
    }
}

/// Collect `global` and `nonlocal` names in a function scope.
///
/// These names should NOT be treated as local variables (they refer
/// to variables in outer scopes).
fn collect_global_nonlocal_names(func_body: Node, content: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_global_nonlocal_recursive(func_body, content, &mut names);
    names
}

fn collect_global_nonlocal_recursive(node: Node, content: &[u8], names: &mut HashSet<String>) {
    match node.kind() {
        "global_statement" | "nonlocal_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier"
                    && let Ok(name) = child.utf8_text(content)
                {
                    names.insert(name.to_string());
                }
            }
        }
        // Don't recurse into nested functions — they have their own global/nonlocal
        "function_definition" | "class_definition" | "lambda" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_global_nonlocal_recursive(child, content, names);
            }
        }
    }
}
