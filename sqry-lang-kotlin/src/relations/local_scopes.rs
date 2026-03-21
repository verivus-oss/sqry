//! Local scope tracking and reference resolution for Kotlin.
//!
//! Builds a per-file scope tree, binds local declarations (val/var,
//! parameters, destructuring, lambda `it`), and resolves identifier
//! usages to declaration nodes for Reference edges.
//!
//! Uses the shared `ScopeTree<K>` infrastructure from `sqry_core::graph::local_scopes`
//! with Kotlin-specific scope kinds and AST patterns.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqry_core::graph::local_scopes::{
    ClassInfo, ClassInfoIndex, ClassMemberInfo, MemberSource, ScopeKindTrait, ScopeTree,
    StringInterner, first_child_of_kind, load_recursion_guard, resolve_class_info,
};
use sqry_core::graph::{GraphBuilderError, GraphResult, Span};
use tree_sitter::Node;

// Re-export shared types used by graph_builder.rs
pub(crate) use sqry_core::graph::local_scopes::{ResolutionOutcome, ScopeId};

// ============================================================================
// Kotlin ScopeKind
// ============================================================================

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[allow(dead_code)] // Block reserved for bare block scopes
pub(crate) enum ScopeKind {
    Method,
    Constructor,
    InitBlock,
    Block,
    IfBranch,
    ForLoop,
    WhileLoop,
    DoWhileLoop,
    TryBlock,
    CatchBlock,
    FinallyBlock,
    WhenBlock,
    WhenEntry,
    Lambda,
    AnonymousObject,
    LocalClass,
    LocalObject,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        matches!(
            self,
            ScopeKind::AnonymousObject | ScopeKind::LocalClass | ScopeKind::LocalObject
        )
    }

    fn is_overlap_boundary(&self) -> bool {
        self.is_class_scope()
    }

    // is_non_capturing_class_scope: default (false) — all Kotlin class scopes capture

    fn allows_nested_shadowing(&self) -> bool {
        true // Kotlin allows shadowing in nested scopes
    }

    // blocks_capture_chain: default (is_non_capturing_class_scope() || is_class_scope())
    // Since is_non_capturing is always false, this is equivalent to is_class_scope()

    fn strict_unresolved_bases(&self) -> bool {
        // When bases are unresolved, do NOT return Ambiguous. Instead, return
        // None to allow the capture chain to proceed. This avoids suppressing
        // outer-scope captures for local classes implementing interfaces or
        // delegated bases.
        false
    }
}

// ============================================================================
// Type alias
// ============================================================================

pub(crate) type KotlinScopeTree = ScopeTree<ScopeKind>;

// ============================================================================
// Build entry point
// ============================================================================

pub(crate) fn build(root: Node, content: &[u8]) -> GraphResult<KotlinScopeTree> {
    let mut scope_tree = ScopeTree::new(content.len());
    scope_tree.class_infos = build_class_info_index(&mut scope_tree, root, content);
    build_scopes(&mut scope_tree, root, content)?;
    scope_tree.rebuild_index();
    bind_declarations(&mut scope_tree, root, content)?;
    scope_tree.rebuild_index();
    Ok(scope_tree)
}

// ============================================================================
// Build phase wrappers
// ============================================================================

fn build_class_info_index(
    tree: &mut KotlinScopeTree,
    root: Node,
    content: &[u8],
) -> ClassInfoIndex {
    let mut index = ClassInfoIndex::default();
    let mut class_stack = Vec::new();
    let mut guard = load_recursion_guard();
    let _ = collect_class_infos(
        root,
        content,
        &mut index,
        &mut class_stack,
        &mut tree.interner,
        &mut guard,
    );
    index
}

fn build_scopes(tree: &mut KotlinScopeTree, root: Node, content: &[u8]) -> GraphResult<()> {
    let mut guard = load_recursion_guard();
    build_scopes_recursive(tree, root, content, None, &mut guard, &mut Vec::new())
}

fn bind_declarations(tree: &mut KotlinScopeTree, root: Node, content: &[u8]) -> GraphResult<()> {
    let mut guard = load_recursion_guard();
    bind_declarations_recursive(tree, root, content, &mut guard)
}

// ============================================================================
// Class info collection
// ============================================================================

fn collect_class_infos(
    node: Node,
    content: &[u8],
    index: &mut ClassInfoIndex,
    class_stack: &mut Vec<String>,
    interner: &mut StringInterner,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    guard.enter()?;

    let mut pushed = false;
    if matches!(
        node.kind(),
        "class_declaration" | "object_declaration" | "interface_declaration"
    ) {
        let name_node = first_child_of_kind(node, "type_identifier");
        if let Some(name_node) = name_node {
            let name = name_node.utf8_text(content).unwrap_or("").to_string();
            if !name.is_empty() {
                class_stack.push(name.clone());
                pushed = true;
                let qualifier = class_stack.join("::");
                let declared_members = collect_class_member_names(node, content, interner, guard)?;
                let info = ClassInfo {
                    qualifier: qualifier.clone(),
                    declared_members,
                };
                let mut keys = Vec::new();
                keys.push(name.clone());
                if class_stack.len() > 1 {
                    let dotted = class_stack.join(".");
                    keys.push(dotted);
                    keys.push(qualifier.clone());
                }
                index.insert(info, &keys);
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_class_infos(child, content, index, class_stack, interner, guard)?;
    }

    if pushed {
        class_stack.pop();
    }

    guard.exit();
    Ok(())
}

fn collect_class_member_names(
    node: Node,
    content: &[u8],
    interner: &mut StringInterner,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<HashSet<Arc<str>>, sqry_core::query::security::RecursionError> {
    guard.enter()?;
    let mut members = HashSet::new();

    if let Some(body) = first_child_of_kind(node, "class_body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "property_declaration" => {
                    collect_property_members(child, content, interner, &mut members);
                }
                "function_declaration" => {
                    if let Some(name_node) = first_child_of_kind(child, "simple_identifier") {
                        let name = name_node.utf8_text(content).unwrap_or("");
                        if !name.is_empty() {
                            members.insert(interner.intern(name));
                        }
                    }
                }
                "companion_object" => {
                    collect_companion_members(child, content, interner, &mut members);
                }
                _ => {}
            }
        }
    }

    collect_primary_constructor_members(node, content, interner, &mut members);

    guard.exit();
    Ok(members)
}

/// Extract property member names from a `property_declaration` node.
///
/// Handles both simple (`val x = ...`) and multi-variable (`val (a, b) = ...`) declarations.
fn collect_property_members(
    child: Node,
    content: &[u8],
    interner: &mut StringInterner,
    members: &mut HashSet<Arc<str>>,
) {
    // val/var property: property_declaration -> variable_declaration -> simple_identifier
    if let Some(var_decl) = first_child_of_kind(child, "variable_declaration")
        && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
    {
        let name = name_node.utf8_text(content).unwrap_or("");
        if !name.is_empty() {
            members.insert(interner.intern(name));
        }
    }
    // Multi-variable: val (a, b) = pair
    if let Some(multi) = first_child_of_kind(child, "multi_variable_declaration") {
        let mut mc = multi.walk();
        for var_decl in multi.children(&mut mc) {
            if var_decl.kind() == "variable_declaration"
                && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    members.insert(interner.intern(name));
                }
            }
        }
    }
}

/// Extract member names from a `companion_object` node's body.
fn collect_companion_members(
    child: Node,
    content: &[u8],
    interner: &mut StringInterner,
    members: &mut HashSet<Arc<str>>,
) {
    if let Some(comp_body) = first_child_of_kind(child, "class_body") {
        let mut cc = comp_body.walk();
        for comp_child in comp_body.children(&mut cc) {
            if comp_child.kind() == "property_declaration"
                && let Some(var_decl) = first_child_of_kind(comp_child, "variable_declaration")
                && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    members.insert(interner.intern(name));
                }
            }
            if comp_child.kind() == "function_declaration"
                && let Some(name_node) = first_child_of_kind(comp_child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    members.insert(interner.intern(name));
                }
            }
        }
    }
}

/// Extract val/var parameters from the primary constructor as class members.
fn collect_primary_constructor_members(
    node: Node,
    content: &[u8],
    interner: &mut StringInterner,
    members: &mut HashSet<Arc<str>>,
) {
    if let Some(primary_ctor) = first_child_of_kind(node, "primary_constructor")
        && let Some(params) = first_child_of_kind(primary_ctor, "class_parameters")
    {
        let mut pc = params.walk();
        for param in params.children(&mut pc) {
            if param.kind() == "class_parameter" {
                let has_val_var = {
                    let mut mc = param.walk();
                    param
                        .children(&mut mc)
                        .any(|c| c.kind() == "val" || c.kind() == "var")
                };
                if has_val_var
                    && let Some(name_node) = first_child_of_kind(param, "simple_identifier")
                {
                    let name = name_node.utf8_text(content).unwrap_or("");
                    if !name.is_empty() {
                        members.insert(interner.intern(name));
                    }
                }
            }
        }
    }
}

// ============================================================================
// Scope building
// ============================================================================

fn build_scopes_recursive(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::from_bytes(node.start_byte(), node.end_byte()),
        reason: format!("Recursion limit: {e}"),
    })?;
    let result = build_scopes_dispatch(tree, node, content, current_scope, guard, class_stack);
    guard.exit();
    result
}

/// Recurse into all children of a node in the given scope.
fn recurse_children(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        build_scopes_recursive(tree, child, content, scope, guard, class_stack)?;
    }
    Ok(())
}

/// Add a scope on the node bounds and recurse all children in that scope.
///
/// Returns `true` if the scope was created and children recursed, `false` if
/// scope creation failed (caller should fall through to default).
fn scope_node_and_recurse_children(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    kind: ScopeKind,
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<bool> {
    if let Some(scope_id) = tree.add_scope(kind, node.start_byte(), node.end_byte(), current_scope)
    {
        recurse_children(tree, node, content, Some(scope_id), guard, class_stack)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Iterate children; `control_structure_body` children get a new scope, others
/// recurse in `current_scope`. Used for if, while, and do-while.
fn handle_control_body_scope(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    kind: ScopeKind,
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "control_structure_body" {
            let scope_id =
                tree.add_scope(kind, child.start_byte(), child.end_byte(), current_scope);
            build_scopes_recursive(
                tree,
                child,
                content,
                scope_id.or(current_scope),
                guard,
                class_stack,
            )?;
        } else {
            build_scopes_recursive(tree, child, content, current_scope, guard, class_stack)?;
        }
    }
    Ok(())
}

/// Find a named body child, add a scope on it, and recurse only the body.
///
/// Returns `true` if the body was found (scope creation may still fail, but we
/// still consider it handled — strict let-chain means we only recurse if scope
/// is created). Returns `false` if no body child exists (caller falls through).
fn handle_function_body_scope(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    kind: ScopeKind,
    body_kind: &str,
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<bool> {
    if let Some(body) = first_child_of_kind(node, body_kind) {
        if let Some(scope_id) =
            tree.add_scope(kind, body.start_byte(), body.end_byte(), current_scope)
        {
            build_scopes_recursive(tree, body, content, Some(scope_id), guard, class_stack)?;
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Handle `secondary_constructor` with triple fallback:
/// `statements` -> `function_body` -> block-body loop.
fn handle_secondary_constructor(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<bool> {
    // Try statements or function_body child
    if let Some(body) = first_child_of_kind(node, "statements")
        .or_else(|| first_child_of_kind(node, "function_body"))
    {
        if let Some(scope_id) = tree.add_scope(
            ScopeKind::Constructor,
            body.start_byte(),
            body.end_byte(),
            current_scope,
        ) {
            build_scopes_recursive(tree, body, content, Some(scope_id), guard, class_stack)?;
        }
        return Ok(true);
    }
    // Constructor with block body: `constructor() { ... }`
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "{" || child.kind() == "}" {
            continue;
        }
        if child.kind() == "statements" {
            if let Some(scope_id) = tree.add_scope(
                ScopeKind::Constructor,
                child.start_byte(),
                child.end_byte(),
                current_scope,
            ) {
                build_scopes_recursive(tree, child, content, Some(scope_id), guard, class_stack)?;
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// Handle `try_expression`: the `statements` child gets a `TryBlock` scope, others recurse
/// in current scope (catch/finally scopes created when matched directly).
fn handle_try_expression(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "statements" {
            let scope_id = tree.add_scope(
                ScopeKind::TryBlock,
                child.start_byte(),
                child.end_byte(),
                current_scope,
            );
            build_scopes_recursive(
                tree,
                child,
                content,
                scope_id.or(current_scope),
                guard,
                class_stack,
            )?;
        } else {
            build_scopes_recursive(tree, child, content, current_scope, guard, class_stack)?;
        }
    }
    Ok(())
}

/// Handle `when_expression`: outer `WhenBlock` scope, inner `WhenEntry` scope per entry.
fn handle_when_expression(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    if let Some(scope_id) = tree.add_scope(
        ScopeKind::WhenBlock,
        node.start_byte(),
        node.end_byte(),
        current_scope,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "when_entry" {
                let entry_scope = tree.add_scope(
                    ScopeKind::WhenEntry,
                    child.start_byte(),
                    child.end_byte(),
                    Some(scope_id),
                );
                build_scopes_recursive(
                    tree,
                    child,
                    content,
                    entry_scope.or(Some(scope_id)),
                    guard,
                    class_stack,
                )?;
            } else {
                build_scopes_recursive(tree, child, content, Some(scope_id), guard, class_stack)?;
            }
        }
        Ok(())
    } else {
        recurse_children(tree, node, content, current_scope, guard, class_stack)
    }
}

/// Handle `object_literal`: add `AnonymousObject` scope, record members, recurse children.
fn handle_object_literal(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<bool> {
    if let Some(scope_id) = tree.add_scope(
        ScopeKind::AnonymousObject,
        node.start_byte(),
        node.end_byte(),
        current_scope,
    ) {
        record_anonymous_object_members(tree, node, content, scope_id);
        recurse_children(tree, node, content, Some(scope_id), guard, class_stack)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Handle `class_declaration` and `object_declaration`: `class_stack` push/pop with
/// local type detection. Guarantees `class_stack.pop()` on all paths via closure.
fn handle_class_declaration(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut pushed = false;
    if let Some(name_node) = first_child_of_kind(node, "type_identifier") {
        let class_name = name_node.utf8_text(content).unwrap_or("").to_string();
        if !class_name.is_empty() {
            class_stack.push(class_name);
            pushed = true;
        }
    }

    // Use closure to guarantee class_stack.pop() on all paths (success, error, fallthrough)
    let result = (|| {
        if pushed && is_local_type_declaration(node) {
            let qualifier = class_stack.join("::");
            let kind = if node.kind() == "object_declaration" {
                ScopeKind::LocalObject
            } else {
                ScopeKind::LocalClass
            };
            if let Some(body) = first_child_of_kind(node, "class_body")
                && let Some(scope_id) =
                    tree.add_scope(kind, body.start_byte(), body.end_byte(), current_scope)
            {
                record_named_class_members(tree, node, content, scope_id, qualifier);
                return build_scopes_recursive(
                    tree,
                    body,
                    content,
                    Some(scope_id),
                    guard,
                    class_stack,
                );
            }
        }
        recurse_children(tree, node, content, current_scope, guard, class_stack)
    })();

    if pushed {
        class_stack.pop();
    }
    result
}

/// Dispatch on Kotlin AST node kind to build scope tree entries.
fn build_scopes_dispatch(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    match node.kind() {
        "function_declaration" | "anonymous_function" | "getter" | "setter" => {
            if handle_function_body_scope(
                tree,
                node,
                content,
                ScopeKind::Method,
                "function_body",
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "secondary_constructor" => {
            if handle_secondary_constructor(tree, node, content, current_scope, guard, class_stack)?
            {
                return Ok(());
            }
        }
        "anonymous_initializer" => {
            if scope_node_and_recurse_children(
                tree,
                node,
                content,
                ScopeKind::InitBlock,
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "if_expression" => {
            return handle_control_body_scope(
                tree,
                node,
                content,
                ScopeKind::IfBranch,
                current_scope,
                guard,
                class_stack,
            );
        }
        "for_statement" => {
            // Scope covers entire for statement so loop var is visible in body
            if scope_node_and_recurse_children(
                tree,
                node,
                content,
                ScopeKind::ForLoop,
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "while_statement" => {
            return handle_control_body_scope(
                tree,
                node,
                content,
                ScopeKind::WhileLoop,
                current_scope,
                guard,
                class_stack,
            );
        }
        "do_while_statement" => {
            return handle_control_body_scope(
                tree,
                node,
                content,
                ScopeKind::DoWhileLoop,
                current_scope,
                guard,
                class_stack,
            );
        }
        "try_expression" => {
            return handle_try_expression(tree, node, content, current_scope, guard, class_stack);
        }
        "catch_block" => {
            if scope_node_and_recurse_children(
                tree,
                node,
                content,
                ScopeKind::CatchBlock,
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "finally_block" => {
            if scope_node_and_recurse_children(
                tree,
                node,
                content,
                ScopeKind::FinallyBlock,
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "when_expression" => {
            return handle_when_expression(tree, node, content, current_scope, guard, class_stack);
        }
        "lambda_literal" => {
            if scope_node_and_recurse_children(
                tree,
                node,
                content,
                ScopeKind::Lambda,
                current_scope,
                guard,
                class_stack,
            )? {
                return Ok(());
            }
        }
        "object_literal" => {
            if handle_object_literal(tree, node, content, current_scope, guard, class_stack)? {
                return Ok(());
            }
        }
        "class_declaration" | "object_declaration" => {
            return handle_class_declaration(
                tree,
                node,
                content,
                current_scope,
                guard,
                class_stack,
            );
        }
        _ => {}
    }
    // Default: recurse into children
    recurse_children(tree, node, content, current_scope, guard, class_stack)
}

fn is_local_type_declaration(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_body" {
            return false;
        }
        if matches!(
            parent.kind(),
            "function_declaration"
                | "function_body"
                | "lambda_literal"
                | "anonymous_initializer"
                | "secondary_constructor"
        ) {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn record_named_class_members(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    scope_id: ScopeId,
    qualifier: String,
) {
    let qualifier_name = qualifier.clone();
    let mut info = ClassMemberInfo {
        qualifier: Some(qualifier),
        declared_members: HashSet::new(),
        inherited_members: HashMap::new(),
        unresolved_base_count: 0,
        explicit_base_count: 0,
    };

    if let Some(name_node) = first_child_of_kind(node, "type_identifier") {
        let class_name = name_node.utf8_text(content).unwrap_or("");
        if let Some(class_info) = resolve_class_info(&tree.class_infos, &qualifier_name)
            .or_else(|| resolve_class_info(&tree.class_infos, class_name))
        {
            info.declared_members
                .clone_from(&class_info.declared_members);
        }
    }

    let base_types = extract_base_types(node, content);
    info.explicit_base_count = base_types.len();
    for base in base_types {
        if let Some(class_info) = resolve_class_info(&tree.class_infos, &base) {
            for member in &class_info.declared_members {
                info.inherited_members
                    .entry(member.clone())
                    .or_default()
                    .push(MemberSource {
                        qualifier: class_info.qualifier.clone(),
                    });
            }
        } else {
            info.unresolved_base_count += 1;
        }
    }

    tree.class_members.by_scope.insert(scope_id, info);
}

fn record_anonymous_object_members(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    scope_id: ScopeId,
) {
    let mut info = ClassMemberInfo {
        qualifier: None,
        declared_members: HashSet::new(),
        inherited_members: HashMap::new(),
        unresolved_base_count: 0,
        explicit_base_count: 0,
    };

    if let Some(body) = first_child_of_kind(node, "class_body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "property_declaration"
                && let Some(var_decl) = first_child_of_kind(child, "variable_declaration")
                && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    info.declared_members.insert(tree.interner.intern(name));
                }
            }
            if child.kind() == "function_declaration"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    info.declared_members.insert(tree.interner.intern(name));
                }
            }
        }
    }

    // Extract base types from delegation specifiers
    let base_types = extract_base_types(node, content);
    info.explicit_base_count = base_types.len();
    for base in base_types {
        if let Some(class_info) = resolve_class_info(&tree.class_infos, &base) {
            for member in &class_info.declared_members {
                info.inherited_members
                    .entry(member.clone())
                    .or_default()
                    .push(MemberSource {
                        qualifier: class_info.qualifier.clone(),
                    });
            }
        } else {
            info.unresolved_base_count += 1;
        }
    }

    tree.class_members.by_scope.insert(scope_id, info);
}

fn extract_base_types(node: Node, content: &[u8]) -> Vec<String> {
    let mut bases = Vec::new();
    // Look for delegation_specifiers (the list) which contains delegation_specifier children
    let specifiers_node = first_child_of_kind(node, "delegation_specifiers");
    let search_node = specifiers_node.unwrap_or(node);
    let mut cursor = search_node.walk();
    for child in search_node.children(&mut cursor) {
        if child.kind() == "delegation_specifier" {
            // Try to extract user_type child for cleaner parsing (skips `by` clauses)
            if let Some(user_type) = first_child_of_kind(child, "user_type") {
                let text = user_type
                    .utf8_text(content)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let normalized = normalize_type_key(&text);
                if !normalized.is_empty() {
                    bases.push(normalized);
                }
            } else if let Some(ctor_invoc) = first_child_of_kind(child, "constructor_invocation") {
                // constructor_invocation contains user_type + value_arguments
                if let Some(user_type) = first_child_of_kind(ctor_invoc, "user_type") {
                    let text = user_type
                        .utf8_text(content)
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let normalized = normalize_type_key(&text);
                    if !normalized.is_empty() {
                        bases.push(normalized);
                    }
                }
            } else {
                // Fallback: use raw text, strip `by ...` delegation clause
                let text = child.utf8_text(content).unwrap_or("").trim().to_string();
                let stripped = text.split(" by ").next().unwrap_or(&text).trim();
                let normalized = normalize_type_key(stripped);
                if !normalized.is_empty() {
                    bases.push(normalized);
                }
            }
        }
    }
    bases
}

fn normalize_type_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let no_generics = trimmed.split('<').next().unwrap_or(trimmed).trim();
    let no_parens = no_generics.split('(').next().unwrap_or(no_generics).trim();
    no_parens.to_string()
}

// ============================================================================
// Declaration binding
// ============================================================================

#[allow(clippy::too_many_lines)]
fn bind_declarations_recursive(
    tree: &mut KotlinScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::from_bytes(node.start_byte(), node.end_byte()),
        reason: format!("Recursion limit: {e}"),
    })?;

    match node.kind() {
        "property_declaration" => {
            bind_property_declaration(tree, node, content);
        }
        "for_statement" => {
            bind_for_variable(tree, node, content);
        }
        "catch_block" => {
            bind_catch_parameter(tree, node, content);
        }
        "lambda_literal" => {
            bind_lambda_parameters(tree, node, content);
        }
        "function_declaration" => {
            bind_function_parameters(tree, node, content);
        }
        "secondary_constructor" => {
            bind_constructor_parameters(tree, node, content);
        }
        "when_expression" => {
            bind_when_subject(tree, node, content);
        }
        "anonymous_function" => {
            bind_anonymous_function_parameters(tree, node, content);
        }
        "setter" => {
            bind_setter_parameter(tree, node, content);
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

/// Bind val/var property declarations inside function bodies.
fn bind_property_declaration(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    // Only bind local properties (inside function bodies), not class members
    if !is_inside_function_body(node) {
        return;
    }

    // Handle destructuring: val (a, b) = pair
    if let Some(multi) = first_child_of_kind(node, "multi_variable_declaration") {
        let mut cursor = multi.walk();
        for child in multi.children(&mut cursor) {
            if child.kind() == "variable_declaration"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let decl_start = name_node.start_byte();
                let decl_end = name_node.end_byte();
                let declarator_end = node.end_byte();
                // Initializer is the expression after `=`
                let initializer_start = find_property_initializer(node).map(|n| n.start_byte());
                if let Some(scope_id) = tree.innermost_scope_at(decl_start) {
                    tree.add_binding(
                        scope_id,
                        name,
                        decl_start,
                        decl_end,
                        declarator_end,
                        initializer_start,
                    );
                }
            }
        }
        return;
    }

    // Simple property: val x = expr
    // AST: property_declaration -> variable_declaration -> simple_identifier
    if let Some(var_decl) = first_child_of_kind(node, "variable_declaration")
        && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
    {
        let name = name_node.utf8_text(content).unwrap_or("");
        if name.is_empty() {
            return;
        }
        let decl_start = name_node.start_byte();
        let decl_end = name_node.end_byte();
        let declarator_end = node.end_byte();
        let initializer_start = find_property_initializer(node).map(|n| n.start_byte());
        if let Some(scope_id) = tree.innermost_scope_at(decl_start) {
            tree.add_binding(
                scope_id,
                name,
                decl_start,
                decl_end,
                declarator_end,
                initializer_start,
            );
        }
    }
}

fn find_property_initializer(node: Node) -> Option<Node> {
    // In Kotlin AST: property_declaration has children like:
    // val simple_identifier : type = expression
    // We look for the expression after `=`
    let mut found_eq = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "=" {
            found_eq = true;
            continue;
        }
        if found_eq && child.kind() != "=" {
            return Some(child);
        }
    }
    None
}

fn is_inside_function_body(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_body"
            | "lambda_literal"
            | "anonymous_initializer"
            | "anonymous_function"
            | "getter"
            | "setter" => {
                return true;
            }
            "class_body" => return false,
            _ => {}
        }
        current = parent.parent();
    }
    false
}

/// Bind for-loop variable: `for (x in list)` or `for ((a, b) in list)`
fn bind_for_variable(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    // tree-sitter-kotlin: for_statement children are:
    // "for" "(" variable_declaration/multi_variable_declaration "in" expression ")" body
    //
    // The loop variable is only visible in the body, not the iterable expression.
    // We use the self-reference prevention mechanism: set initializer_start to the
    // variable declaration start, and declarator_end to the body start. This makes
    // any usage between the variable name and the body start appear as a
    // "self-reference" and get skipped, so `for (x in listOf(x))` resolves the
    // iterable `x` to the outer scope.
    let body_start = find_for_body_start(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration"
            && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
        {
            let name = name_node.utf8_text(content).unwrap_or("");
            if !name.is_empty()
                && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
            {
                let decl_start = name_node.start_byte();
                let decl_end = name_node.end_byte();
                // declarator_end covers through the for-header end (body start)
                let declarator_end = body_start.unwrap_or(decl_end);
                // initializer covers the for-header to prevent iterable resolution
                let initializer_start = Some(decl_start);
                tree.add_binding(
                    scope_id,
                    name,
                    decl_start,
                    decl_end,
                    declarator_end,
                    initializer_start,
                );
            }
        }
        if child.kind() == "multi_variable_declaration" {
            // Destructuring: for ((a, b) in list)
            let mut mc = child.walk();
            for var_decl in child.children(&mut mc) {
                if var_decl.kind() == "variable_declaration"
                    && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
                {
                    let name = name_node.utf8_text(content).unwrap_or("");
                    if !name.is_empty()
                        && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                    {
                        let decl_start = name_node.start_byte();
                        let decl_end = name_node.end_byte();
                        let declarator_end = body_start.unwrap_or(decl_end);
                        let initializer_start = Some(decl_start);
                        tree.add_binding(
                            scope_id,
                            name,
                            decl_start,
                            decl_end,
                            declarator_end,
                            initializer_start,
                        );
                    }
                }
            }
        }
    }
}

/// Find the start byte of the for-loop body (after the `)` closing paren).
fn find_for_body_start(node: Node) -> Option<usize> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // The body is a control_structure_body or statements after the closing `)`
        if child.kind() == "control_structure_body" || child.kind() == "statements" {
            return Some(child.start_byte());
        }
    }
    None
}

/// Bind catch exception variable: `catch (e: Exception)`
fn bind_catch_parameter(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    // catch_block children: "catch" "(" simple_identifier ":" type ")" "{" statements "}"
    if let Some(name_node) = first_child_of_kind(node, "simple_identifier") {
        let name = name_node.utf8_text(content).unwrap_or("");
        if !name.is_empty()
            && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
        {
            let decl_start = name_node.start_byte();
            let decl_end = name_node.end_byte();
            tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
        }
    }
}

/// Check if a lambda body actually uses the `it` identifier.
///
/// Recursively scans the lambda's statements for any `simple_identifier` node
/// whose text is `"it"`. Stops at nested `lambda_literal` boundaries to avoid
/// finding `it` usages that belong to inner lambdas.
fn lambda_body_uses_it(lambda_node: Node, content: &[u8]) -> bool {
    fn scan_for_it(node: Node, content: &[u8]) -> bool {
        // Don't recurse into nested lambdas — their `it` is their own
        if node.kind() == "lambda_literal" {
            return false;
        }
        if node.kind() == "simple_identifier" && node.utf8_text(content).unwrap_or("") == "it" {
            return true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if scan_for_it(child, content) {
                return true;
            }
        }
        false
    }

    // Scan all children of the lambda body. The body is typically a `statements`
    // node, but the grammar could also emit a direct expression body. Scanning
    // all non-lambda children handles both shapes. `scan_for_it` already stops
    // at nested `lambda_literal` boundaries.
    let mut cursor = lambda_node.walk();
    for child in lambda_node.children(&mut cursor) {
        if scan_for_it(child, content) {
            return true;
        }
    }
    false
}

/// Bind lambda parameters: `{ x, y -> ... }` or implicit `it`
fn bind_lambda_parameters(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    let Some(scope_id) = tree.innermost_scope_at(node.start_byte()) else {
        return;
    };

    // Check for explicit lambda_parameters
    let lambda_params = first_child_of_kind(node, "lambda_parameters");

    if let Some(params) = lambda_params {
        // Explicit parameters: { x, y -> ... } or destructuring { (a, b) -> ... }
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "variable_declaration"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    let decl_start = name_node.start_byte();
                    let decl_end = name_node.end_byte();
                    tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                }
            }
            // Destructuring lambda parameters: { (a, b) -> ... }
            if child.kind() == "multi_variable_declaration" {
                let mut mc = child.walk();
                for var_decl in child.children(&mut mc) {
                    if var_decl.kind() == "variable_declaration"
                        && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
                    {
                        let name = name_node.utf8_text(content).unwrap_or("");
                        if !name.is_empty() {
                            let decl_start = name_node.start_byte();
                            let decl_end = name_node.end_byte();
                            tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                        }
                    }
                }
            }
        }
    } else {
        // No explicit parameters -> implicit `it` if the lambda actually uses `it`.
        // Guard: only create the `it` binding when we detect an actual `it` identifier
        // in the lambda body. This prevents false Reference edges in parameterless
        // lambdas where `it` is not valid Kotlin (e.g., `run { println("hello") }`).
        if lambda_body_uses_it(node, content) {
            let decl_start = node.start_byte();
            let decl_end = node.start_byte();
            tree.add_binding(scope_id, "it", decl_start, decl_end, decl_end, None);
        }
    }
}

/// Bind function parameters: `fun foo(x: Int, y: String)`
fn bind_function_parameters(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    let Some(body) = first_child_of_kind(node, "function_body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    if let Some(params) = first_child_of_kind(node, "function_value_parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    let decl_start = name_node.start_byte();
                    let decl_end = name_node.end_byte();
                    tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                }
            }
        }
    }
}

/// Bind constructor parameters: `constructor(x: Int)`
fn bind_constructor_parameters(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    // Find the body scope
    let body = first_child_of_kind(node, "statements")
        .or_else(|| first_child_of_kind(node, "function_body"));
    let Some(body) = body else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    if let Some(params) = first_child_of_kind(node, "function_value_parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    let decl_start = name_node.start_byte();
                    let decl_end = name_node.end_byte();
                    tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                }
            }
        }
    }
}

/// Bind `when` subject variable: `when (val x = expr) { ... }`
fn bind_when_subject(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    // tree-sitter-kotlin: when_expression may have a when_subject child
    // that contains a property_declaration (val x = ...) or variable_declaration
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "when_subject" {
            let mut sc = child.walk();
            for sub in child.children(&mut sc) {
                if sub.kind() == "property_declaration"
                    && let Some(var_decl) = first_child_of_kind(sub, "variable_declaration")
                    && let Some(name_node) = first_child_of_kind(var_decl, "simple_identifier")
                {
                    let name = name_node.utf8_text(content).unwrap_or("");
                    if !name.is_empty()
                        && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                    {
                        let decl_start = name_node.start_byte();
                        let decl_end = name_node.end_byte();
                        let declarator_end = sub.end_byte();
                        let initializer_start =
                            find_property_initializer(sub).map(|n| n.start_byte());
                        tree.add_binding(
                            scope_id,
                            name,
                            decl_start,
                            decl_end,
                            declarator_end,
                            initializer_start,
                        );
                    }
                }
                // Also handle bare variable_declaration
                if sub.kind() == "variable_declaration"
                    && let Some(name_node) = first_child_of_kind(sub, "simple_identifier")
                {
                    let name = name_node.utf8_text(content).unwrap_or("");
                    if !name.is_empty()
                        && let Some(scope_id) = tree.innermost_scope_at(node.start_byte())
                    {
                        let decl_start = name_node.start_byte();
                        let decl_end = name_node.end_byte();
                        tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                    }
                }
            }
        }
    }
}

/// Bind anonymous function parameters: `val f = fun(x: Int) { x + 1 }`
fn bind_anonymous_function_parameters(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    let Some(body) = first_child_of_kind(node, "function_body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    if let Some(params) = first_child_of_kind(node, "function_value_parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter"
                && let Some(name_node) = first_child_of_kind(child, "simple_identifier")
            {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    let decl_start = name_node.start_byte();
                    let decl_end = name_node.end_byte();
                    tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                }
            }
        }
    }
}

/// Bind setter parameter: `set(value) { field = value }`
///
/// The setter parameter is the explicitly named parameter in the setter declaration.
/// Note: the synthetic `field` and `value` identifiers are NOT bound here -- they are
/// documented as out-of-scope (synthetics require compiler knowledge).
fn bind_setter_parameter(tree: &mut KotlinScopeTree, node: Node, content: &[u8]) {
    let Some(body) = first_child_of_kind(node, "function_body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body.start_byte()) else {
        return;
    };

    // tree-sitter-kotlin AST: setter -> "set" "(" parameter_with_optional_type ")" function_body
    // The parameter is a direct child of the setter node, NOT wrapped in function_value_parameters.
    if let Some(param) = first_child_of_kind(node, "parameter_with_optional_type")
        && let Some(name_node) = first_child_of_kind(param, "simple_identifier")
    {
        let name = name_node.utf8_text(content).unwrap_or("");
        if !name.is_empty() {
            let decl_start = name_node.start_byte();
            let decl_end = name_node.end_byte();
            tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
        }
    }
}
