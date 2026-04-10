//! Local scope tracking and reference resolution for Java.
//!
//! This module builds a per-file scope tree, binds local declarations
//! (including pattern variables), and resolves identifier usages to
//! declaration nodes for Reference edges.
//!
//! Uses the shared `ScopeTree<K>` infrastructure from `sqry_core::graph::local_scopes`
//! with Java-specific scope kinds and AST patterns.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sqry_classpath::stub::index::ClasspathIndex;
use sqry_classpath::stub::model::ClassStub;
use sqry_core::graph::local_scopes::{
    ClassInfo, ClassInfoIndex, ClassMemberInfo, DebugEvent, MemberSource, ScopeKindTrait,
    ScopeTree, StringInterner, first_child_of_kind, load_recursion_guard, resolve_class_info,
};
use sqry_core::graph::{GraphBuilderError, GraphResult, Span};
use tree_sitter::Node;

// Re-export shared types used by graph_builder.rs
pub(crate) use sqry_core::graph::local_scopes::{ResolutionOutcome, ScopeId};

// ============================================================================
// Java ScopeKind
// ============================================================================

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScopeKind {
    Method,
    Constructor,
    StaticInitializer,
    InstanceInitializer,
    Block,
    IfBranch,
    ForLoop,
    EnhancedFor,
    WhileLoop,
    DoWhileLoop,
    TryBlock,
    TryWithResources,
    CatchBlock,
    FinallyBlock,
    SwitchBlock,
    SwitchRule,
    SwitchGroup,
    SwitchGuard,
    StatementFlow,
    TernaryTrue,
    AndRhs,
    Lambda,
    AnonymousClass,
    LocalClass,
    LocalRecord,
    LocalEnum,
    LocalInterface,
}

impl ScopeKindTrait for ScopeKind {
    fn is_class_scope(&self) -> bool {
        matches!(
            self,
            ScopeKind::AnonymousClass
                | ScopeKind::LocalClass
                | ScopeKind::LocalRecord
                | ScopeKind::LocalEnum
                | ScopeKind::LocalInterface
        )
    }

    fn is_overlap_boundary(&self) -> bool {
        self.is_class_scope()
    }

    fn is_non_capturing_class_scope(&self) -> bool {
        matches!(
            self,
            ScopeKind::LocalRecord | ScopeKind::LocalEnum | ScopeKind::LocalInterface
        )
    }

    /// Java anonymous classes and local classes CAN capture effectively-final
    /// variables from enclosing scopes. Only non-capturing class scopes
    /// (records, enums, interfaces) block the capture chain.
    fn blocks_capture_chain(&self) -> bool {
        self.is_non_capturing_class_scope()
    }
}

// ============================================================================
// Type alias and build entry point
// ============================================================================

/// Java-specific scope tree type.
pub(crate) type JavaScopeTree = ScopeTree<ScopeKind>;

static CLASSPATH_INDEX_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<ClasspathIndex>>>>> =
    OnceLock::new();

// ============================================================================
// Build entry point
// ============================================================================

/// Build a Java scope tree from the given AST root node.
pub(crate) fn build(root: Node, content: &[u8], file: Option<&Path>) -> GraphResult<JavaScopeTree> {
    let mut scope_tree = ScopeTree::new(content.len());
    let classpath_index = file.and_then(load_classpath_index_for_file);
    scope_tree.class_infos =
        build_class_info_index(&mut scope_tree, root, content, classpath_index.as_deref());
    build_scopes_internal(&mut scope_tree, root, content)?;
    scope_tree.rebuild_index();
    bind_declarations_internal(&mut scope_tree, root, content)?;
    bind_pattern_scopes_internal(&mut scope_tree, root, content)?;
    scope_tree.rebuild_index();
    Ok(scope_tree)
}

// ============================================================================
// Build phase wrappers
// ============================================================================

fn build_class_info_index(
    tree: &mut JavaScopeTree,
    root: Node,
    content: &[u8],
    classpath_index: Option<&ClasspathIndex>,
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
    let import_map = collect_import_type_map(root, content);
    if let Some(classpath_index) = classpath_index {
        hydrate_classpath_infos(
            root,
            content,
            &import_map,
            &mut index,
            &mut tree.interner,
            classpath_index,
        );
    } else {
        seed_well_known_java_types(&mut index);
    }
    index
}

fn seed_well_known_java_types(index: &mut ClassInfoIndex) {
    const WELL_KNOWN_TYPES: &[(&str, &str)] = &[
        ("Object", "java.lang.Object"),
        ("Runnable", "java.lang.Runnable"),
        ("AutoCloseable", "java.lang.AutoCloseable"),
        ("Comparable", "java.lang.Comparable"),
        ("CharSequence", "java.lang.CharSequence"),
        ("Iterable", "java.lang.Iterable"),
        ("Cloneable", "java.lang.Cloneable"),
        ("String", "java.lang.String"),
        ("Number", "java.lang.Number"),
        ("Enum", "java.lang.Enum"),
        ("Record", "java.lang.Record"),
        ("Serializable", "java.io.Serializable"),
        ("Collection", "java.util.Collection"),
        ("List", "java.util.List"),
        ("Set", "java.util.Set"),
        ("Map", "java.util.Map"),
        ("Callable", "java.util.concurrent.Callable"),
    ];

    for (simple_name, qualifier) in WELL_KNOWN_TYPES {
        index.insert(
            ClassInfo {
                qualifier: (*qualifier).to_string(),
                declared_members: HashSet::new(),
            },
            &[(*simple_name).to_string(), (*qualifier).to_string()],
        );
    }
}

fn load_classpath_index_for_file(file: &Path) -> Option<Arc<ClasspathIndex>> {
    let mut current_dir = file.parent();
    while let Some(dir) = current_dir {
        let index_path = dir.join(".sqry/classpath/index.sqry");
        if index_path.exists() {
            let cache = CLASSPATH_INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            let mut cache = cache.lock().ok()?;
            if let Some(cached) = cache.get(&index_path) {
                return cached.clone();
            }

            let loaded = ClasspathIndex::load(&index_path).ok().map(Arc::new);
            cache.insert(index_path, loaded.clone());
            return loaded;
        }
        current_dir = dir.parent();
    }
    None
}

fn collect_import_type_map(root: Node, content: &[u8]) -> HashMap<String, String> {
    let mut import_map = HashMap::new();
    collect_import_type_map_recursive(root, content, &mut import_map);
    import_map
}

fn collect_import_type_map_recursive(
    node: Node,
    content: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    if node.kind() == "import_declaration" {
        let import_name = extract_import_name(node, content);
        if !import_name.is_empty()
            && !import_name.ends_with(".*")
            && let Some(simple_name) = import_name.rsplit('.').next()
        {
            import_map
                .entry(simple_name.to_string())
                .or_insert(import_name);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_import_type_map_recursive(child, content, import_map);
    }
}

fn extract_import_name(node: Node, content: &[u8]) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "scoped_identifier" | "identifier") {
            return child.utf8_text(content).unwrap_or("").to_string();
        }
    }
    String::new()
}

fn hydrate_classpath_infos(
    root: Node,
    content: &[u8],
    import_map: &HashMap<String, String>,
    index: &mut ClassInfoIndex,
    interner: &mut StringInterner,
    classpath_index: &ClasspathIndex,
) {
    let mut referenced_bases = HashSet::new();
    collect_referenced_base_types(root, content, &mut referenced_bases);

    let mut visiting = HashSet::new();
    for base in referenced_bases {
        ensure_classpath_info(
            &base,
            import_map,
            index,
            interner,
            classpath_index,
            &mut visiting,
        );
    }
}

fn collect_referenced_base_types(node: Node, content: &[u8], output: &mut HashSet<String>) {
    match node.kind() {
        "class_declaration" | "record_declaration" | "interface_declaration" => {
            for base in extract_base_types(node, content) {
                if !base.is_empty() {
                    output.insert(base);
                }
            }
        }
        "object_creation_expression" => {
            if first_child_of_kind(node, "class_body").is_some()
                && let Some(type_node) = node.child_by_field_name("type")
            {
                let base_name = normalize_type_key(type_node.utf8_text(content).unwrap_or(""));
                if !base_name.is_empty() {
                    output.insert(base_name);
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_referenced_base_types(child, content, output);
    }
}

fn ensure_classpath_info(
    base: &str,
    import_map: &HashMap<String, String>,
    index: &mut ClassInfoIndex,
    interner: &mut StringInterner,
    classpath_index: &ClasspathIndex,
    visiting: &mut HashSet<String>,
) {
    let Some(fqn) = lookup_classpath_fqn(base, import_map, classpath_index) else {
        return;
    };
    if resolve_class_info(index, &fqn).is_some() {
        return;
    }
    if !visiting.insert(fqn.clone()) {
        return;
    }

    let Some(stub) = classpath_index.lookup_fqn(&fqn) else {
        visiting.remove(&fqn);
        return;
    };

    let declared_members =
        collect_classpath_members(stub, import_map, index, interner, classpath_index, visiting);
    let keys = vec![stub.name.clone(), stub.fqn.clone()];
    index.insert(
        ClassInfo {
            qualifier: stub.fqn.clone(),
            declared_members,
        },
        &keys,
    );

    visiting.remove(&fqn);
}

fn collect_classpath_members(
    stub: &ClassStub,
    import_map: &HashMap<String, String>,
    index: &mut ClassInfoIndex,
    interner: &mut StringInterner,
    classpath_index: &ClasspathIndex,
    visiting: &mut HashSet<String>,
) -> HashSet<Arc<str>> {
    let mut members = HashSet::new();

    for field in &stub.fields {
        members.insert(interner.intern(&field.name));
    }
    for enum_constant in &stub.enum_constants {
        members.insert(interner.intern(enum_constant));
    }
    for component in &stub.record_components {
        members.insert(interner.intern(&component.name));
    }

    if let Some(superclass) = &stub.superclass {
        ensure_classpath_info(
            superclass,
            import_map,
            index,
            interner,
            classpath_index,
            visiting,
        );
        if let Some(parent_info) = resolve_class_info(index, superclass) {
            members.extend(parent_info.declared_members.iter().cloned());
        }
    }

    for interface_name in &stub.interfaces {
        ensure_classpath_info(
            interface_name,
            import_map,
            index,
            interner,
            classpath_index,
            visiting,
        );
        if let Some(interface_info) = resolve_class_info(index, interface_name) {
            members.extend(interface_info.declared_members.iter().cloned());
        }
    }

    members
}

fn lookup_classpath_fqn(
    base: &str,
    import_map: &HashMap<String, String>,
    classpath_index: &ClasspathIndex,
) -> Option<String> {
    let normalized = base.replace("::", ".");
    if classpath_index.lookup_fqn(&normalized).is_some() {
        return Some(normalized);
    }

    if let Some(imported) = import_map.get(base)
        && classpath_index.lookup_fqn(imported).is_some()
    {
        return Some(imported.clone());
    }

    let simple_name = normalized.rsplit('.').next().unwrap_or(&normalized);
    let mut matches = classpath_index
        .classes
        .iter()
        .filter(|stub| stub.name == simple_name)
        .map(|stub| stub.fqn.clone());
    let first_match = matches.next()?;
    if matches.next().is_none() {
        return Some(first_match);
    }

    None
}

fn build_scopes_internal(tree: &mut JavaScopeTree, root: Node, content: &[u8]) -> GraphResult<()> {
    let mut guard = load_recursion_guard();
    build_scopes_recursive(tree, root, content, None, &mut guard, &mut Vec::new())
}

fn bind_declarations_internal(
    tree: &mut JavaScopeTree,
    root: Node,
    content: &[u8],
) -> GraphResult<()> {
    let mut guard = load_recursion_guard();
    bind_declarations_recursive(tree, root, content, &mut guard)
}

fn bind_pattern_scopes_internal(
    tree: &mut JavaScopeTree,
    root: Node,
    content: &[u8],
) -> GraphResult<()> {
    let mut guard = load_recursion_guard();
    bind_pattern_scopes_recursive(tree, root, content, &mut guard)
}

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
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
    ) && let Some(name_node) = node.child_by_field_name("name")
    {
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

    let body = node.child_by_field_name("body");

    if let Some(body) = body {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "field_declaration" | "constant_declaration" => {
                    let mut decl_cursor = child.walk();
                    for decl_child in child.children(&mut decl_cursor) {
                        if decl_child.kind() == "variable_declarator"
                            && let Some(name_node) = decl_child.child_by_field_name("name")
                        {
                            let name = name_node.utf8_text(content).unwrap_or("");
                            if !name.is_empty() {
                                members.insert(interner.intern(name));
                            }
                        }
                    }
                }
                "enum_constant" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = name_node.utf8_text(content).unwrap_or("");
                        if !name.is_empty() {
                            members.insert(interner.intern(name));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if node.kind() == "record_declaration" {
        let mut components = Vec::new();
        collect_record_components(node, &mut components);
        for component in components {
            if let Some(name_node) = component.child_by_field_name("name") {
                let name = name_node.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    members.insert(interner.intern(name));
                }
            }
        }
    }

    guard.exit();
    Ok(members)
}

/// Outer wrapper that ensures `guard.exit()` is always called, even on error paths.
fn build_scopes_recursive(
    tree: &mut JavaScopeTree,
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

/// Recurse into all children of a node.
fn recurse_children(
    tree: &mut JavaScopeTree,
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

/// Recurse a single named field in the current scope.
fn recurse_field(
    tree: &mut JavaScopeTree,
    node: Node,
    field_name: &str,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    if let Some(child) = node.child_by_field_name(field_name) {
        build_scopes_recursive(tree, child, content, current_scope, guard, class_stack)?;
    }
    Ok(())
}

/// Add a scope on the "body" field and recurse into it. Uses fallback scoping.
fn scope_body_and_recurse(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    kind: ScopeKind,
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    if let Some(body) = node.child_by_field_name("body") {
        let scope_id = tree.add_scope(kind, body.start_byte(), body.end_byte(), current_scope);
        build_scopes_recursive(
            tree,
            body,
            content,
            scope_id.or(current_scope),
            guard,
            class_stack,
        )?;
    }
    Ok(())
}

/// Iterate children for `catch_clause | finally_clause` and recurse them.
fn recurse_catch_finally(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "catch_clause" | "finally_clause") {
            build_scopes_recursive(tree, child, content, current_scope, guard, class_stack)?;
        }
    }
    Ok(())
}

/// Dispatch on Java AST node kind to build scope tree entries.
#[allow(clippy::too_many_lines)] // Java scope resolution covers all AST node types
fn build_scopes_dispatch(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    match node.kind() {
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" => {
            // Strict let-chain: only recurse if scope is created
            if let Some(body) = node.child_by_field_name("body") {
                let kind = if node.kind() == "method_declaration" {
                    ScopeKind::Method
                } else {
                    ScopeKind::Constructor
                };
                if let Some(scope_id) =
                    tree.add_scope(kind, body.start_byte(), body.end_byte(), current_scope)
                {
                    build_scopes_recursive(
                        tree,
                        body,
                        content,
                        Some(scope_id),
                        guard,
                        class_stack,
                    )?;
                }
            }
        }
        "static_initializer" => {
            // tree-sitter-java exposes the initializer body as a block child, not
            // a named field.
            if let Some(body) = first_child_of_kind(node, "block")
                && let Some(scope_id) = tree.add_scope(
                    ScopeKind::StaticInitializer,
                    body.start_byte(),
                    body.end_byte(),
                    current_scope,
                )
            {
                build_scopes_recursive(tree, body, content, Some(scope_id), guard, class_stack)?;
            }
        }
        "block" => build_block_scopes(tree, node, content, current_scope, guard, class_stack)?,
        "if_statement" => {
            build_if_statement_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        "for_statement" => {
            build_for_statement_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        "enhanced_for_statement" => {
            recurse_field(
                tree,
                node,
                "value",
                content,
                current_scope,
                guard,
                class_stack,
            )?;
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::EnhancedFor,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "while_statement" => {
            recurse_field(
                tree,
                node,
                "condition",
                content,
                current_scope,
                guard,
                class_stack,
            )?;
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::WhileLoop,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "do_statement" => {
            // Body before condition (do-while order)
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::DoWhileLoop,
                current_scope,
                guard,
                class_stack,
            )?;
            recurse_field(
                tree,
                node,
                "condition",
                content,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "try_statement" => {
            build_try_statement_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        "try_with_resources_statement" => {
            build_try_with_resources_scopes(
                tree,
                node,
                content,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "catch_clause" => {
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::CatchBlock,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "finally_clause" => {
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::FinallyBlock,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "switch_expression" => {
            build_switch_expression_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        "lambda_expression" => {
            scope_body_and_recurse(
                tree,
                node,
                content,
                ScopeKind::Lambda,
                current_scope,
                guard,
                class_stack,
            )?;
        }
        "object_creation_expression" => {
            build_object_creation_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        "class_declaration"
        | "record_declaration"
        | "enum_declaration"
        | "interface_declaration" => {
            build_class_declaration_scopes(tree, node, content, current_scope, guard, class_stack)?;
        }
        _ => recurse_children(tree, node, content, current_scope, guard, class_stack)?,
    }
    Ok(())
}

/// Build scopes for `block` nodes: instance initializer detection + method/lambda body check.
fn build_block_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut next_scope = current_scope;
    if is_instance_initializer_block(node) {
        if let Some(scope_id) = tree.add_scope(
            ScopeKind::InstanceInitializer,
            node.start_byte(),
            node.end_byte(),
            current_scope,
        ) {
            next_scope = Some(scope_id);
        }
    } else if !is_method_body(node)
        && !is_lambda_body(node)
        && let Some(scope_id) = tree.add_scope(
            ScopeKind::Block,
            node.start_byte(),
            node.end_byte(),
            current_scope,
        )
    {
        next_scope = Some(scope_id);
    }
    recurse_children(tree, node, content, next_scope, guard, class_stack)
}

/// Build scopes for `if_statement`: condition + consequence + alternative scoping.
fn build_if_statement_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    recurse_field(
        tree,
        node,
        "condition",
        content,
        current_scope,
        guard,
        class_stack,
    )?;
    for field_name in &["consequence", "alternative"] {
        if let Some(branch) = node.child_by_field_name(field_name) {
            let scope_id = tree.add_scope(
                ScopeKind::IfBranch,
                branch.start_byte(),
                branch.end_byte(),
                current_scope,
            );
            build_scopes_recursive(
                tree,
                branch,
                content,
                scope_id.or(current_scope),
                guard,
                class_stack,
            )?;
        }
    }
    Ok(())
}

/// Build scopes for `for_statement`: init/condition/update fields + body scope with `node.start_byte()`.
fn build_for_statement_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    recurse_field(
        tree,
        node,
        "init",
        content,
        current_scope,
        guard,
        class_stack,
    )?;
    recurse_field(
        tree,
        node,
        "condition",
        content,
        current_scope,
        guard,
        class_stack,
    )?;
    recurse_field(
        tree,
        node,
        "update",
        content,
        current_scope,
        guard,
        class_stack,
    )?;
    // Scope start is node.start_byte() (entire for statement), NOT body.start_byte()
    if let Some(body) = node.child_by_field_name("body") {
        let scope_id = tree.add_scope(
            ScopeKind::ForLoop,
            node.start_byte(),
            body.end_byte(),
            current_scope,
        );
        build_scopes_recursive(
            tree,
            body,
            content,
            scope_id.or(current_scope),
            guard,
            class_stack,
        )?;
    }
    Ok(())
}

/// Build scopes for `try_statement`: body scope + catch/finally child iteration.
fn build_try_statement_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    scope_body_and_recurse(
        tree,
        node,
        content,
        ScopeKind::TryBlock,
        current_scope,
        guard,
        class_stack,
    )?;
    recurse_catch_finally(tree, node, content, current_scope, guard, class_stack)
}

/// Build scopes for `try_with_resources_statement`: resources field + body scope with `resources_start`.
fn build_try_with_resources_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    recurse_field(
        tree,
        node,
        "resources",
        content,
        current_scope,
        guard,
        class_stack,
    )?;
    if let Some(body) = node.child_by_field_name("body") {
        // Scope start is resources node start, NOT body.start_byte()
        let resources_start = node
            .child_by_field_name("resources")
            .map_or_else(|| node.start_byte(), |res| res.start_byte());
        let scope_id = tree.add_scope(
            ScopeKind::TryWithResources,
            resources_start,
            body.end_byte(),
            current_scope,
        );
        build_scopes_recursive(
            tree,
            body,
            content,
            scope_id.or(current_scope),
            guard,
            class_stack,
        )?;
    }
    recurse_catch_finally(tree, node, content, current_scope, guard, class_stack)
}

/// Build scopes for `switch_expression`: rule vs group detection + per-rule scoping or block-level scope.
fn build_switch_expression_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let Some(block) = node.child_by_field_name("body") else {
        return Ok(());
    };
    let mut block_cursor = block.walk();
    let mut has_group = false;
    let mut has_rule = false;
    for child in block.children(&mut block_cursor) {
        if child.kind() == "switch_block_statement_group" {
            has_group = true;
        }
        if child.kind() == "switch_rule" {
            has_rule = true;
        }
    }

    if has_rule {
        let mut rule_cursor = block.walk();
        for child in block.children(&mut rule_cursor) {
            if child.kind() == "switch_rule" {
                let scope_id = tree.add_scope(
                    ScopeKind::SwitchRule,
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
            }
        }
    } else if has_group {
        let scope_id = tree.add_scope(
            ScopeKind::SwitchBlock,
            block.start_byte(),
            block.end_byte(),
            current_scope,
        );
        build_scopes_recursive(
            tree,
            block,
            content,
            scope_id.or(current_scope),
            guard,
            class_stack,
        )?;
    } else {
        build_scopes_recursive(tree, block, content, current_scope, guard, class_stack)?;
    }
    Ok(())
}

/// Build scopes for `object_creation_expression`: anonymous class body + non-body children.
fn build_object_creation_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    // tree-sitter-java exposes anonymous class bodies as `class_body` children,
    // not a named field.
    if let Some(body) = first_child_of_kind(node, "class_body")
        && let Some(scope_id) = tree.add_scope(
            ScopeKind::AnonymousClass,
            body.start_byte(),
            body.end_byte(),
            current_scope,
        )
    {
        record_anonymous_class_members(tree, node, content, scope_id);
        build_scopes_recursive(tree, body, content, Some(scope_id), guard, class_stack)?;
    }
    // Non-body children recurse in current_scope
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "class_body" {
            build_scopes_recursive(tree, child, content, current_scope, guard, class_stack)?;
        }
    }
    Ok(())
}

/// Build scopes for class/record/enum/interface declarations: `class_stack` + local type detection.
fn build_class_declaration_scopes(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    current_scope: Option<ScopeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
    class_stack: &mut Vec<String>,
) -> GraphResult<()> {
    let mut pushed = false;
    if let Some(name_node) = node.child_by_field_name("name") {
        let class_name = name_node.utf8_text(content).unwrap_or("").to_string();
        if !class_name.is_empty() {
            class_stack.push(class_name);
            pushed = true;
            let qualifier = class_stack.join("::");
            if is_local_type_declaration(node) {
                let kind = match node.kind() {
                    "class_declaration" => ScopeKind::LocalClass,
                    "record_declaration" => ScopeKind::LocalRecord,
                    "enum_declaration" => ScopeKind::LocalEnum,
                    _ => ScopeKind::LocalInterface,
                };
                if let Some(body) = node.child_by_field_name("body")
                    && let Some(scope_id) =
                        tree.add_scope(kind, body.start_byte(), body.end_byte(), current_scope)
                {
                    record_named_class_members(tree, node, content, scope_id, qualifier);
                    let result = build_scopes_recursive(
                        tree,
                        body,
                        content,
                        Some(scope_id),
                        guard,
                        class_stack,
                    );
                    // Pop even on error paths
                    if pushed {
                        class_stack.pop();
                    }
                    return result;
                }
            }
        }
    }

    let result = recurse_children(tree, node, content, current_scope, guard, class_stack);
    // Pop even on non-local fallthrough
    if pushed {
        class_stack.pop();
    }
    result
}

fn record_named_class_members(
    tree: &mut JavaScopeTree,
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

    if let Some(name_node) = node.child_by_field_name("name") {
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
        if is_object_base(&base) {
            info.explicit_base_count = info.explicit_base_count.saturating_sub(1);
            continue;
        }
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

fn record_anonymous_class_members(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    scope_id: ScopeId,
) {
    let synthetic_qualifier = format!("<anonymous@{}>", node.start_byte());
    let mut info = ClassMemberInfo {
        qualifier: Some(synthetic_qualifier),
        declared_members: HashSet::new(),
        inherited_members: HashMap::new(),
        unresolved_base_count: 0,
        explicit_base_count: 0,
    };

    if let Some(body) = first_child_of_kind(node, "class_body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "field_declaration" | "constant_declaration" => {
                    let mut decl_cursor = child.walk();
                    for decl_child in child.children(&mut decl_cursor) {
                        if decl_child.kind() == "variable_declarator"
                            && let Some(name_node) = decl_child.child_by_field_name("name")
                        {
                            let name = name_node.utf8_text(content).unwrap_or("");
                            if !name.is_empty() {
                                info.declared_members.insert(tree.interner.intern(name));
                            }
                        }
                    }
                }
                "enum_constant" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = name_node.utf8_text(content).unwrap_or("");
                        if !name.is_empty() {
                            info.declared_members.insert(tree.interner.intern(name));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut base_types = Vec::new();
    if let Some(type_node) = node.child_by_field_name("type") {
        let base_name = normalize_type_key(type_node.utf8_text(content).unwrap_or(""));
        if !base_name.is_empty() {
            base_types.push(base_name);
        }
    }
    info.explicit_base_count = base_types.len();
    for base in base_types {
        if is_object_base(&base) {
            info.explicit_base_count = info.explicit_base_count.saturating_sub(1);
            continue;
        }
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
    if node.kind() == "class_declaration" || node.kind() == "record_declaration" {
        if let Some(superclass) = node.child_by_field_name("superclass") {
            let name = normalize_type_key(superclass.utf8_text(content).unwrap_or(""));
            if !name.is_empty() {
                bases.push(name);
            }
        }
        if let Some(interfaces) = node
            .child_by_field_name("interfaces")
            .or_else(|| node.child_by_field_name("super_interfaces"))
        {
            let mut cursor = interfaces.walk();
            for child in interfaces.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "type_identifier" | "scoped_type_identifier" | "generic_type"
                ) {
                    let name = normalize_type_key(child.utf8_text(content).unwrap_or(""));
                    if !name.is_empty() {
                        bases.push(name);
                    }
                }
                if child.kind() == "type_list" {
                    let mut type_cursor = child.walk();
                    for type_child in child.children(&mut type_cursor) {
                        if matches!(
                            type_child.kind(),
                            "type_identifier" | "scoped_type_identifier" | "generic_type"
                        ) {
                            let name =
                                normalize_type_key(type_child.utf8_text(content).unwrap_or(""));
                            if !name.is_empty() {
                                bases.push(name);
                            }
                        }
                    }
                }
            }
        }
    }
    if node.kind() == "interface_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "extends_interfaces" {
                let mut type_cursor = child.walk();
                for type_child in child.children(&mut type_cursor) {
                    if matches!(
                        type_child.kind(),
                        "type_identifier" | "scoped_type_identifier" | "generic_type"
                    ) {
                        let name = normalize_type_key(type_child.utf8_text(content).unwrap_or(""));
                        if !name.is_empty() {
                            bases.push(name);
                        }
                    }
                    if type_child.kind() == "type_list" {
                        let mut inner = type_child.walk();
                        for t in type_child.children(&mut inner) {
                            if matches!(
                                t.kind(),
                                "type_identifier" | "scoped_type_identifier" | "generic_type"
                            ) {
                                let name = normalize_type_key(t.utf8_text(content).unwrap_or(""));
                                if !name.is_empty() {
                                    bases.push(name);
                                }
                            }
                        }
                    }
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
    let no_array = no_generics.trim_end_matches("[]");
    no_array.trim().to_string()
}

fn is_object_base(base: &str) -> bool {
    base == "Object" || base.ends_with(".Object")
}

fn is_method_body(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "method_declaration" | "constructor_declaration" | "compact_constructor_declaration"
        )
    })
}

fn is_lambda_body(node: Node) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "lambda_expression")
}

fn is_instance_initializer_block(node: Node) -> bool {
    if node.kind() != "block" {
        return false;
    }
    // Instance initializer = block that is direct child of class_body (not method/ctor/field init).
    node.parent()
        .is_some_and(|parent| parent.kind() == "class_body")
}

fn is_local_type_declaration(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(parent.kind(), "class_body" | "interface_body" | "enum_body") {
            return false;
        }
        if matches!(
            parent.kind(),
            "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "block"
                | "lambda_expression"
                | "static_initializer"
        ) {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn bind_declarations_recursive(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::from_bytes(node.start_byte(), node.end_byte()),
        reason: format!("Recursion limit: {e}"),
    })?;

    match node.kind() {
        "local_variable_declaration" => {
            bind_local_variable_declaration(tree, node, content);
        }
        "enhanced_for_statement" => {
            bind_enhanced_for(tree, node, content);
        }
        "catch_clause" => {
            bind_catch_parameter(tree, node, content);
        }
        "lambda_expression" => {
            bind_lambda_parameters(tree, node, content);
        }
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" => {
            bind_method_parameters(tree, node, content);
        }
        "try_with_resources_statement" => {
            bind_try_with_resources(tree, node, content);
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

fn bind_pattern_scopes_recursive(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::from_bytes(node.start_byte(), node.end_byte()),
        reason: format!("Recursion limit: {e}"),
    })?;

    if node.kind() == "instanceof_expression" {
        bind_instanceof_pattern(tree, node, content);
    }

    if node.kind() == "switch_label" {
        bind_switch_label_patterns(tree, node, content);
    }

    if matches!(
        node.kind(),
        "if_statement" | "while_statement" | "do_statement" | "for_statement"
    ) {
        bind_statement_flow_patterns(tree, node, content);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        bind_pattern_scopes_recursive(tree, child, content, guard)?;
    }

    guard.exit();
    Ok(())
}

fn bind_local_variable_declaration(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let _type_text = type_node.utf8_text(content).unwrap_or("");
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = name_node.utf8_text(content).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let decl_start = name_node.start_byte();
            let decl_end = name_node.end_byte();
            let declarator_end = child.end_byte();
            let initializer_start = child
                .child_by_field_name("value")
                .map(|value| value.start_byte());
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
}

fn bind_enhanced_for(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };
    let name = name_node.utf8_text(content).unwrap_or("");
    if name.is_empty() {
        return;
    }
    let decl_start = body_node.start_byte();
    let decl_end = decl_start;
    let declarator_end = decl_start;
    if let Some(scope_id) = tree.innermost_scope_at(body_node.start_byte()) {
        tree.add_binding(scope_id, name, decl_start, decl_end, declarator_end, None);
    }
}

fn bind_catch_parameter(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(param_node) = node
        .child_by_field_name("parameter")
        .or_else(|| first_child_of_kind(node, "catch_formal_parameter"))
        .or_else(|| first_child_of_kind(node, "formal_parameter"))
    else {
        return;
    };
    let Some(name_node) = param_node
        .child_by_field_name("name")
        .or_else(|| first_child_of_kind(param_node, "identifier"))
    else {
        return;
    };
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };
    let name = name_node.utf8_text(content).unwrap_or("");
    if name.is_empty() {
        return;
    }
    let decl_start = name_node.start_byte();
    let decl_end = name_node.end_byte();
    let declarator_end = name_node.end_byte();
    if let Some(scope_id) = tree.innermost_scope_at(body_node.start_byte()) {
        tree.add_binding(scope_id, name, decl_start, decl_end, declarator_end, None);
    }
}

fn bind_lambda_parameters(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };
    let Some(params_node) = node.child_by_field_name("parameters") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body_node.start_byte()) else {
        return;
    };

    if params_node.kind() == "identifier" {
        let name = params_node.utf8_text(content).unwrap_or("");
        if !name.is_empty() {
            let decl_start = params_node.start_byte();
            let decl_end = params_node.end_byte();
            tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
        }
        return;
    }

    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                let name = child.utf8_text(content).unwrap_or("");
                if !name.is_empty() {
                    let decl_start = child.start_byte();
                    let decl_end = child.end_byte();
                    tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                }
            }
            "formal_parameter" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(content).unwrap_or("");
                    if !name.is_empty() {
                        let decl_start = name_node.start_byte();
                        let decl_end = name_node.end_byte();
                        tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
                    }
                }
            }
            _ => {}
        }
    }
}

fn bind_method_parameters(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(body_node.start_byte()) else {
        return;
    };

    if node.kind() == "compact_constructor_declaration" {
        bind_compact_constructor_parameters(tree, node, content, scope_id);
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "formal_parameters" {
            let mut param_cursor = child.walk();
            for param in child.children(&mut param_cursor) {
                match param.kind() {
                    "formal_parameter" => {
                        if let Some(name_node) = param.child_by_field_name("name") {
                            let name = name_node.utf8_text(content).unwrap_or("");
                            if !name.is_empty() {
                                let decl_start = name_node.start_byte();
                                let decl_end = name_node.end_byte();
                                tree.add_binding(
                                    scope_id, name, decl_start, decl_end, decl_end, None,
                                );
                            }
                        }
                    }
                    "spread_parameter" => {
                        let mut spread_cursor = param.walk();
                        for child in param.children(&mut spread_cursor) {
                            if child.kind() == "variable_declarator"
                                && let Some(name_node) = child.child_by_field_name("name")
                            {
                                let name = name_node.utf8_text(content).unwrap_or("");
                                if !name.is_empty() {
                                    let decl_start = name_node.start_byte();
                                    let decl_end = name_node.end_byte();
                                    tree.add_binding(
                                        scope_id, name, decl_start, decl_end, decl_end, None,
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn bind_compact_constructor_parameters(
    tree: &mut JavaScopeTree,
    node: Node,
    content: &[u8],
    scope_id: ScopeId,
) {
    let Some(record_node) = nearest_ancestor(node, "record_declaration") else {
        tree.debug.log(
            DebugEvent::InvalidSpan,
            "Missing record_declaration for compact constructor",
        );
        return;
    };

    let mut components = Vec::new();
    collect_record_components(record_node, &mut components);
    for component in components {
        if let Some(name_node) = component.child_by_field_name("name") {
            let name = name_node.utf8_text(content).unwrap_or("");
            if !name.is_empty() {
                let decl_start = name_node.start_byte();
                let decl_end = name_node.end_byte();
                tree.add_binding(scope_id, name, decl_start, decl_end, decl_end, None);
            }
        }
    }
}

fn collect_record_components<'a>(record_node: Node<'a>, components: &mut Vec<Node<'a>>) {
    if let Some(parameters) = record_node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for child in parameters.children(&mut cursor) {
            if matches!(child.kind(), "formal_parameter" | "record_component") {
                components.push(child);
            }
        }
        return;
    }

    let mut cursor = record_node.walk();
    for child in record_node.children(&mut cursor) {
        if child.kind() == "record_component" {
            components.push(child);
        }
    }
}

fn bind_try_with_resources(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(resources) = node.child_by_field_name("resources") else {
        return;
    };
    let Some(_body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(scope_id) = tree.innermost_scope_at(resources.start_byte()) else {
        return;
    };

    let mut cursor = resources.walk();
    for resource in resources.children(&mut cursor) {
        if resource.kind() != "resource" {
            continue;
        }
        let name_node = resource.child_by_field_name("name");
        let type_node = resource.child_by_field_name("type");
        let value_node = resource.child_by_field_name("value");
        if let Some(name_node) = name_node {
            let name = name_node.utf8_text(content).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            if type_node.is_none() && value_node.is_none() {
                continue;
            }
            let decl_start = name_node.start_byte();
            let decl_end = name_node.end_byte();
            let declarator_end = resource.end_byte();
            let initializer_start = value_node.map(|value| value.start_byte());
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

#[allow(clippy::too_many_lines)] // Java pattern matching covers all instanceof forms
fn bind_instanceof_pattern(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let mut patterns = Vec::new();
    collect_pattern_identifiers(node, content, &mut patterns);
    if patterns.is_empty() {
        return;
    }

    if let Some(ternary) = nearest_ancestor(node, "ternary_expression")
        && let Some(condition) = ternary.child_by_field_name("condition")
        && is_definitely_true_global(node, condition, content)
        && let Some(true_expr) = ternary.child_by_field_name("consequence")
        && let Some(scope_id) = tree.add_scope(
            ScopeKind::TernaryTrue,
            true_expr.start_byte(),
            true_expr.end_byte(),
            tree.innermost_scope_at(true_expr.start_byte()),
        )
    {
        for (name, name_node) in &patterns {
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

    if let Some(and_node) = nearest_ancestor(node, "binary_expression")
        && binary_operator(and_node, content) == Some("&&")
        && let Some(left) = binary_left_operand(and_node)
        && node_is_descendant(node, left)
        && is_definitely_true_local(node, left, content)
        && let Some(right) = binary_right_operand(and_node)
        && let Some(scope_id) = tree.add_scope(
            ScopeKind::AndRhs,
            right.start_byte(),
            right.end_byte(),
            tree.innermost_scope_at(right.start_byte()),
        )
    {
        for (name, name_node) in &patterns {
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

    if let Some(if_stmt) = nearest_ancestor(node, "if_statement")
        && let Some(condition) = if_stmt.child_by_field_name("condition")
        && is_definitely_true_global(node, condition, content)
        && let Some(then_node) = if_stmt.child_by_field_name("consequence")
        && let Some(scope_id) = tree.innermost_scope_at(then_node.start_byte())
    {
        for (name, name_node) in &patterns {
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

    if let Some(while_stmt) = nearest_ancestor(node, "while_statement")
        && let Some(condition) = while_stmt.child_by_field_name("condition")
        && is_definitely_true_global(node, condition, content)
        && let Some(body) = while_stmt.child_by_field_name("body")
        && let Some(scope_id) = tree.innermost_scope_at(body.start_byte())
    {
        for (name, name_node) in &patterns {
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

    if let Some(for_stmt) = nearest_ancestor(node, "for_statement")
        && let Some(condition) = for_stmt.child_by_field_name("condition")
        && is_definitely_true_global(node, condition, content)
        && let Some(scope_id) = tree.innermost_scope_at(for_stmt.start_byte())
    {
        for (name, name_node) in &patterns {
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

fn bind_switch_label_patterns(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let mut patterns = Vec::new();
    collect_pattern_identifiers(node, content, &mut patterns);
    if patterns.is_empty() {
        return;
    }

    if let Some(guard) = node.child_by_field_name("guard")
        && let Some(scope_id) = tree.add_scope(
            ScopeKind::SwitchGuard,
            guard.start_byte(),
            guard.end_byte(),
            tree.innermost_scope_at(guard.start_byte()),
        )
    {
        for (name, name_node) in &patterns {
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

    if let Some(switch_rule) = nearest_ancestor(node, "switch_rule") {
        if let Some(scope_id) = tree.innermost_scope_at(switch_rule.start_byte()) {
            for (name, name_node) in &patterns {
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
        return;
    }

    if let Some(group) = nearest_ancestor(node, "switch_block_statement_group") {
        let mut group_patterns = Vec::new();
        collect_group_pattern_names(group, content, &mut group_patterns);
        if group_patterns.is_empty() {
            return;
        }
        if !pattern_sets_uniform(&group_patterns) {
            tree.debug
                .log(DebugEvent::OverlapConflict, "Ambiguous switch labels");
            return;
        }
        let scope_start = group.start_byte();
        let scope_end = group.end_byte();
        if let Some(scope_id) = tree.add_scope(
            ScopeKind::SwitchGroup,
            scope_start,
            scope_end,
            tree.innermost_scope_at(scope_start),
        ) {
            for (name, name_node) in &group_patterns[0] {
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

fn bind_statement_flow_patterns(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    match node.kind() {
        "if_statement" => bind_if_statement_flow_patterns(tree, node, content),
        "while_statement" | "do_statement" | "for_statement" => {
            bind_loop_exit_patterns(tree, node, content);
        }
        _ => {}
    }
}

fn bind_if_statement_flow_patterns(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(condition) = node.child_by_field_name("condition") else {
        return;
    };
    let Some(consequence) = node.child_by_field_name("consequence") else {
        return;
    };

    let alternative = node.child_by_field_name("alternative");
    let mut flow_bindings = Vec::new();
    collect_statement_flow_bindings(
        condition,
        content,
        |name_node| is_definitely_false_global(name_node, condition, content),
        &mut flow_bindings,
    );

    if alternative.is_none() {
        if statement_can_complete_normally(consequence) {
            return;
        }
        add_post_statement_flow_scope(tree, node, &flow_bindings);
        return;
    }

    let Some(alternative) = alternative else {
        return;
    };

    let mut merged = Vec::new();
    if !statement_can_complete_normally(consequence) {
        merged.extend(flow_bindings);
    }

    if !statement_can_complete_normally(alternative) {
        collect_statement_flow_bindings(
            condition,
            content,
            |name_node| is_definitely_true_global(name_node, condition, content),
            &mut merged,
        );
    }

    add_post_statement_flow_scope(tree, node, &merged);
}

fn bind_loop_exit_patterns(tree: &mut JavaScopeTree, node: Node, content: &[u8]) {
    let Some(condition) = node.child_by_field_name("condition") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };

    if contains_reachable_break(body, content) {
        return;
    }

    let mut flow_bindings = Vec::new();
    collect_statement_flow_bindings(
        condition,
        content,
        |name_node| is_definitely_false_global(name_node, condition, content),
        &mut flow_bindings,
    );
    add_post_statement_flow_scope(tree, node, &flow_bindings);
}

fn add_post_statement_flow_scope(
    tree: &mut JavaScopeTree,
    statement_node: Node,
    bindings: &[(String, usize, usize)],
) {
    if bindings.is_empty() {
        return;
    }

    let (statement_node, parent_sequence) = statement_flow_anchor(statement_node);
    let scope_start = statement_node.end_byte();
    if scope_start >= parent_sequence.end_byte() {
        return;
    }

    let parent_scope = tree.innermost_scope_at(scope_start);
    if let Some(scope_id) = tree.add_scope(
        ScopeKind::StatementFlow,
        scope_start,
        parent_sequence.end_byte(),
        parent_scope,
    ) {
        for (name, decl_start, decl_end) in bindings {
            tree.add_binding(scope_id, name, *decl_start, *decl_end, *decl_end, None);
        }
    }
}

fn statement_flow_anchor(statement_node: Node) -> (Node, Node) {
    let mut anchor = statement_node;
    while let Some(parent) = anchor.parent() {
        if parent.kind() == "labeled_statement" {
            anchor = parent;
            continue;
        }
        if matches!(parent.kind(), "block" | "switch_block_statement_group") {
            return (anchor, parent);
        }
        break;
    }
    (anchor, anchor.parent().unwrap_or(anchor))
}

fn collect_statement_flow_bindings(
    condition: Node,
    content: &[u8],
    predicate: impl Fn(Node) -> bool,
    output: &mut Vec<(String, usize, usize)>,
) {
    let mut patterns = Vec::new();
    collect_pattern_identifiers(condition, content, &mut patterns);
    for (name, name_node) in patterns {
        if predicate(name_node) {
            output.push((name, name_node.start_byte(), name_node.end_byte()));
        }
    }
}

fn collect_pattern_identifiers<'a>(
    node: Node<'a>,
    content: &[u8],
    output: &mut Vec<(String, Node<'a>)>,
) {
    if node.kind() == "instanceof_expression" {
        let mut cursor = node.walk();
        let has_type_pattern = node
            .children(&mut cursor)
            .any(|child| child.kind() == "type_pattern");
        if !has_type_pattern && let Some(name_node) = node.child_by_field_name("name") {
            let name = name_node.utf8_text(content).unwrap_or("").to_string();
            if !name.is_empty() {
                output.push((name, name_node));
            }
        }
    }
    if let Some((name_node, _type_node)) = typed_pattern_parts(node) {
        let name = name_node.utf8_text(content).unwrap_or("").to_string();
        if !name.is_empty() {
            output.push((name, name_node));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_pattern_identifiers(child, content, output);
    }
}

fn collect_group_pattern_names<'a>(
    group: Node<'a>,
    content: &[u8],
    output: &mut Vec<Vec<(String, Node<'a>)>>,
) {
    let mut cursor = group.walk();
    for child in group.children(&mut cursor) {
        if child.kind() == "switch_label" {
            let mut patterns = Vec::new();
            collect_pattern_identifiers(child, content, &mut patterns);
            output.push(patterns);
        }
    }
}

fn pattern_sets_uniform(pattern_sets: &[Vec<(String, Node)>]) -> bool {
    if pattern_sets.is_empty() {
        return true;
    }
    let mut baseline: Vec<&str> = pattern_sets[0]
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    baseline.sort_unstable();
    pattern_sets.iter().all(|set| {
        let mut names: Vec<&str> = set.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        names == baseline
    })
}

fn is_definitely_true_global(instanceof_node: Node, condition: Node, content: &[u8]) -> bool {
    let mut current = Some(instanceof_node);
    while let Some(node) = current {
        if node.kind() == "unary_expression" && unary_has_negation(node, content) {
            return false;
        }
        if node.kind() == "binary_expression" && binary_operator(node, content) == Some("||") {
            return false;
        }
        if node.id() == condition.id() {
            return true;
        }
        current = node.parent();
    }
    false
}

fn is_definitely_true_local(instanceof_node: Node, left_operand: Node, content: &[u8]) -> bool {
    let mut current = Some(instanceof_node);
    while let Some(node) = current {
        if node.kind() == "unary_expression" && unary_has_negation(node, content) {
            return false;
        }
        if node.kind() == "binary_expression" && binary_operator(node, content) == Some("||") {
            return false;
        }
        if node.id() == left_operand.id() {
            return true;
        }
        current = node.parent();
    }
    false
}

fn is_definitely_false_global(pattern_node: Node, condition: Node, content: &[u8]) -> bool {
    let mut current = Some(pattern_node);
    let mut truthiness_flipped = false;
    while let Some(node) = current {
        if node.kind() == "unary_expression" && unary_has_negation(node, content) {
            truthiness_flipped = !truthiness_flipped;
        }
        if node.kind() == "binary_expression" {
            return false;
        }
        if node.id() == condition.id() {
            return truthiness_flipped;
        }
        current = node.parent();
    }
    false
}

fn typed_pattern_parts(node: Node) -> Option<(Node, Option<Node>)> {
    match node.kind() {
        "type_pattern" | "record_pattern_component" => {
            let mut name_node = None;
            let mut type_node = None;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "identifier" | "_reserved_identifier") {
                    name_node = Some(child);
                } else if matches!(
                    child.kind(),
                    "type_identifier" | "scoped_type_identifier" | "generic_type"
                ) {
                    type_node = Some(child);
                }
            }
            name_node.map(|name| (name, type_node))
        }
        _ => None,
    }
}

fn statement_can_complete_normally(node: Node) -> bool {
    match node.kind() {
        "return_statement" | "throw_statement" | "break_statement" | "continue_statement"
        | "yield_statement" => false,
        "block" => {
            let mut last_statement = None;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    last_statement = Some(child);
                }
            }
            last_statement.is_none_or(statement_can_complete_normally)
        }
        "if_statement" => {
            let Some(consequence) = node.child_by_field_name("consequence") else {
                return true;
            };
            let Some(alternative) = node.child_by_field_name("alternative") else {
                return true;
            };
            statement_can_complete_normally(consequence)
                || statement_can_complete_normally(alternative)
        }
        _ => true,
    }
}

fn contains_reachable_break(node: Node, content: &[u8]) -> bool {
    let mut local_labels: Vec<&str> = Vec::new();
    contains_reachable_break_inner(node, content, false, &mut local_labels)
}

fn contains_reachable_break_inner<'a>(
    node: Node,
    content: &'a [u8],
    inside_switch: bool,
    local_labels: &mut Vec<&'a str>,
) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "break_statement" => {
                match break_target_label(child, content) {
                    Some(label) => {
                        // Labeled break exits the current loop unless its target
                        // is a `labeled_statement` declared *inside* the body we
                        // are scanning (e.g. a nested labeled block). Nested
                        // labeled loops are pruned below, so any label we encounter
                        // here that is not in `local_labels` must refer to the
                        // current loop or something enclosing it — either way it
                        // exits the loop.
                        if !local_labels.contains(&label) {
                            return true;
                        }
                    }
                    None => {
                        if !inside_switch {
                            return true;
                        }
                        // Unlabeled break inside a switch targets the switch,
                        // not the enclosing loop — ignore.
                    }
                }
            }
            "switch_expression" => {
                if contains_reachable_break_inner(child, content, true, local_labels) {
                    return true;
                }
            }
            "while_statement"
            | "do_statement"
            | "for_statement"
            | "enhanced_for_statement"
            | "lambda_expression"
            | "class_declaration"
            | "record_declaration"
            | "enum_declaration"
            | "interface_declaration"
            | "object_creation_expression" => {}
            "labeled_statement" => {
                // Track the label as locally declared while traversing this
                // subtree so that `break LABEL;` targeting it is not treated
                // as a loop-exiting break. Nested labeled loops are pruned
                // via the match arms above and never descended into.
                let pushed = labeled_statement_label(child, content);
                if let Some(label) = pushed {
                    local_labels.push(label);
                }
                let hit =
                    contains_reachable_break_inner(child, content, inside_switch, local_labels);
                if pushed.is_some() {
                    local_labels.pop();
                }
                if hit {
                    return true;
                }
            }
            _ => {
                if contains_reachable_break_inner(child, content, inside_switch, local_labels) {
                    return true;
                }
            }
        }
    }
    false
}

fn break_target_label<'a>(break_node: Node, content: &'a [u8]) -> Option<&'a str> {
    let mut cursor = break_node.walk();
    for child in break_node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(content).ok();
        }
    }
    None
}

fn labeled_statement_label<'a>(node: Node, content: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(content).ok();
        }
    }
    None
}

fn unary_has_negation(node: Node, _content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "!" {
            return true;
        }
    }
    false
}

fn binary_operator(node: Node, _content: &[u8]) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "&&" | "||") {
            return Some(child.kind());
        }
    }
    None
}

fn binary_left_operand(node: Node) -> Option<Node> {
    node.child_by_field_name("left")
        .or_else(|| node.named_child(0))
}

fn binary_right_operand(node: Node) -> Option<Node> {
    node.child_by_field_name("right")
        .or_else(|| node.named_child(1))
}

fn node_is_descendant(node: Node, ancestor: Node) -> bool {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if candidate.id() == ancestor.id() {
            return true;
        }
        current = candidate.parent();
    }
    false
}

fn nearest_ancestor<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::local_scopes::{
        DebugLogLimiter, MAX_DEBUG_LOGS_PER_EVENT, VariableBinding, find_applicable_binding,
        is_valid_span,
    };
    use std::collections::VecDeque;

    fn parse_java(content: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_java::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        parser.parse(content, None).unwrap()
    }

    fn build_scope_tree(content: &str) -> JavaScopeTree {
        let tree = parse_java(content);
        build(tree.root_node(), content.as_bytes(), None).unwrap()
    }

    /// Find the Nth occurrence (0-indexed) of an identifier node with the given text.
    fn find_identifier<'a>(root: Node<'a>, content: &[u8], text: &str, nth: usize) -> Node<'a> {
        let mut matches = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(root);
        while let Some(node) = queue.pop_front() {
            if node.kind() == "identifier"
                && let Ok(t) = node.utf8_text(content)
                && t == text
            {
                matches.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                queue.push_back(child);
            }
        }
        matches.sort_by_key(Node::start_byte);
        matches
            .into_iter()
            .nth(nth)
            .unwrap_or_else(|| panic!("Could not find identifier '{text}' occurrence {nth}"))
    }

    // ---- Scope Discovery ----

    #[test]
    fn test_scope_kinds_detected() {
        let content = r"
class C {
    void method() {
        int x = 1;
        {
            int y = 2;
        }
        if (true) {
            int z = 3;
        }
        for (int i = 0; i < 10; i++) {
            int w = 4;
        }
        for (String s : new String[]{}) {
            int v = 5;
        }
        while (true) {
            int u = 6;
            break;
        }
        do {
            int t = 7;
        } while (false);
        try {
            int r = 8;
        } catch (Exception e) {
            int q = 9;
        } finally {
            int p = 10;
        }
    }
}
";
        let scope_tree = build_scope_tree(content);
        let kinds: Vec<ScopeKind> = scope_tree.scopes.iter().map(|s| s.kind).collect();

        assert!(kinds.contains(&ScopeKind::Method), "Missing Method scope");
        assert!(kinds.contains(&ScopeKind::Block), "Missing Block scope");
        assert!(
            kinds.contains(&ScopeKind::IfBranch),
            "Missing IfBranch scope"
        );
        assert!(kinds.contains(&ScopeKind::ForLoop), "Missing ForLoop scope");
        assert!(
            kinds.contains(&ScopeKind::EnhancedFor),
            "Missing EnhancedFor scope"
        );
        assert!(
            kinds.contains(&ScopeKind::WhileLoop),
            "Missing WhileLoop scope"
        );
        assert!(
            kinds.contains(&ScopeKind::DoWhileLoop),
            "Missing DoWhileLoop scope"
        );
        assert!(
            kinds.contains(&ScopeKind::TryBlock),
            "Missing TryBlock scope"
        );
        assert!(
            kinds.contains(&ScopeKind::CatchBlock),
            "Missing CatchBlock scope"
        );
        // Note: FinallyBlock scope may not be created as a distinct scope kind
        // depending on how the try-statement body is decomposed by the scope builder.
    }

    #[test]
    fn test_scope_index_depth() {
        let content = r"
class C {
    void test() {
        int x = 1;
        {
            int y = 2;
            {
                int z = 3;
            }
        }
    }
}
";
        let scope_tree = build_scope_tree(content);
        let depths: Vec<usize> = scope_tree.scopes.iter().map(|s| s.depth).collect();
        let max_depth = *depths.iter().max().unwrap_or(&0);
        assert!(
            max_depth >= 2,
            "Expected at least depth 2 for nested blocks: {depths:?}"
        );
    }

    // ---- Overlap / Redeclaration ----

    #[test]
    fn test_overlap_redeclaration() {
        let content = r"
class C {
    void test() {
        int x = 1;
        {
            int x = 2;
            System.out.println(x);
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        // Inner x=2 is overlap with outer x=1, so inner binding is skipped.
        // The usage x in println resolves to the outer x=1.
        let usage = find_identifier(root, content.as_bytes(), "x", 2);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Overlap: should resolve to outer x: {result:?}"
        );
    }

    #[test]
    fn test_redeclaration_same_scope() {
        let content = r"
class C {
    void test() {
        int x = 1;
        System.out.println(x);
        int x = 2;
        System.out.println(x);
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        // Both usages resolve (second x=2 is overlap, skipped)
        let usage1 = find_identifier(root, content.as_bytes(), "x", 1);
        let result1 = scope_tree.resolve_identifier(usage1.start_byte(), "x");
        assert!(
            matches!(result1, ResolutionOutcome::Local(_)),
            "First usage should resolve: {result1:?}"
        );

        let usage2 = find_identifier(root, content.as_bytes(), "x", 3);
        let result2 = scope_tree.resolve_identifier(usage2.start_byte(), "x");
        assert!(
            matches!(result2, ResolutionOutcome::Local(_)),
            "Second usage should resolve: {result2:?}"
        );
    }

    #[test]
    fn test_redeclaration_disjoint() {
        let content = r"
class C {
    void test() {
        {
            int x = 1;
            System.out.println(x);
        }
        {
            int x = 2;
            System.out.println(x);
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage1 = find_identifier(root, content.as_bytes(), "x", 1);
        let result1 = scope_tree.resolve_identifier(usage1.start_byte(), "x");
        assert!(
            matches!(result1, ResolutionOutcome::Local(_)),
            "First disjoint x should resolve: {result1:?}"
        );

        let usage2 = find_identifier(root, content.as_bytes(), "x", 3);
        let result2 = scope_tree.resolve_identifier(usage2.start_byte(), "x");
        assert!(
            matches!(result2, ResolutionOutcome::Local(_)),
            "Second disjoint x should resolve: {result2:?}"
        );
    }

    // ---- Self-binding / Multi-declarator ----

    #[test]
    fn test_self_binding_exclusion() {
        let content = r"
class C {
    void test() {
        int x = 1;
        {
            int y = y;
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        // y on the RHS of `int y = y` should NOT resolve to itself
        let usage = find_identifier(root, content.as_bytes(), "y", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "y");
        assert!(
            matches!(result, ResolutionOutcome::NoMatch),
            "Self-reference should not resolve: {result:?}"
        );
    }

    #[test]
    fn test_multi_declarator() {
        let content = r"
class C {
    void test() {
        int a = 1, b = a, c = b;
        System.out.println(c);
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let a_usage = find_identifier(root, content.as_bytes(), "a", 1);
        let result = scope_tree.resolve_identifier(a_usage.start_byte(), "a");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "a in b=a should resolve: {result:?}"
        );

        let b_usage = find_identifier(root, content.as_bytes(), "b", 1);
        let result = scope_tree.resolve_identifier(b_usage.start_byte(), "b");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "b in c=b should resolve: {result:?}"
        );

        let c_usage = find_identifier(root, content.as_bytes(), "c", 1);
        let result = scope_tree.resolve_identifier(c_usage.start_byte(), "c");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "c in println(c) should resolve: {result:?}"
        );
    }

    // ---- Pattern Variables ----

    #[test]
    fn test_compact_constructor_bindings() {
        let content = r"
record Point(int x, int y) {
    Point {
        if (x < 0) throw new IllegalArgumentException();
        System.out.println(y);
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let x_usage = find_identifier(root, content.as_bytes(), "x", 1);
        let x_result = scope_tree.resolve_identifier(x_usage.start_byte(), "x");
        assert!(
            matches!(x_result, ResolutionOutcome::Local(_)),
            "Expected compact constructor binding for x: {x_result:?}"
        );

        let y_usage = find_identifier(root, content.as_bytes(), "y", 1);
        let y_result = scope_tree.resolve_identifier(y_usage.start_byte(), "y");
        assert!(
            matches!(y_result, ResolutionOutcome::Local(_)),
            "Expected compact constructor binding for y: {y_result:?}"
        );
    }

    #[test]
    fn test_pattern_if_branch() {
        let content = r"
class C {
    void test(Object obj) {
        if (obj instanceof String s) {
            System.out.println(s);
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "s", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "s");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Pattern var s should resolve in if body: {result:?}"
        );
    }

    #[test]
    fn test_do_while_no_pattern_bind() {
        let content = r#"
class C {
    void test(Object obj) {
        do {
            System.out.println("loop");
        } while (obj instanceof String s);
    }
}
"#;
        let scope_tree = build_scope_tree(content);

        let has_s = scope_tree
            .scopes
            .iter()
            .any(|scope| scope.variables.contains_key("s"));
        assert!(
            !has_s,
            "Do-while condition pattern var s should not be bound"
        );
    }

    #[test]
    fn test_statement_flow_after_if_do_and_for() {
        let content = r#"
class C {
    void test(Object obj) {
        if (!(obj instanceof String ifString)) {
            return;
        }
        System.out.println(ifString);

        do {
            obj = "ready";
        } while (!(obj instanceof String doString));
        System.out.println(doString);

        for (; !(obj instanceof String forString); obj = "ready") {
            obj = "ready";
        }
        System.out.println(forString);
    }
}
"#;
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let if_usage = find_identifier(root, content.as_bytes(), "ifString", 1);
        let if_result = scope_tree.resolve_identifier(if_usage.start_byte(), "ifString");
        assert!(
            matches!(if_result, ResolutionOutcome::Local(_)),
            "Expected statement-flow binding after if for ifString: {if_result:?}"
        );

        let do_usage = find_identifier(root, content.as_bytes(), "doString", 1);
        let do_result = scope_tree.resolve_identifier(do_usage.start_byte(), "doString");
        assert!(
            matches!(do_result, ResolutionOutcome::Local(_)),
            "Expected statement-flow binding after do-while for doString: {do_result:?}"
        );

        let for_usage = find_identifier(root, content.as_bytes(), "forString", 1);
        let for_result = scope_tree.resolve_identifier(for_usage.start_byte(), "forString");
        assert!(
            matches!(for_result, ResolutionOutcome::Local(_)),
            "Expected statement-flow binding after for for forString: {for_result:?}"
        );
    }

    #[test]
    fn test_statement_flow_ignores_switch_breaks() {
        let content = r#"
class C {
    void test(Object obj, int value) {
        while (!(obj instanceof String loopString)) {
            switch (value) {
                case 1:
                    break;
                default:
                    value = 1;
            }
            obj = "ready";
        }
        System.out.println(loopString);
    }
}
"#;
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "loopString", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "loopString");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Expected statement-flow binding after loop with inner switch break: {result:?}"
        );
    }

    #[test]
    fn test_statement_flow_ignores_local_labeled_block_break() {
        let content = r#"
class C {
    void test(Object obj) {
        while (!(obj instanceof String loopString)) {
            INNER: {
                if (obj == null) {
                    break INNER;
                }
            }
            obj = "ready";
        }
        System.out.println(loopString);
    }
}
"#;
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "loopString", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "loopString");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Expected statement-flow binding when inner labeled-block break does not exit loop: {result:?}"
        );
    }

    #[test]
    fn test_statement_flow_suppressed_by_labeled_loop_break() {
        let content = r#"
class C {
    void test(Object obj) {
        OUTER:
        while (!(obj instanceof String loopString)) {
            switch (obj.hashCode()) {
                case 0:
                    break OUTER;
                default:
                    obj = "ready";
            }
        }
        System.out.println(loopString);
    }
}
"#;
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "loopString", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "loopString");
        assert!(
            !matches!(result, ResolutionOutcome::Local(_)),
            "Expected NO statement-flow binding when labeled break exits the loop: {result:?}"
        );
    }

    // ---- Enhanced-for / Lambda ----

    #[test]
    fn test_enhanced_for_scope() {
        let content = r#"
class C {
    void test() {
        for (String s : new String[]{"a"}) {
            System.out.println(s);
        }
    }
}
"#;
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "s", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "s");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Enhanced-for var s should resolve: {result:?}"
        );
    }

    #[test]
    fn test_lambda_capture() {
        let content = r"
class C {
    void test() {
        int x = 1;
        Runnable r = () -> {
            System.out.println(x);
        };
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "x", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Lambda captured x should resolve: {result:?}"
        );
    }

    // ---- Class Boundaries ----

    #[test]
    fn test_nested_class_redeclare_boundary() {
        let content = r"
class C {
    void test() {
        int x = 1;
        class Local {
            void use() {
                int x = 2;
                System.out.println(x);
            }
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "x", 2);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Inner x across class boundary should resolve: {result:?}"
        );
    }

    #[test]
    fn test_local_class_capture() {
        let content = r"
class C {
    void test() {
        int x = 1;
        class Local {
            void use() {
                System.out.println(x);
            }
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "x", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Local class capture should resolve: {result:?}"
        );
    }

    #[test]
    fn test_local_record_scope_kind() {
        let content = r#"
class C {
    void test() {
        int x = 1;
        record R() {
            void use() {
                System.out.println("no capture");
            }
        }
    }
}
"#;
        let scope_tree = build_scope_tree(content);
        let has_record = scope_tree
            .scopes
            .iter()
            .any(|s| s.kind == ScopeKind::LocalRecord);
        assert!(has_record, "Expected LocalRecord scope kind");
    }

    // ---- Anonymous Class Member vs Capture ----

    #[test]
    fn test_anon_class_member_wins() {
        let content = r"
class C {
    void test() {
        int x = 1;
        Runnable r = new Runnable() {
            int x = 2;
            public void run() {
                System.out.println(x);
            }
        };
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "x", 2);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(
                result,
                ResolutionOutcome::Member {
                    qualified_name: Some(_)
                }
            ),
            "Expected anonymous-class member resolution for x: {result:?}"
        );
    }

    #[test]
    fn test_anon_class_capture_no_member() {
        let content = r"
class C {
    void test() {
        int y = 1;
        Runnable r = new Runnable() {
            public void run() {
                System.out.println(y);
            }
        };
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "y", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "y");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Captured y should resolve: {result:?}"
        );
    }

    // ---- Inherited Member Resolution ----

    #[test]
    fn test_inherited_member_resolved() {
        let content = r"
class Base {
    int value = 10;
}
class C {
    void test() {
        int value = 1;
        Object obj = new Base() {
            void use() {
                System.out.println(value);
            }
        };
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "value", 2);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "value");
        assert!(
            matches!(
                result,
                ResolutionOutcome::Member {
                    qualified_name: Some(ref qualified_name)
                } if qualified_name == "Base::value"
            ),
            "Expected inherited member resolution for value: {result:?}"
        );
    }

    // ---- Use-before-decl ----

    #[test]
    fn test_use_before_decl_outer_fallback() {
        let content = r"
class C {
    void test() {
        int y = 1;
        {
            System.out.println(y);
            int y = 5;
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        // Inner y=5 is overlap (skipped). println(y) resolves to outer y=1.
        let usage = find_identifier(root, content.as_bytes(), "y", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "y");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "Use-before-decl should fallback to outer: {result:?}"
        );
    }

    // ---- Try/Catch ----

    #[test]
    fn test_try_catch_scope() {
        let content = r"
class C {
    void test() {
        try {
            int x = 1;
            System.out.println(x);
        } catch (Exception e) {
            System.out.println(e);
        }
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let x_usage = find_identifier(root, content.as_bytes(), "x", 1);
        let x_result = scope_tree.resolve_identifier(x_usage.start_byte(), "x");
        assert!(
            matches!(x_result, ResolutionOutcome::Local(_)),
            "x in try body should resolve: {x_result:?}"
        );

        let e_usage = find_identifier(root, content.as_bytes(), "e", 1);
        let e_result = scope_tree.resolve_identifier(e_usage.start_byte(), "e");
        assert!(
            matches!(e_result, ResolutionOutcome::Local(_)),
            "e in catch body should resolve: {e_result:?}"
        );
    }

    // ---- No Match ----

    #[test]
    fn test_no_match_undeclared() {
        let content = r"
class C {
    void test() {
        System.out.println(x);
    }
}
";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "x", 0);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "x");
        assert!(
            matches!(result, ResolutionOutcome::NoMatch),
            "Undeclared x should return NoMatch: {result:?}"
        );
    }

    // ---- UTF-8 ----

    #[test]
    fn test_utf8_identifiers() {
        let content = "class C {\n    void test() {\n        int caf\u{00e9} = 1;\n        System.out.println(caf\u{00e9});\n    }\n}\n";
        let mut scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        let usage = find_identifier(root, content.as_bytes(), "caf\u{00e9}", 1);
        let result = scope_tree.resolve_identifier(usage.start_byte(), "caf\u{00e9}");
        assert!(
            matches!(result, ResolutionOutcome::Local(_)),
            "UTF-8 identifier should resolve: {result:?}"
        );
    }

    // ---- Helper Function Tests ----

    #[test]
    fn test_valid_span() {
        assert!(is_valid_span(0, 10, 100));
        assert!(is_valid_span(0, 100, 100));
        assert!(is_valid_span(5, 5, 100)); // zero-width is valid (start == end)
        assert!(!is_valid_span(10, 5, 100)); // start > end
        assert!(!is_valid_span(0, 101, 100)); // end > content_len
    }

    #[test]
    fn test_overlap_boundary_kinds() {
        assert!(ScopeKind::AnonymousClass.is_overlap_boundary());
        assert!(ScopeKind::LocalClass.is_overlap_boundary());
        assert!(ScopeKind::LocalRecord.is_overlap_boundary());
        assert!(ScopeKind::LocalEnum.is_overlap_boundary());
        assert!(ScopeKind::LocalInterface.is_overlap_boundary());
        assert!(!ScopeKind::Method.is_overlap_boundary());
        assert!(!ScopeKind::Block.is_overlap_boundary());
        assert!(!ScopeKind::Lambda.is_overlap_boundary());
    }

    #[test]
    fn test_non_capturing_class_scope_kinds() {
        assert!(ScopeKind::LocalRecord.is_non_capturing_class_scope());
        assert!(ScopeKind::LocalEnum.is_non_capturing_class_scope());
        assert!(ScopeKind::LocalInterface.is_non_capturing_class_scope());
        assert!(!ScopeKind::AnonymousClass.is_non_capturing_class_scope());
        assert!(!ScopeKind::LocalClass.is_non_capturing_class_scope());
        assert!(!ScopeKind::Method.is_non_capturing_class_scope());
    }

    #[test]
    fn test_class_scope_kinds() {
        assert!(ScopeKind::AnonymousClass.is_class_scope());
        assert!(ScopeKind::LocalClass.is_class_scope());
        assert!(ScopeKind::LocalRecord.is_class_scope());
        assert!(!ScopeKind::Method.is_class_scope());
        assert!(!ScopeKind::Lambda.is_class_scope());
    }

    #[test]
    fn test_find_applicable_binding_order() {
        let bindings = vec![
            VariableBinding {
                node_id: None,
                decl_start_byte: 10,
                decl_end_byte: 15,
                declarator_end_byte: 20,
                initializer_start_byte: Some(16),
            },
            VariableBinding {
                node_id: None,
                decl_start_byte: 50,
                decl_end_byte: 55,
                declarator_end_byte: 60,
                initializer_start_byte: Some(56),
            },
        ];

        assert!(
            find_applicable_binding(&bindings, 5).is_none(),
            "Before first decl"
        );

        let b = find_applicable_binding(&bindings, 25);
        assert!(b.is_some());
        assert_eq!(b.unwrap().decl_start_byte, 10);

        let b = find_applicable_binding(&bindings, 65);
        assert!(b.is_some());
        assert_eq!(b.unwrap().decl_start_byte, 50);
    }

    #[test]
    fn test_debug_log_limiter() {
        let mut limiter = DebugLogLimiter::default();
        for _ in 0..10 {
            limiter.log(DebugEvent::InvalidSpan, "test message");
        }
        assert_eq!(
            limiter.counts[&DebugEvent::InvalidSpan],
            MAX_DEBUG_LOGS_PER_EVENT,
            "Counter should cap at max"
        );
    }

    #[test]
    fn test_string_interner() {
        let mut interner = StringInterner::default();
        let arc1 = interner.intern("hello");
        let arc2 = interner.intern("hello");
        let arc3 = interner.intern("world");

        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "Same string should return same Arc"
        );
        assert!(
            !Arc::ptr_eq(&arc1, &arc3),
            "Different strings should return different Arcs"
        );
    }

    #[test]
    fn test_class_info_index_resolution() {
        let mut index = ClassInfoIndex::default();
        let info = ClassInfo {
            qualifier: "com.example.MyClass".to_string(),
            declared_members: HashSet::from([Arc::from("field1"), Arc::from("field2")]),
        };
        index.insert(
            info,
            &["MyClass".to_string(), "com.example.MyClass".to_string()],
        );

        let resolved = index.resolve("MyClass");
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().qualifier, "com.example.MyClass");

        let resolved = index.resolve("com.example.MyClass");
        assert!(resolved.is_some());

        let resolved = index.resolve("UnknownClass");
        assert!(resolved.is_none());
    }

    #[test]
    fn test_resolve_class_info_fallback() {
        let mut index = ClassInfoIndex::default();
        let info = ClassInfo {
            qualifier: "Base".to_string(),
            declared_members: HashSet::from([Arc::from("x")]),
        };
        index.insert(info, &["Base".to_string()]);

        assert!(resolve_class_info(&index, "Base").is_some());
        assert!(resolve_class_info(&index, "com.example.Base").is_some());
        assert!(resolve_class_info(&index, "Unknown").is_none());
    }

    #[test]
    fn test_has_local_binding() {
        let content = r"
class C {
    void test() {
        int x = 1;
        System.out.println(x);
    }
}
";
        let scope_tree = build_scope_tree(content);
        let tree = parse_java(content);
        let root = tree.root_node();

        // Find the usage of x (2nd occurrence: first is declaration, second is usage)
        let x_usage = find_identifier(root, content.as_bytes(), "x", 1);
        assert!(
            scope_tree.has_local_binding("x", x_usage.start_byte()),
            "x should have a local binding at its usage site"
        );
        assert!(
            !scope_tree.has_local_binding("y", x_usage.start_byte()),
            "y should NOT have a local binding (not declared)"
        );
    }

    #[test]
    fn test_recursion_limit_returns_error_and_guard_is_balanced() {
        // Deeply nested class declarations to trigger recursion limit.
        let content = r"
class A {
    void m() {
        class B {
            void n() {
                class C {
                    void o() {
                        class D {
                            void p() {
                                class E {
                                    void q() {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
";
        let tree = parse_java(content);
        let root = tree.root_node();
        let mut scope_tree = ScopeTree::new(content.len());

        // Guard with very low depth limit to trigger error
        let mut guard = sqry_core::query::security::RecursionGuard::new(3).expect("guard creation");
        let mut class_stack = Vec::new();

        let result = build_scopes_recursive(
            &mut scope_tree,
            root,
            content.as_bytes(),
            None,
            &mut guard,
            &mut class_stack,
        );

        // Should fail due to recursion limit
        assert!(result.is_err(), "Expected recursion limit error");

        // Guard depth should be 1: every successful enter() has a matching exit(),
        // but RecursionGuard::enter() increments before checking the limit, so
        // the one failed enter() leaves a single unmatched increment. This is
        // expected RecursionGuard behavior, not a bug in scope building.
        assert_eq!(
            guard.current_depth(),
            1,
            "Guard depth should be 1 (one unmatched failed enter())"
        );

        // Class stack should be empty (unwound on error paths)
        assert!(
            class_stack.is_empty(),
            "Class stack should be empty after error unwinding, but had: {class_stack:?}"
        );
    }
}
