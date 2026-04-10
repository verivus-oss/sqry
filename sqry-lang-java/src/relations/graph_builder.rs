use std::{collections::HashMap, path::Path};

use crate::relations::java_common::{PackageResolver, build_member_symbol, build_symbol};
use crate::relations::local_scopes::{self, JavaScopeTree, ResolutionOutcome};
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// File-level module name for exports/imports.
/// Distinct from `<module>` to avoid node kind collision in `GraphBuildHelper` cache.
const FILE_MODULE_NAME: &str = "<file_module>";

/// Graph builder for Java files using unified `CodeGraph` architecture.
///
/// This implementation follows the two-phase `ASTGraph` architecture introduced
/// in JavaScript and Rust for O(1) context lookups during call edge detection.
///
/// # Supported Features
///
/// - Class and interface definitions
/// - Method definitions (instance, static, constructors)
/// - Method call expressions
/// - Constructor calls (new expressions)
/// - Static method calls (`Class.method()`)
/// - Import declarations (single and wildcard)
/// - Export edges (public classes, interfaces, methods, fields)
/// - Package declarations
/// - JNI detection (native methods)
/// - Anonymous classes and lambda expressions
/// - Nested classes
/// - Synchronized detection
/// - Proper argument counting
#[derive(Debug, Clone, Copy)]
pub struct JavaGraphBuilder {
    max_scope_depth: usize,
}

impl Default for JavaGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl JavaGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for JavaGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Java);

        // Build AST context for O(1) method lookups
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth);
        let mut scope_tree = local_scopes::build(tree.root_node(), content, Some(file))?;

        // Phase 1: Create method/constructor nodes and JNI FFI edges for native methods
        for context in ast_graph.contexts() {
            let qualified_name = context.qualified_name();
            let span = Span::from_bytes(context.span.0, context.span.1);

            if context.is_constructor {
                helper.add_method_with_visibility(
                    qualified_name,
                    Some(span),
                    false,
                    false,
                    context.visibility.as_deref(),
                );
            } else {
                // Use add_method_with_signature to store return type for `returns:` queries
                helper.add_method_with_signature(
                    qualified_name,
                    Some(span),
                    false,
                    context.is_static,
                    context.visibility.as_deref(),
                    context.return_type.as_deref(),
                );

                // JNI: Create FFI edge for native methods
                if context.is_native {
                    build_jni_native_method_edge(context, &mut helper);
                }
            }
        }

        // Phase 1.5: Add TypeOf edges for fields
        add_field_typeof_edges(&ast_graph, &mut helper);

        // Phase 2: Walk the tree to find calls, imports, classes, interfaces
        let root = tree.root_node();
        walk_tree_for_edges(
            root,
            content,
            &ast_graph,
            &mut scope_tree,
            &mut helper,
            tree,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Java
    }
}

// ================================
// ASTGraph: In-memory function context index
// ================================

#[derive(Debug)]
struct ASTGraph {
    contexts: Vec<MethodContext>,
    /// Maps qualified field names to their metadata: (`type_fqn`, `is_final`, visibility, `is_static`)
    /// - Key: Qualified field name (e.g., `ClassName::fieldName`)
    /// - Tuple: (`type_fqn`, `is_final`, visibility, `is_static`)
    ///   - `type_fqn`: Fully qualified type name (e.g., `com.example.service.UserService`)
    ///   - `is_final`: true if field has `final` modifier (determines Constant vs Property node)
    ///   - visibility: Public or Private (public fields are Public, others are Private)
    ///   - `is_static`: true if field has `static` modifier
    ///
    /// Used to resolve method calls on fields and create appropriate node types with metadata
    field_types: HashMap<String, (String, bool, Option<sqry_core::schema::Visibility>, bool)>,
    /// Maps simple type names to FQNs (e.g., `UserService` -> `com.example.service.UserService`)
    /// Used to resolve static method calls (e.g., `UserRepository.method` ->
    /// `com.example.repository.UserRepository.method`)
    import_map: HashMap<String, String>,
    /// Whether this file imports JNA (`com.sun.jna.*`)
    has_jna_import: bool,
    /// Whether this file imports Panama Foreign Function API (`java.lang.foreign.*`)
    has_panama_import: bool,
    /// Interfaces that extend JNA Library (simple names)
    jna_library_interfaces: Vec<String>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Self {
        // Extract package name from AST
        let package_name = PackageResolver::package_from_ast(tree, content);

        let mut contexts = Vec::new();
        let mut class_stack = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .expect("Failed to load recursion limits");
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .expect("Invalid file_ops_depth configuration");
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .expect("Failed to create recursion guard");

        if let Err(e) = extract_java_contexts(
            tree.root_node(),
            content,
            &mut contexts,
            &mut class_stack,
            package_name.as_deref(),
            0,
            max_depth,
            &mut guard,
        ) {
            eprintln!("Warning: Java AST traversal hit recursion limit: {e}");
        }

        // Extract field declarations and imports to enable type resolution
        let (field_types, import_map) = extract_field_and_import_types(tree.root_node(), content);

        // Detect FFI-related imports
        let (has_jna_import, has_panama_import) = detect_ffi_imports(tree.root_node(), content);

        // Find interfaces extending JNA Library
        let jna_library_interfaces = find_jna_library_interfaces(tree.root_node(), content);

        Self {
            contexts,
            field_types,
            import_map,
            has_jna_import,
            has_panama_import,
            jna_library_interfaces,
        }
    }

    fn contexts(&self) -> &[MethodContext] {
        &self.contexts
    }

    /// Find the enclosing method context for a given byte position
    fn find_enclosing(&self, byte_pos: usize) -> Option<&MethodContext> {
        self.contexts
            .iter()
            .filter(|ctx| byte_pos >= ctx.span.0 && byte_pos < ctx.span.1)
            .max_by_key(|ctx| ctx.depth)
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Captures explicit method traits for graph resolution.
struct MethodContext {
    /// Fully qualified name: `com.example.Class.method` or `com.example.Class.<init>`
    qualified_name: String,
    /// Byte span of the method body
    span: (usize, usize),
    /// Nesting depth (for resolving ambiguity)
    depth: usize,
    /// Whether this is a static method
    is_static: bool,
    /// Whether this is synchronized
    #[allow(dead_code)] // Reserved for threading analysis
    is_synchronized: bool,
    /// Whether this is a constructor
    is_constructor: bool,
    /// Whether this is a native method (JNI)
    #[allow(dead_code)] // Reserved for JNI bridge analysis
    is_native: bool,
    /// Package name for use in call resolution (e.g., `com.example`)
    package_name: Option<String>,
    /// Class stack for use in call resolution (e.g., `["Outer", "Inner"]`)
    class_stack: Vec<String>,
    /// Return type of the method (e.g., `Optional<User>`, `void`)
    return_type: Option<String>,
    /// Visibility modifier (e.g., "public", "private", "protected", "package-private")
    visibility: Option<String>,
}

impl MethodContext {
    fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
}

// ================================
// Context Extraction
// ================================

/// Recursively extract method contexts from Java AST
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_java_contexts(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<MethodContext>,
    class_stack: &mut Vec<String>,
    package_name: Option<&str>,
    depth: usize,
    max_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    guard.enter()?;

    if depth > max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration" => {
            // Extract class/interface name
            if let Some(name_node) = node.child_by_field_name("name") {
                let class_name = extract_identifier(name_node, content);

                // Push class onto stack for nested context
                class_stack.push(class_name.clone());

                // Extract methods within this class
                if let Some(body_node) = node.child_by_field_name("body") {
                    extract_methods_from_body(
                        body_node,
                        content,
                        class_stack,
                        package_name,
                        contexts,
                        depth + 1,
                        max_depth,
                        guard,
                    )?;

                    // Handle nested classes (recursively)
                    for i in 0..body_node.child_count() {
                        #[allow(clippy::cast_possible_truncation)]
                        // Graph storage: node/edge index counts fit in u32
                        if let Some(child) = body_node.child(i as u32) {
                            extract_java_contexts(
                                child,
                                content,
                                contexts,
                                class_stack,
                                package_name,
                                depth + 1,
                                max_depth,
                                guard,
                            )?;
                        }
                    }
                }

                // Pop class from stack when exiting
                class_stack.pop();

                guard.exit();
                return Ok(());
            }
        }
        _ => {}
    }

    // Continue traversing for top-level declarations
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            extract_java_contexts(
                child,
                content,
                contexts,
                class_stack,
                package_name,
                depth,
                max_depth,
                guard,
            )?;
        }
    }

    guard.exit();
    Ok(())
}

/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
#[allow(clippy::unnecessary_wraps)]
fn extract_methods_from_body(
    body_node: Node,
    content: &[u8],
    class_stack: &[String],
    package_name: Option<&str>,
    contexts: &mut Vec<MethodContext>,
    depth: usize,
    _max_depth: usize,
    _guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    for i in 0..body_node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = body_node.child(i as u32) {
            match child.kind() {
                "method_declaration" => {
                    if let Some(method_context) =
                        extract_method_context(child, content, class_stack, package_name, depth)
                    {
                        contexts.push(method_context);
                    }
                }
                "constructor_declaration" | "compact_constructor_declaration" => {
                    let constructor_context = extract_constructor_context(
                        child,
                        content,
                        class_stack,
                        package_name,
                        depth,
                    );
                    contexts.push(constructor_context);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn extract_method_context(
    method_node: Node,
    content: &[u8],
    class_stack: &[String],
    package_name: Option<&str>,
    depth: usize,
) -> Option<MethodContext> {
    let name_node = method_node.child_by_field_name("name")?;
    let method_name = extract_identifier(name_node, content);

    let is_static = has_modifier(method_node, "static", content);
    let is_synchronized = has_modifier(method_node, "synchronized", content);
    let is_native = has_modifier(method_node, "native", content);
    let visibility = extract_visibility(method_node, content);

    // Extract return type from method_declaration
    // tree-sitter-java structure: (method_declaration type: <type_node> name: identifier ...)
    let return_type = method_node
        .child_by_field_name("type")
        .map(|type_node| extract_full_return_type(type_node, content));

    // Use build_member_symbol to create fully qualified name
    let qualified_name = build_member_symbol(package_name, class_stack, &method_name);

    Some(MethodContext {
        qualified_name,
        span: (method_node.start_byte(), method_node.end_byte()),
        depth,
        is_static,
        is_synchronized,
        is_constructor: false,
        is_native,
        package_name: package_name.map(std::string::ToString::to_string),
        class_stack: class_stack.to_vec(),
        return_type,
        visibility,
    })
}

fn extract_constructor_context(
    constructor_node: Node,
    content: &[u8],
    class_stack: &[String],
    package_name: Option<&str>,
    depth: usize,
) -> MethodContext {
    // Use build_member_symbol with "<init>" as method name
    let qualified_name = build_member_symbol(package_name, class_stack, "<init>");
    let visibility = extract_visibility(constructor_node, content);

    MethodContext {
        qualified_name,
        span: (constructor_node.start_byte(), constructor_node.end_byte()),
        depth,
        is_static: false,
        is_synchronized: false,
        is_constructor: true,
        is_native: false,
        package_name: package_name.map(std::string::ToString::to_string),
        class_stack: class_stack.to_vec(),
        return_type: None, // Constructors don't have return types
        visibility,
    }
}

// ================================
// Edge Building with GraphBuildHelper
// ================================

/// Walk the AST tree and build edges using `GraphBuildHelper`
fn walk_tree_for_edges(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
    tree: &Tree,
) -> GraphResult<()> {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration" => {
            // handle_type_declaration already walks the body children, so return early
            return handle_type_declaration(node, content, ast_graph, scope_tree, helper, tree);
        }
        "method_declaration" | "constructor_declaration" => {
            // Handle both method and constructor parameters
            handle_method_declaration_parameters(node, content, ast_graph, scope_tree, helper);

            // Detect Spring MVC route annotations on method declarations
            if node.kind() == "method_declaration"
                && let Some((http_method, path)) = extract_spring_route_info(node, content)
            {
                // Compose class-level @RequestMapping prefix with method path
                let full_path =
                    if let Some(class_prefix) = extract_class_request_mapping_path(node, content) {
                        let prefix = class_prefix.trim_end_matches('/');
                        let suffix = path.trim_start_matches('/');
                        if suffix.is_empty() {
                            class_prefix
                        } else {
                            format!("{prefix}/{suffix}")
                        }
                    } else {
                        path
                    };
                let qualified_name = format!("route::{http_method}::{full_path}");
                let span = Span::from_bytes(node.start_byte(), node.end_byte());
                let endpoint_id = helper.add_endpoint(&qualified_name, Some(span));

                // Link endpoint to the handler method via Contains edge
                let byte_pos = node.start_byte();
                if let Some(context) = ast_graph.find_enclosing(byte_pos) {
                    let method_id = helper.ensure_method(
                        context.qualified_name(),
                        Some(Span::from_bytes(context.span.0, context.span.1)),
                        false,
                        context.is_static,
                    );
                    helper.add_contains_edge(endpoint_id, method_id);
                }
            }
        }
        "compact_constructor_declaration" => {
            handle_compact_constructor_parameters(node, content, ast_graph, scope_tree, helper);
        }
        "method_invocation" => {
            handle_method_invocation(node, content, ast_graph, helper);
        }
        "object_creation_expression" => {
            handle_constructor_call(node, content, ast_graph, helper);
        }
        "import_declaration" => {
            handle_import_declaration(node, content, helper);
        }
        "local_variable_declaration" => {
            handle_local_variable_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "enhanced_for_statement" => {
            handle_enhanced_for_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "catch_clause" => {
            handle_catch_parameter_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "lambda_expression" => {
            handle_lambda_parameter_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "try_with_resources_statement" => {
            handle_try_with_resources_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "instanceof_expression" => {
            handle_instanceof_pattern_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "switch_label" => {
            handle_switch_pattern_declaration(node, content, ast_graph, scope_tree, helper);
        }
        "identifier" => {
            handle_identifier_for_reference(node, content, ast_graph, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse to children
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            walk_tree_for_edges(child, content, ast_graph, scope_tree, helper, tree)?;
        }
    }

    Ok(())
}

fn handle_type_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
    tree: &Tree,
) -> GraphResult<()> {
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(());
    };
    let class_name = extract_identifier(name_node, content);
    let span = Span::from_bytes(node.start_byte(), node.end_byte());

    let package = PackageResolver::package_from_ast(tree, content);
    let class_stack = extract_declaration_class_stack(node, content);
    let qualified_name = qualify_class_name(&class_name, &class_stack, package.as_deref());
    let class_node_id = add_type_node(helper, node.kind(), &qualified_name, span);

    if is_public(node, content) {
        export_from_file_module(helper, class_node_id);
    }

    process_inheritance(node, content, package.as_deref(), class_node_id, helper);
    if node.kind() == "class_declaration" {
        process_implements(node, content, package.as_deref(), class_node_id, helper);
    }
    if node.kind() == "interface_declaration" {
        process_interface_extends(node, content, package.as_deref(), class_node_id, helper);
    }

    if let Some(body_node) = node.child_by_field_name("body") {
        let is_interface = node.kind() == "interface_declaration";
        process_class_member_exports(body_node, content, &qualified_name, helper, is_interface);

        for i in 0..body_node.child_count() {
            #[allow(clippy::cast_possible_truncation)]
            // Graph storage: node/edge index counts fit in u32
            if let Some(child) = body_node.child(i as u32) {
                walk_tree_for_edges(child, content, ast_graph, scope_tree, helper, tree)?;
            }
        }
    }

    Ok(())
}

fn extract_declaration_class_stack(node: Node, content: &[u8]) -> Vec<String> {
    let mut class_stack = Vec::new();
    let mut current_node = Some(node);

    while let Some(current) = current_node {
        if matches!(
            current.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) && let Some(name_node) = current.child_by_field_name("name")
        {
            class_stack.push(extract_identifier(name_node, content));
        }

        current_node = current.parent();
    }

    class_stack.reverse();
    class_stack
}

fn qualify_class_name(class_name: &str, class_stack: &[String], package: Option<&str>) -> String {
    let scope = class_stack
        .split_last()
        .map_or(&[][..], |(_, parent_stack)| parent_stack);
    build_symbol(package, scope, class_name)
}

fn add_type_node(
    helper: &mut GraphBuildHelper,
    kind: &str,
    qualified_name: &str,
    span: Span,
) -> sqry_core::graph::unified::node::NodeId {
    match kind {
        "interface_declaration" => helper.add_interface(qualified_name, Some(span)),
        _ => helper.add_class(qualified_name, Some(span)),
    }
}

fn handle_method_invocation(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    if let Some(caller_context) = ast_graph.find_enclosing(node.start_byte()) {
        let is_ffi = build_ffi_call_edge(node, content, caller_context, ast_graph, helper);
        if is_ffi {
            return;
        }
    }

    process_method_call_unified(node, content, ast_graph, helper);
}

fn handle_constructor_call(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    process_constructor_call_unified(node, content, ast_graph, helper);
}

fn handle_import_declaration(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    process_import_unified(node, content, helper);
}

/// Add `TypeOf` edges for all field declarations
/// Creates Property nodes for mutable fields and Constant nodes for final fields
fn add_field_typeof_edges(ast_graph: &ASTGraph, helper: &mut GraphBuildHelper) {
    for (field_name, (type_fqn, is_final, visibility, is_static)) in &ast_graph.field_types {
        // Create appropriate node type based on 'final' modifier, with visibility and static metadata
        let field_id = if *is_final {
            // final fields are constants
            if let Some(vis) = visibility {
                helper.add_constant_with_static_and_visibility(
                    field_name,
                    None,
                    *is_static,
                    Some(vis.as_str()),
                )
            } else {
                helper.add_constant_with_static_and_visibility(field_name, None, *is_static, None)
            }
        } else {
            // non-final fields are properties
            if let Some(vis) = visibility {
                helper.add_property_with_static_and_visibility(
                    field_name,
                    None,
                    *is_static,
                    Some(vis.as_str()),
                )
            } else {
                helper.add_property_with_static_and_visibility(field_name, None, *is_static, None)
            }
        };

        // Create class node for the type
        let type_id = helper.add_class(type_fqn, None);

        // Create TypeOf edge from field to its type
        helper.add_typeof_edge(field_id, type_id);
    }
}

/// Extract method parameters and create Parameter nodes with `TypeOf` edges
/// Should be called during method context creation
fn extract_method_parameters(
    method_node: Node,
    content: &[u8],
    qualified_method_name: &str,
    helper: &mut GraphBuildHelper,
    import_map: &HashMap<String, String>,
    scope_tree: &mut JavaScopeTree,
) {
    // Find formal_parameters node in the method declaration
    let mut cursor = method_node.walk();
    for child in method_node.children(&mut cursor) {
        if child.kind() == "formal_parameters" {
            // Iterate through each parameter (formal, varargs, receiver)
            let mut param_cursor = child.walk();
            for param_child in child.children(&mut param_cursor) {
                match param_child.kind() {
                    "formal_parameter" => {
                        handle_formal_parameter(
                            param_child,
                            content,
                            qualified_method_name,
                            helper,
                            import_map,
                            scope_tree,
                        );
                    }
                    "spread_parameter" => {
                        handle_spread_parameter(
                            param_child,
                            content,
                            qualified_method_name,
                            helper,
                            import_map,
                            scope_tree,
                        );
                    }
                    "receiver_parameter" => {
                        handle_receiver_parameter(
                            param_child,
                            content,
                            qualified_method_name,
                            helper,
                            import_map,
                            scope_tree,
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Handle a single formal parameter and create Parameter node with `TypeOf` edge
fn handle_formal_parameter(
    param_node: Node,
    content: &[u8],
    method_name: &str,
    helper: &mut GraphBuildHelper,
    import_map: &HashMap<String, String>,
    scope_tree: &mut JavaScopeTree,
) {
    use sqry_core::graph::unified::node::NodeKind;

    // Extract type from formal_parameter
    let Some(type_node) = param_node.child_by_field_name("type") else {
        return;
    };

    // Extract parameter name
    let Some(name_node) = param_node.child_by_field_name("name") else {
        return;
    };

    // Get type and parameter name texts
    let type_text = extract_type_name(type_node, content);
    let param_name = extract_identifier(name_node, content);

    if type_text.is_empty() || param_name.is_empty() {
        return;
    }

    // Resolve type to FQN using import map
    let resolved_type = import_map.get(&type_text).cloned().unwrap_or(type_text);

    // Create qualified parameter name (method::param)
    let qualified_param = format!("{method_name}::{param_name}");
    let span = Span::from_bytes(param_node.start_byte(), param_node.end_byte());

    // Create parameter node
    let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);

    scope_tree.attach_node_id(&param_name, name_node.start_byte(), param_id);

    // Create type node (class/interface)
    let type_id = helper.add_class(&resolved_type, None);

    // Add TypeOf edge from parameter to its type
    helper.add_typeof_edge(param_id, type_id);
}

/// Handle a spread parameter (varargs like String... args)
fn handle_spread_parameter(
    param_node: Node,
    content: &[u8],
    method_name: &str,
    helper: &mut GraphBuildHelper,
    import_map: &HashMap<String, String>,
    scope_tree: &mut JavaScopeTree,
) {
    use sqry_core::graph::unified::node::NodeKind;

    // spread_parameter structure:
    // (spread_parameter
    //   type_identifier
    //   ...
    //   variable_declarator
    //     identifier)

    // Find type node (first type_identifier child)
    let mut type_text = String::new();
    let mut param_name = String::new();
    let mut param_name_node = None;

    let mut cursor = param_node.walk();
    for child in param_node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "generic_type" | "scoped_type_identifier" => {
                type_text = extract_type_name(child, content);
            }
            "variable_declarator" => {
                // Name is inside variable_declarator
                if let Some(name_node) = child.child_by_field_name("name") {
                    param_name = extract_identifier(name_node, content);
                    param_name_node = Some(name_node);
                }
            }
            _ => {}
        }
    }

    if type_text.is_empty() || param_name.is_empty() {
        return;
    }

    // Resolve type to FQN using import map
    let resolved_type = import_map.get(&type_text).cloned().unwrap_or(type_text);

    // Create qualified parameter name (method::param)
    let qualified_param = format!("{method_name}::{param_name}");
    let span = Span::from_bytes(param_node.start_byte(), param_node.end_byte());

    // Create parameter node
    let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);

    if let Some(name_node) = param_name_node {
        scope_tree.attach_node_id(&param_name, name_node.start_byte(), param_id);
    }

    // Create type node for the array type (resolved_type represents the element type)
    // For varargs, the actual type is an array of the base type
    let type_id = helper.add_class(&resolved_type, None);

    // Add TypeOf edge from parameter to its type
    helper.add_typeof_edge(param_id, type_id);
}

/// Handle a receiver parameter (e.g., Outer.this in inner class methods)
fn handle_receiver_parameter(
    param_node: Node,
    content: &[u8],
    method_name: &str,
    helper: &mut GraphBuildHelper,
    import_map: &HashMap<String, String>,
    _scope_tree: &mut JavaScopeTree,
) {
    use sqry_core::graph::unified::node::NodeKind;

    // receiver_parameter structure:
    // (receiver_parameter
    //   type_identifier
    //   identifier (optional - class name)
    //   .
    //   this)

    let mut type_text = String::new();
    let mut cursor = param_node.walk();

    // Find the type_identifier child
    for child in param_node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "type_identifier" | "generic_type" | "scoped_type_identifier"
        ) {
            type_text = extract_type_name(child, content);
            break;
        }
    }

    if type_text.is_empty() {
        return;
    }

    // Receiver parameter name is always "this"
    let param_name = "this";

    // Resolve type to FQN using import map
    let resolved_type = import_map.get(&type_text).cloned().unwrap_or(type_text);

    // Create qualified parameter name (method::this)
    let qualified_param = format!("{method_name}::{param_name}");
    let span = Span::from_bytes(param_node.start_byte(), param_node.end_byte());

    // Create parameter node
    let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);

    // Create type node (class)
    let type_id = helper.add_class(&resolved_type, None);

    // Add TypeOf edge from parameter to its type
    helper.add_typeof_edge(param_id, type_id);
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FieldAccessRole {
    Default,
    ExplicitThisOrSuper,
    Skip,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FieldResolutionMode {
    Default,
    CurrentOnly,
}

fn field_access_role(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &JavaScopeTree,
    identifier_text: &str,
) -> FieldAccessRole {
    let Some(parent) = node.parent() else {
        return FieldAccessRole::Default;
    };

    if parent.kind() == "field_access" {
        if let Some(field_node) = parent.child_by_field_name("field")
            && field_node.id() == node.id()
            && let Some(object_node) = parent.child_by_field_name("object")
        {
            if is_explicit_this_or_super(object_node, content) {
                return FieldAccessRole::ExplicitThisOrSuper;
            }
            return FieldAccessRole::Skip;
        }

        if let Some(object_node) = parent.child_by_field_name("object")
            && object_node.id() == node.id()
            && !scope_tree.has_local_binding(identifier_text, node.start_byte())
            && is_static_type_identifier(identifier_text, ast_graph, scope_tree)
        {
            return FieldAccessRole::Skip;
        }
    }

    if parent.kind() == "method_invocation"
        && let Some(object_node) = parent.child_by_field_name("object")
        && object_node.id() == node.id()
        && !scope_tree.has_local_binding(identifier_text, node.start_byte())
        && is_static_type_identifier(identifier_text, ast_graph, scope_tree)
    {
        return FieldAccessRole::Skip;
    }

    if parent.kind() == "method_reference"
        && let Some(object_node) = parent.child_by_field_name("object")
        && object_node.id() == node.id()
        && !scope_tree.has_local_binding(identifier_text, node.start_byte())
        && is_static_type_identifier(identifier_text, ast_graph, scope_tree)
    {
        return FieldAccessRole::Skip;
    }

    FieldAccessRole::Default
}

fn is_static_type_identifier(
    identifier_text: &str,
    ast_graph: &ASTGraph,
    scope_tree: &JavaScopeTree,
) -> bool {
    ast_graph.import_map.contains_key(identifier_text)
        || scope_tree.is_known_type_name(identifier_text)
}

fn is_explicit_this_or_super(node: Node, content: &[u8]) -> bool {
    if matches!(node.kind(), "this" | "super") {
        return true;
    }
    if node.kind() == "identifier" {
        let text = extract_identifier(node, content);
        return matches!(text.as_str(), "this" | "super");
    }
    if node.kind() == "field_access"
        && let Some(field) = node.child_by_field_name("field")
    {
        let text = extract_identifier(field, content);
        if matches!(text.as_str(), "this" | "super") {
            return true;
        }
    }
    false
}

/// Check if an identifier node is part of a declaration context
/// Returns true if the identifier is being declared (not referenced)
#[allow(clippy::too_many_lines)]
fn is_declaration_context(node: Node) -> bool {
    // Check if parent is a declaration node
    let Some(parent) = node.parent() else {
        return false;
    };

    // For variable_declarator, only the 'name' field is a declaration, not 'value'
    // Example: `String key = API_KEY`
    //   - 'key' has parent variable_declarator with field 'name' (declaration)
    //   - 'API_KEY' has parent variable_declarator with field 'value' (NOT declaration)
    if parent.kind() == "variable_declarator" {
        // Check if this identifier is the 'name' field
        let mut cursor = parent.walk();
        for (idx, child) in parent.children(&mut cursor).enumerate() {
            if child.id() == node.id() {
                #[allow(clippy::cast_possible_truncation)]
                if let Some(field_name) = parent.field_name_for_child(idx as u32) {
                    // Only 'name' field is declaration context, not 'value'
                    return field_name == "name";
                }
                break;
            }
        }

        // If inside variable_declarator that's inside spread_parameter, it's a declaration
        if let Some(grandparent) = parent.parent()
            && grandparent.kind() == "spread_parameter"
        {
            return true;
        }

        return false;
    }

    // For formal_parameter, only the 'name' field is a declaration
    if parent.kind() == "formal_parameter" {
        let mut cursor = parent.walk();
        for (idx, child) in parent.children(&mut cursor).enumerate() {
            if child.id() == node.id() {
                #[allow(clippy::cast_possible_truncation)]
                if let Some(field_name) = parent.field_name_for_child(idx as u32) {
                    return field_name == "name";
                }
                break;
            }
        }
        return false;
    }

    // For enhanced_for_statement, only the loop variable 'name' field is a declaration
    // Example: `for (String item : items)` - 'item' is declaration, 'items' is not
    if parent.kind() == "enhanced_for_statement" {
        // Check if this identifier is the loop variable name field
        let mut cursor = parent.walk();
        for (idx, child) in parent.children(&mut cursor).enumerate() {
            if child.id() == node.id() {
                #[allow(clippy::cast_possible_truncation)]
                if let Some(field_name) = parent.field_name_for_child(idx as u32) {
                    // Only the 'name' field is declaration, not the iterable expression
                    return field_name == "name";
                }
                break;
            }
        }
        return false;
    }

    if parent.kind() == "lambda_expression" {
        if let Some(params) = parent.child_by_field_name("parameters") {
            return params.id() == node.id();
        }
        return false;
    }

    if parent.kind() == "inferred_parameters" {
        return true;
    }

    if parent.kind() == "resource" {
        if let Some(name_node) = parent.child_by_field_name("name")
            && name_node.id() == node.id()
        {
            let has_type = parent.child_by_field_name("type").is_some();
            let has_value = parent.child_by_field_name("value").is_some();
            return has_type || has_value;
        }
        return false;
    }

    // Pattern variables (Java 16+)
    // Type pattern: case String s -> ...; if (obj instanceof String s)
    // The 'name' field is the pattern variable declaration
    if parent.kind() == "type_pattern" {
        if let Some((name_node, _type_node)) = typed_pattern_parts(parent) {
            return name_node.id() == node.id();
        }
        return false;
    }

    // instanceof pattern: if (obj instanceof String value)
    if parent.kind() == "instanceof_expression" {
        let mut cursor = parent.walk();
        for (idx, child) in parent.children(&mut cursor).enumerate() {
            if child.id() == node.id() {
                #[allow(clippy::cast_possible_truncation)]
                if let Some(field_name) = parent.field_name_for_child(idx as u32) {
                    // The 'name' field in instanceof_expression is the pattern variable
                    return field_name == "name";
                }
                break;
            }
        }
        return false;
    }

    // Record pattern components: case Point(int x, int y)
    // The identifiers in record_pattern_component are declarations
    if parent.kind() == "record_pattern_component" {
        // In record pattern component, the second child (after type) is the identifier declaration
        let mut cursor = parent.walk();
        for child in parent.children(&mut cursor) {
            if child.id() == node.id() && child.kind() == "identifier" {
                // This is a pattern variable declaration
                return true;
            }
        }
        return false;
    }

    if parent.kind() == "record_component" {
        if let Some(name_node) = parent.child_by_field_name("name") {
            return name_node.id() == node.id();
        }
        return false;
    }

    // For other declaration contexts, any direct child identifier is considered a declaration
    matches!(
        parent.kind(),
        "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "field_declaration"
            | "catch_formal_parameter"
    )
}

fn is_method_invocation_name(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "method_invocation" {
        return false;
    }
    parent
        .child_by_field_name("name")
        .is_some_and(|name_node| name_node.id() == node.id())
}

fn is_method_reference_name(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "method_reference" {
        return false;
    }
    parent
        .child_by_field_name("name")
        .is_some_and(|name_node| name_node.id() == node.id())
}

fn is_label_identifier(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "labeled_statement" {
        return true;
    }
    if matches!(parent.kind(), "break_statement" | "continue_statement")
        && let Some(label) = parent.child_by_field_name("label")
    {
        return label.id() == node.id();
    }
    false
}

fn is_class_literal(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "class_literal"
}

fn is_type_identifier_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(
        parent.kind(),
        "type_identifier"
            | "scoped_type_identifier"
            | "scoped_identifier"
            | "generic_type"
            | "type_argument"
            | "type_bound"
    )
}

fn add_reference_edge_for_target(
    usage_node: Node,
    identifier_text: &str,
    target_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let usage_span = Span::from_bytes(usage_node.start_byte(), usage_node.end_byte());
    let usage_id = helper.add_node(
        &format!("{}@{}", identifier_text, usage_node.start_byte()),
        Some(usage_span),
        sqry_core::graph::unified::node::NodeKind::Variable,
    );
    helper.add_reference_edge(usage_id, target_id);
}

fn resolve_field_reference(
    node: Node,
    identifier_text: &str,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    mode: FieldResolutionMode,
) {
    let context = ast_graph.find_enclosing(node.start_byte());
    let mut candidates = Vec::new();
    if let Some(ctx) = context
        && !ctx.class_stack.is_empty()
    {
        if mode == FieldResolutionMode::CurrentOnly {
            let class_path = ctx.class_stack.join("::");
            candidates.push(format!("{class_path}::{identifier_text}"));
        } else {
            let stack_len = ctx.class_stack.len();
            for idx in (1..=stack_len).rev() {
                let class_path = ctx.class_stack[..idx].join("::");
                candidates.push(format!("{class_path}::{identifier_text}"));
            }
        }
    }

    if mode != FieldResolutionMode::CurrentOnly {
        candidates.push(identifier_text.to_string());
    }

    for candidate in candidates {
        if ast_graph.field_types.contains_key(&candidate) {
            add_field_reference(node, identifier_text, &candidate, ast_graph, helper);
            return;
        }
    }
}

fn add_field_reference(
    node: Node,
    identifier_text: &str,
    field_name: &str,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    let usage_span = Span::from_bytes(node.start_byte(), node.end_byte());
    let usage_id = helper.add_node(
        &format!("{}@{}", identifier_text, node.start_byte()),
        Some(usage_span),
        sqry_core::graph::unified::node::NodeKind::Variable,
    );

    let field_metadata = ast_graph.field_types.get(field_name);
    let field_id = if let Some((_, is_final, visibility, is_static)) = field_metadata {
        if *is_final {
            if let Some(vis) = visibility {
                helper.add_constant_with_static_and_visibility(
                    field_name,
                    None,
                    *is_static,
                    Some(vis.as_str()),
                )
            } else {
                helper.add_constant_with_static_and_visibility(field_name, None, *is_static, None)
            }
        } else if let Some(vis) = visibility {
            helper.add_property_with_static_and_visibility(
                field_name,
                None,
                *is_static,
                Some(vis.as_str()),
            )
        } else {
            helper.add_property_with_static_and_visibility(field_name, None, *is_static, None)
        }
    } else {
        helper.add_property_with_static_and_visibility(field_name, None, false, None)
    };

    helper.add_reference_edge(usage_id, field_id);
}

/// Handle identifier nodes to create Reference edges for variable/field accesses
#[allow(clippy::similar_names)]
fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier_text = extract_identifier(node, content);

    if identifier_text.is_empty() {
        return;
    }

    // Skip if this identifier is part of a declaration
    if is_declaration_context(node) {
        return;
    }

    if is_method_invocation_name(node)
        || is_method_reference_name(node)
        || is_label_identifier(node)
        || is_class_literal(node)
    {
        return;
    }

    if is_type_identifier_context(node) {
        return;
    }

    let field_access_role =
        field_access_role(node, content, ast_graph, scope_tree, &identifier_text);
    if matches!(field_access_role, FieldAccessRole::Skip) {
        return;
    }

    let allow_local = matches!(field_access_role, FieldAccessRole::Default);
    let allow_field = !matches!(field_access_role, FieldAccessRole::Skip);
    let field_mode = if matches!(field_access_role, FieldAccessRole::ExplicitThisOrSuper) {
        FieldResolutionMode::CurrentOnly
    } else {
        FieldResolutionMode::Default
    };

    if allow_local {
        match scope_tree.resolve_identifier(node.start_byte(), &identifier_text) {
            ResolutionOutcome::Local(binding) => {
                let target_id = if let Some(node_id) = binding.node_id {
                    node_id
                } else {
                    let span = Span::from_bytes(binding.decl_start_byte, binding.decl_end_byte);
                    let qualified_var = format!("{}@{}", identifier_text, binding.decl_start_byte);
                    let var_id = helper.add_variable(&qualified_var, Some(span));
                    scope_tree.attach_node_id(&identifier_text, binding.decl_start_byte, var_id);
                    var_id
                };
                add_reference_edge_for_target(node, &identifier_text, target_id, helper);
                return;
            }
            ResolutionOutcome::Member { qualified_name } => {
                if let Some(field_name) = qualified_name {
                    add_field_reference(node, &identifier_text, &field_name, ast_graph, helper);
                }
                return;
            }
            ResolutionOutcome::Ambiguous => {
                return;
            }
            ResolutionOutcome::NoMatch => {}
        }
    }

    if !allow_field {
        return;
    }

    resolve_field_reference(node, &identifier_text, ast_graph, helper, field_mode);
}

/// Handle method declarations to extract parameter `TypeOf` edges
fn handle_method_declaration_parameters(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    // Find the enclosing method context to get the qualified name
    let byte_pos = node.start_byte();
    if let Some(context) = ast_graph.find_enclosing(byte_pos) {
        let qualified_method_name = &context.qualified_name;

        // Extract parameters from this method
        extract_method_parameters(
            node,
            content,
            qualified_method_name,
            helper,
            &ast_graph.import_map,
            scope_tree,
        );
    }
}

/// Handle local variable declarations and create `TypeOf` edges
fn handle_local_variable_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    // Extract the type from the local variable declaration
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };

    let type_text = extract_type_name(type_node, content);
    if type_text.is_empty() {
        return;
    }

    // Resolve type through import map (e.g., Optional<User> -> java.util.Optional)
    let resolved_type = ast_graph
        .import_map
        .get(&type_text)
        .cloned()
        .unwrap_or_else(|| type_text.clone());

    // Process all variable declarators (handles cases like: String a, b, c;)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let var_name = extract_identifier(name_node, content);

            // Create unique variable name using byte position to avoid conflicts
            let qualified_var = format!("{}@{}", var_name, name_node.start_byte());

            // Create variable node
            let span = Span::from_bytes(child.start_byte(), child.end_byte());
            let var_id = helper.add_variable(&qualified_var, Some(span));
            scope_tree.attach_node_id(&var_name, name_node.start_byte(), var_id);

            // Create type node
            let type_id = helper.add_class(&resolved_type, None);

            // Create TypeOf edge
            helper.add_typeof_edge(var_id, type_id);
        }
    }
}

fn handle_enhanced_for_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };

    let type_text = extract_type_name(type_node, content);
    let var_name = extract_identifier(name_node, content);
    if type_text.is_empty() || var_name.is_empty() {
        return;
    }

    let resolved_type = ast_graph
        .import_map
        .get(&type_text)
        .cloned()
        .unwrap_or(type_text);

    let qualified_var = format!("{}@{}", var_name, name_node.start_byte());
    let span = Span::from_bytes(name_node.start_byte(), name_node.end_byte());
    let var_id = helper.add_variable(&qualified_var, Some(span));
    scope_tree.attach_node_id(&var_name, body_node.start_byte(), var_id);

    let type_id = helper.add_class(&resolved_type, None);
    helper.add_typeof_edge(var_id, type_id);
}

fn handle_catch_parameter_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
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

    let var_name = extract_identifier(name_node, content);
    if var_name.is_empty() {
        return;
    }

    let qualified_var = format!("{}@{}", var_name, name_node.start_byte());
    let span = Span::from_bytes(param_node.start_byte(), param_node.end_byte());
    let var_id = helper.add_variable(&qualified_var, Some(span));
    scope_tree.attach_node_id(&var_name, name_node.start_byte(), var_id);

    if let Some(type_node) = param_node
        .child_by_field_name("type")
        .or_else(|| first_child_of_kind(param_node, "type_identifier"))
        .or_else(|| first_child_of_kind(param_node, "scoped_type_identifier"))
        .or_else(|| first_child_of_kind(param_node, "generic_type"))
    {
        add_typeof_for_catch_type(type_node, content, ast_graph, helper, var_id);
    }
}

fn add_typeof_for_catch_type(
    type_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    var_id: sqry_core::graph::unified::node::NodeId,
) {
    if type_node.kind() == "union_type" {
        let mut cursor = type_node.walk();
        for child in type_node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "type_identifier" | "scoped_type_identifier" | "generic_type"
            ) {
                let type_text = extract_type_name(child, content);
                if !type_text.is_empty() {
                    let resolved_type = ast_graph
                        .import_map
                        .get(&type_text)
                        .cloned()
                        .unwrap_or(type_text);
                    let type_id = helper.add_class(&resolved_type, None);
                    helper.add_typeof_edge(var_id, type_id);
                }
            }
        }
        return;
    }

    let type_text = extract_type_name(type_node, content);
    if type_text.is_empty() {
        return;
    }
    let resolved_type = ast_graph
        .import_map
        .get(&type_text)
        .cloned()
        .unwrap_or(type_text);
    let type_id = helper.add_class(&resolved_type, None);
    helper.add_typeof_edge(var_id, type_id);
}

fn handle_lambda_parameter_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    use sqry_core::graph::unified::node::NodeKind;

    let Some(params_node) = node.child_by_field_name("parameters") else {
        return;
    };
    let lambda_prefix = format!("lambda@{}", node.start_byte());

    if params_node.kind() == "identifier" {
        let name = extract_identifier(params_node, content);
        if name.is_empty() {
            return;
        }
        let qualified_param = format!("{lambda_prefix}::{name}");
        let span = Span::from_bytes(params_node.start_byte(), params_node.end_byte());
        let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);
        scope_tree.attach_node_id(&name, params_node.start_byte(), param_id);
        return;
    }

    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                let name = extract_identifier(child, content);
                if name.is_empty() {
                    continue;
                }
                let qualified_param = format!("{lambda_prefix}::{name}");
                let span = Span::from_bytes(child.start_byte(), child.end_byte());
                let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);
                scope_tree.attach_node_id(&name, child.start_byte(), param_id);
            }
            "formal_parameter" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let Some(type_node) = child.child_by_field_name("type") else {
                    continue;
                };
                let name = extract_identifier(name_node, content);
                if name.is_empty() {
                    continue;
                }
                let type_text = extract_type_name(type_node, content);
                let resolved_type = ast_graph
                    .import_map
                    .get(&type_text)
                    .cloned()
                    .unwrap_or(type_text);
                let qualified_param = format!("{lambda_prefix}::{name}");
                let span = Span::from_bytes(child.start_byte(), child.end_byte());
                let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);
                scope_tree.attach_node_id(&name, name_node.start_byte(), param_id);
                let type_id = helper.add_class(&resolved_type, None);
                helper.add_typeof_edge(param_id, type_id);
            }
            _ => {}
        }
    }
}

fn handle_try_with_resources_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let Some(resources) = node.child_by_field_name("resources") else {
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
            if type_node.is_none() && value_node.is_none() {
                continue;
            }
            let name = extract_identifier(name_node, content);
            if name.is_empty() {
                continue;
            }

            let qualified_var = format!("{}@{}", name, name_node.start_byte());
            let span = Span::from_bytes(resource.start_byte(), resource.end_byte());
            let var_id = helper.add_variable(&qualified_var, Some(span));
            scope_tree.attach_node_id(&name, name_node.start_byte(), var_id);

            if let Some(type_node) = type_node {
                let type_text = extract_type_name(type_node, content);
                if !type_text.is_empty() {
                    let resolved_type = ast_graph
                        .import_map
                        .get(&type_text)
                        .cloned()
                        .unwrap_or(type_text);
                    let type_id = helper.add_class(&resolved_type, None);
                    helper.add_typeof_edge(var_id, type_id);
                }
            }
        }
    }
}

fn handle_instanceof_pattern_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let mut patterns = Vec::new();
    collect_pattern_declarations(node, &mut patterns);
    for (name_node, type_node) in patterns {
        let name = extract_identifier(name_node, content);
        if name.is_empty() {
            continue;
        }
        let qualified_var = format!("{}@{}", name, name_node.start_byte());
        let span = Span::from_bytes(name_node.start_byte(), name_node.end_byte());
        let var_id = helper.add_variable(&qualified_var, Some(span));
        scope_tree.attach_node_id(&name, name_node.start_byte(), var_id);

        if let Some(type_node) = type_node {
            let type_text = extract_type_name(type_node, content);
            if !type_text.is_empty() {
                let resolved_type = ast_graph
                    .import_map
                    .get(&type_text)
                    .cloned()
                    .unwrap_or(type_text);
                let type_id = helper.add_class(&resolved_type, None);
                helper.add_typeof_edge(var_id, type_id);
            }
        }
    }
}

fn handle_switch_pattern_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let mut patterns = Vec::new();
    collect_pattern_declarations(node, &mut patterns);
    for (name_node, type_node) in patterns {
        let name = extract_identifier(name_node, content);
        if name.is_empty() {
            continue;
        }
        let qualified_var = format!("{}@{}", name, name_node.start_byte());
        let span = Span::from_bytes(name_node.start_byte(), name_node.end_byte());
        let var_id = helper.add_variable(&qualified_var, Some(span));
        scope_tree.attach_node_id(&name, name_node.start_byte(), var_id);

        if let Some(type_node) = type_node {
            let type_text = extract_type_name(type_node, content);
            if !type_text.is_empty() {
                let resolved_type = ast_graph
                    .import_map
                    .get(&type_text)
                    .cloned()
                    .unwrap_or(type_text);
                let type_id = helper.add_class(&resolved_type, None);
                helper.add_typeof_edge(var_id, type_id);
            }
        }
    }
}

fn handle_compact_constructor_parameters(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut JavaScopeTree,
    helper: &mut GraphBuildHelper,
) {
    use sqry_core::graph::unified::node::NodeKind;

    let Some(record_node) = node
        .parent()
        .and_then(|parent| find_record_declaration(parent))
    else {
        return;
    };

    let Some(record_name_node) = record_node.child_by_field_name("name") else {
        return;
    };
    let record_name = extract_identifier(record_name_node, content);
    if record_name.is_empty() {
        return;
    }

    let mut components = Vec::new();
    collect_record_components_nodes(record_node, &mut components);
    for component in components {
        let Some(name_node) = component.child_by_field_name("name") else {
            continue;
        };
        let Some(type_node) = component.child_by_field_name("type") else {
            continue;
        };
        let name = extract_identifier(name_node, content);
        if name.is_empty() {
            continue;
        }

        let type_text = extract_type_name(type_node, content);
        if type_text.is_empty() {
            continue;
        }
        let resolved_type = ast_graph
            .import_map
            .get(&type_text)
            .cloned()
            .unwrap_or(type_text);

        let qualified_param = format!("{record_name}.<init>::{name}");
        let span = Span::from_bytes(component.start_byte(), component.end_byte());
        let param_id = helper.add_node(&qualified_param, Some(span), NodeKind::Parameter);
        scope_tree.attach_node_id(&name, name_node.start_byte(), param_id);

        let type_id = helper.add_class(&resolved_type, None);
        helper.add_typeof_edge(param_id, type_id);
    }
}

fn collect_pattern_declarations<'a>(
    node: Node<'a>,
    output: &mut Vec<(Node<'a>, Option<Node<'a>>)>,
) {
    if node.kind() == "instanceof_expression"
        && !node_has_direct_child_kind(node, "type_pattern")
        && let Some(name_node) = node.child_by_field_name("name")
    {
        let type_node = first_type_like_child(node);
        output.push((name_node, type_node));
    }

    if node.kind() == "type_pattern"
        && let Some((name_node, type_node)) = typed_pattern_parts(node)
    {
        output.push((name_node, type_node));
    }

    if node.kind() == "record_pattern_component"
        && let Some((name_node, type_node)) = typed_pattern_parts(node)
    {
        output.push((name_node, type_node));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_pattern_declarations(child, output);
    }
}

fn node_has_direct_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == kind)
}

fn typed_pattern_parts(node: Node) -> Option<(Node, Option<Node>)> {
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

fn first_type_like_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "type_identifier" | "scoped_type_identifier" | "generic_type"
        ) {
            return Some(child);
        }
    }
    None
}

fn find_record_declaration(node: Node) -> Option<Node> {
    if node.kind() == "record_declaration" {
        return Some(node);
    }
    node.parent().and_then(find_record_declaration)
}

fn collect_record_components_nodes<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for child in parameters.children(&mut cursor) {
            if matches!(child.kind(), "formal_parameter" | "record_component") {
                output.push(child);
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "record_component" {
            output.push(child);
        }
    }
}

/// Process method invocation using `GraphBuildHelper`
fn process_method_call_unified(
    call_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    let Some(caller_context) = ast_graph.find_enclosing(call_node.start_byte()) else {
        return;
    };
    let Ok(callee_name) = extract_method_invocation_name(call_node, content) else {
        return;
    };

    let callee_qualified =
        resolve_callee_qualified(&call_node, content, ast_graph, caller_context, &callee_name);
    let caller_method_id = ensure_caller_method(helper, caller_context);
    let target_method_id = helper.ensure_method(&callee_qualified, None, false, false);

    add_call_edge(helper, caller_method_id, target_method_id, call_node);
}

/// Process constructor call (new expression) using `GraphBuildHelper`
fn process_constructor_call_unified(
    new_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    let Some(caller_context) = ast_graph.find_enclosing(new_node.start_byte()) else {
        return;
    };

    let Some(type_node) = new_node.child_by_field_name("type") else {
        return;
    };

    let class_name = extract_type_name(type_node, content);
    if class_name.is_empty() {
        return;
    }

    let qualified_class = qualify_constructor_class(&class_name, caller_context);
    let constructor_name = format!("{qualified_class}.<init>");

    let caller_method_id = ensure_caller_method(helper, caller_context);
    let target_method_id = helper.ensure_method(&constructor_name, None, false, false);
    add_call_edge(helper, caller_method_id, target_method_id, new_node);
}

fn count_call_arguments(call_node: Node<'_>) -> u8 {
    let Some(args_node) = call_node.child_by_field_name("arguments") else {
        return 255;
    };
    let count = args_node.named_child_count();
    if count <= 254 {
        u8::try_from(count).unwrap_or(u8::MAX)
    } else {
        u8::MAX
    }
}

/// Process import declaration using `GraphBuildHelper`
fn process_import_unified(import_node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    let has_asterisk = import_has_wildcard(import_node);
    let Some(mut imported_name) = extract_import_name(import_node, content) else {
        return;
    };
    if has_asterisk {
        imported_name = format!("{imported_name}.*");
    }

    let module_id = helper.add_module("<module>", None);
    let external_id = helper.add_import(
        &imported_name,
        Some(Span::from_bytes(
            import_node.start_byte(),
            import_node.end_byte(),
        )),
    );

    helper.add_import_edge(module_id, external_id);
}

fn ensure_caller_method(
    helper: &mut GraphBuildHelper,
    caller_context: &MethodContext,
) -> sqry_core::graph::unified::node::NodeId {
    helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    )
}

fn resolve_callee_qualified(
    call_node: &Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    caller_context: &MethodContext,
    callee_name: &str,
) -> String {
    if let Some(object_node) = call_node.child_by_field_name("object") {
        let object_text = extract_node_text(object_node, content);
        return resolve_member_call_target(&object_text, ast_graph, caller_context, callee_name);
    }

    build_member_symbol(
        caller_context.package_name.as_deref(),
        &caller_context.class_stack,
        callee_name,
    )
}

fn resolve_member_call_target(
    object_text: &str,
    ast_graph: &ASTGraph,
    caller_context: &MethodContext,
    callee_name: &str,
) -> String {
    if object_text.contains('.') {
        return format!("{object_text}.{callee_name}");
    }
    if object_text == "this" {
        return build_member_symbol(
            caller_context.package_name.as_deref(),
            &caller_context.class_stack,
            callee_name,
        );
    }

    // Try qualified field lookup (ClassName::fieldName)
    if let Some(class_name) = caller_context.class_stack.last() {
        let qualified_field = format!("{class_name}::{object_text}");
        if let Some((field_type, _is_final, _visibility, _is_static)) =
            ast_graph.field_types.get(&qualified_field)
        {
            return format!("{field_type}.{callee_name}");
        }
    }

    // Fallback: try unqualified field lookup (for backwards compatibility)
    if let Some((field_type, _is_final, _visibility, _is_static)) =
        ast_graph.field_types.get(object_text)
    {
        return format!("{field_type}.{callee_name}");
    }

    if let Some(type_fqn) = ast_graph.import_map.get(object_text) {
        return format!("{type_fqn}.{callee_name}");
    }

    format!("{object_text}.{callee_name}")
}

fn qualify_constructor_class(class_name: &str, caller_context: &MethodContext) -> String {
    if class_name.contains('.') {
        class_name.to_string()
    } else if let Some(pkg) = caller_context.package_name.as_deref() {
        format!("{pkg}.{class_name}")
    } else {
        class_name.to_string()
    }
}

fn add_call_edge(
    helper: &mut GraphBuildHelper,
    caller_method_id: sqry_core::graph::unified::node::NodeId,
    target_method_id: sqry_core::graph::unified::node::NodeId,
    call_node: Node,
) {
    let argument_count = count_call_arguments(call_node);
    let call_span = Span::from_bytes(call_node.start_byte(), call_node.end_byte());
    helper.add_call_edge_full_with_span(
        caller_method_id,
        target_method_id,
        argument_count,
        false,
        vec![call_span],
    );
}

fn import_has_wildcard(import_node: Node) -> bool {
    let mut cursor = import_node.walk();
    import_node
        .children(&mut cursor)
        .any(|child| child.kind() == "asterisk")
}

fn extract_import_name(import_node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = import_node.walk();
    for child in import_node.children(&mut cursor) {
        if child.kind() == "scoped_identifier" || child.kind() == "identifier" {
            return Some(extract_full_identifier(child, content));
        }
    }
    None
}

// ================================
// Inheritance and Interface Implementation
// ================================

/// Process class inheritance (extends clause).
///
/// Handles patterns like:
/// - `class Child extends Parent`
/// - `class Dog extends Animal`
fn process_inheritance(
    class_node: Node,
    content: &[u8],
    package_name: Option<&str>,
    child_class_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // In tree-sitter-java, the superclass is in a "superclass" field
    if let Some(superclass_node) = class_node.child_by_field_name("superclass") {
        // The superclass node typically wraps a type_identifier
        let parent_type_name = extract_type_from_superclass(superclass_node, content);
        if !parent_type_name.is_empty() {
            // Build qualified name for parent (may be in same package or imported)
            let parent_qualified = qualify_type_name(&parent_type_name, package_name);
            let parent_id = helper.add_class(&parent_qualified, None);
            helper.add_inherits_edge(child_class_id, parent_id);
        }
    }
}

/// Process implements clause for classes.
///
/// Handles patterns like:
/// - `class Foo implements IBar`
/// - `class Foo implements IBar, IBaz`
fn process_implements(
    class_node: Node,
    content: &[u8],
    package_name: Option<&str>,
    class_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // In tree-sitter-java, the implements clause may be:
    // - Field named "interfaces" or "super_interfaces"
    // - A child node with kind "super_interfaces"

    // First try field-based access
    let interfaces_node = class_node
        .child_by_field_name("interfaces")
        .or_else(|| class_node.child_by_field_name("super_interfaces"));

    if let Some(node) = interfaces_node {
        extract_interface_types(node, content, package_name, class_id, helper);
        return;
    }

    // Walk children to find super_interfaces node by kind
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        // tree-sitter-java uses "super_interfaces" node kind for implements clause
        if child.kind() == "super_interfaces" {
            extract_interface_types(child, content, package_name, class_id, helper);
            return;
        }
    }
}

/// Process interface inheritance (extends clause for interfaces).
///
/// Handles patterns like:
/// - `interface IChild extends IParent`
/// - `interface IChild extends IParent, IOther`
///
/// tree-sitter-java structure:
/// ```text
/// interface_declaration
///   interface (keyword)
///   identifier "Stream"
///   extends_interfaces  <- not a field, but a child node by kind
///     extends (keyword)
///     type_list
///       type_identifier "Readable"
///       type_identifier "Closeable"
/// ```
fn process_interface_extends(
    interface_node: Node,
    content: &[u8],
    package_name: Option<&str>,
    interface_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // Walk children to find extends_interfaces by node kind
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        if child.kind() == "extends_interfaces" {
            // Found the extends clause - extract parent interfaces using same logic as implements
            extract_parent_interfaces_for_inherits(
                child,
                content,
                package_name,
                interface_id,
                helper,
            );
            return;
        }
    }
}

/// Extract parent interfaces for Inherits edges (interface extends).
/// Reuses the same tree structure as `extract_interface_types` but creates Inherits edges.
fn extract_parent_interfaces_for_inherits(
    extends_node: Node,
    content: &[u8],
    package_name: Option<&str>,
    child_interface_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = extends_node.walk();
    for child in extends_node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" => {
                let type_name = extract_identifier(child, content);
                if !type_name.is_empty() {
                    let parent_qualified = qualify_type_name(&type_name, package_name);
                    let parent_id = helper.add_interface(&parent_qualified, None);
                    helper.add_inherits_edge(child_interface_id, parent_id);
                }
            }
            "type_list" => {
                let mut type_cursor = child.walk();
                for type_child in child.children(&mut type_cursor) {
                    if let Some(type_name) = extract_type_identifier(type_child, content)
                        && !type_name.is_empty()
                    {
                        let parent_qualified = qualify_type_name(&type_name, package_name);
                        let parent_id = helper.add_interface(&parent_qualified, None);
                        helper.add_inherits_edge(child_interface_id, parent_id);
                    }
                }
            }
            "generic_type" | "scoped_type_identifier" => {
                if let Some(type_name) = extract_type_identifier(child, content)
                    && !type_name.is_empty()
                {
                    let parent_qualified = qualify_type_name(&type_name, package_name);
                    let parent_id = helper.add_interface(&parent_qualified, None);
                    helper.add_inherits_edge(child_interface_id, parent_id);
                }
            }
            _ => {}
        }
    }
}

/// Extract type name from superclass node.
fn extract_type_from_superclass(superclass_node: Node, content: &[u8]) -> String {
    // The superclass node may directly be a type_identifier or contain one
    if superclass_node.kind() == "type_identifier" {
        return extract_identifier(superclass_node, content);
    }

    // Look for type_identifier among children
    let mut cursor = superclass_node.walk();
    for child in superclass_node.children(&mut cursor) {
        if let Some(name) = extract_type_identifier(child, content) {
            return name;
        }
    }

    // Fallback: try to extract the entire text
    extract_identifier(superclass_node, content)
}

/// Extract all interface types from a `super_interfaces` or `extends_interfaces` node.
///
/// tree-sitter-java structure:
/// ```text
/// super_interfaces
///   implements (keyword)
///   type_list
///     type_identifier "Runnable"
///     type_identifier "Serializable" (if multiple)
/// ```
fn extract_interface_types(
    interfaces_node: Node,
    content: &[u8],
    package_name: Option<&str>,
    implementor_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // Walk all children to find type_list or direct type identifiers
    let mut cursor = interfaces_node.walk();
    for child in interfaces_node.children(&mut cursor) {
        match child.kind() {
            // Direct type identifiers at this level
            "type_identifier" => {
                let type_name = extract_identifier(child, content);
                if !type_name.is_empty() {
                    let interface_qualified = qualify_type_name(&type_name, package_name);
                    let interface_id = helper.add_interface(&interface_qualified, None);
                    helper.add_implements_edge(implementor_id, interface_id);
                }
            }
            // type_list contains the actual interfaces
            "type_list" => {
                let mut type_cursor = child.walk();
                for type_child in child.children(&mut type_cursor) {
                    if let Some(type_name) = extract_type_identifier(type_child, content)
                        && !type_name.is_empty()
                    {
                        let interface_qualified = qualify_type_name(&type_name, package_name);
                        let interface_id = helper.add_interface(&interface_qualified, None);
                        helper.add_implements_edge(implementor_id, interface_id);
                    }
                }
            }
            // Generic type at this level
            "generic_type" | "scoped_type_identifier" => {
                if let Some(type_name) = extract_type_identifier(child, content)
                    && !type_name.is_empty()
                {
                    let interface_qualified = qualify_type_name(&type_name, package_name);
                    let interface_id = helper.add_interface(&interface_qualified, None);
                    helper.add_implements_edge(implementor_id, interface_id);
                }
            }
            _ => {}
        }
    }
}

/// Extract type identifier from a node (handles `type_identifier` and `generic_type`).
fn extract_type_identifier(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(extract_identifier(node, content)),
        "generic_type" => {
            // For generic types like `List<String>`, extract base type
            if let Some(name_node) = node.child_by_field_name("name") {
                Some(extract_identifier(name_node, content))
            } else {
                // Fallback: get first child if it's a type_identifier
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "type_identifier" {
                        return Some(extract_identifier(child, content));
                    }
                }
                None
            }
        }
        "scoped_type_identifier" => {
            // Fully qualified type like `java.util.List`
            Some(extract_full_identifier(node, content))
        }
        _ => None,
    }
}

/// Qualify a type name with package prefix if not already qualified.
fn qualify_type_name(type_name: &str, package_name: Option<&str>) -> String {
    // If already qualified (contains '.'), keep as-is
    if type_name.contains('.') {
        return type_name.to_string();
    }

    // Otherwise, prefix with package if available
    if let Some(pkg) = package_name {
        format!("{pkg}.{type_name}")
    } else {
        type_name.to_string()
    }
}

// ================================
// Field Type Extraction
// ================================

/// Extract field declarations and imports to build type resolution maps.
/// Returns (`field_types`, `import_map`) where:
/// - `field_types` maps field names to (`type_fqn`, `is_final`) tuples
/// - `import_map` maps simple type names to FQNs (e.g., "`UserService`" -> "com.example.service.UserService")
#[allow(clippy::type_complexity)]
fn extract_field_and_import_types(
    node: Node,
    content: &[u8],
) -> (
    HashMap<String, (String, bool, Option<sqry_core::schema::Visibility>, bool)>,
    HashMap<String, String>,
) {
    // First, build import map (simple name -> FQN)
    let import_map = extract_import_map(node, content);

    let mut field_types = HashMap::new();
    let mut class_stack = Vec::new();
    extract_field_types_recursive(
        node,
        content,
        &import_map,
        &mut field_types,
        &mut class_stack,
    );

    (field_types, import_map)
}

/// Build a map from simple type names to their FQNs based on import declarations
fn extract_import_map(node: Node, content: &[u8]) -> HashMap<String, String> {
    let mut import_map = HashMap::new();
    collect_import_map_recursive(node, content, &mut import_map);
    import_map
}

fn collect_import_map_recursive(
    node: Node,
    content: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    if node.kind() == "import_declaration" {
        // import com.example.service.UserService;
        // Tree structure: (import_declaration (scoped_identifier ...))
        // Try to get the full import path
        let full_path = node.utf8_text(content).unwrap_or("");

        // Parse out the class name from the import statement
        // "import com.example.service.UserService;" -> "com.example.service.UserService"
        if let Some(path_start) = full_path.find("import ") {
            let after_import = &full_path[path_start + 7..].trim();
            if let Some(path_end) = after_import.find(';') {
                let import_path = &after_import[..path_end].trim();

                // Get the simple name (last part)
                if let Some(simple_name) = import_path.rsplit('.').next() {
                    import_map.insert(simple_name.to_string(), (*import_path).to_string());
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_import_map_recursive(child, content, import_map);
    }
}

fn extract_field_types_recursive(
    node: Node,
    content: &[u8],
    import_map: &HashMap<String, String>,
    field_types: &mut HashMap<String, (String, bool, Option<sqry_core::schema::Visibility>, bool)>,
    class_stack: &mut Vec<String>,
) {
    // Handle class/interface/enum declarations - push onto stack
    if matches!(
        node.kind(),
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
    ) && let Some(name_node) = node.child_by_field_name("name")
    {
        let class_name = extract_identifier(name_node, content);
        class_stack.push(class_name);

        // Recurse into body
        if let Some(body_node) = node.child_by_field_name("body") {
            let mut cursor = body_node.walk();
            for child in body_node.children(&mut cursor) {
                extract_field_types_recursive(child, content, import_map, field_types, class_stack);
            }
        }

        // Pop class from stack
        class_stack.pop();
        return; // Already recursed into body
    }

    // field_declaration node structure:
    // (field_declaration
    //   modifiers?: (modifiers) - may contain "final", "static", "public", etc.
    //   type: (type_identifier) @type
    //   declarator: (variable_declarator
    //     name: (identifier) @name))
    if node.kind() == "field_declaration" {
        // Check for modifiers using the helper function
        let is_final = has_modifier(node, "final", content);
        let is_static = has_modifier(node, "static", content);

        // Extract visibility (Java has: public, private, protected, package-private)
        // Map to sqry Visibility: public -> Public, others -> Private
        let visibility = if has_modifier(node, "public", content) {
            Some(sqry_core::schema::Visibility::Public)
        } else {
            // private, protected, or package-private (default) all map to Private
            Some(sqry_core::schema::Visibility::Private)
        };

        // Extract type
        if let Some(type_node) = node.child_by_field_name("type") {
            let type_text = extract_type_name_internal(type_node, content);
            if !type_text.is_empty() {
                // Resolve simple type name to FQN using imports
                let resolved_type = import_map
                    .get(&type_text)
                    .cloned()
                    .unwrap_or(type_text.clone());

                // Extract all declarators (there can be multiple: "String a, b;")
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "variable_declarator"
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let field_name = extract_identifier(name_node, content);

                        // Create qualified field name using full class path (OuterClass::InnerClass::fieldName)
                        // This prevents collisions for fields with same name in different nested classes
                        let qualified_field = if class_stack.is_empty() {
                            field_name
                        } else {
                            let class_path = class_stack.join("::");
                            format!("{class_path}::{field_name}")
                        };

                        field_types.insert(
                            qualified_field,
                            (resolved_type.clone(), is_final, visibility, is_static),
                        );
                    }
                }
            }
        }
    }

    // Recurse into children (for non-class nodes)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_field_types_recursive(child, content, import_map, field_types, class_stack);
    }
}

/// Helper to extract type names for field extraction.
fn extract_type_name_internal(type_node: Node, content: &[u8]) -> String {
    match type_node.kind() {
        "generic_type" => {
            // Extract base type (e.g., "List" from "List<String>")
            if let Some(name_node) = type_node.child_by_field_name("name") {
                extract_identifier(name_node, content)
            } else {
                extract_identifier(type_node, content)
            }
        }
        "scoped_type_identifier" => {
            // e.g., "java.util.List"
            extract_full_identifier(type_node, content)
        }
        _ => extract_identifier(type_node, content),
    }
}

// ================================
// AST Extraction Helpers
// ================================

fn extract_identifier(node: Node, content: &[u8]) -> String {
    node.utf8_text(content).unwrap_or("").to_string()
}

fn extract_node_text(node: Node, content: &[u8]) -> String {
    node.utf8_text(content).unwrap_or("").to_string()
}

fn extract_full_identifier(node: Node, content: &[u8]) -> String {
    node.utf8_text(content).unwrap_or("").to_string()
}

fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|&child| child.kind() == kind)
}

fn extract_method_invocation_name(call_node: Node, content: &[u8]) -> GraphResult<String> {
    // method_invocation has a "name" field
    if let Some(name_node) = call_node.child_by_field_name("name") {
        Ok(extract_identifier(name_node, content))
    } else {
        // Fallback: try to find identifier
        let mut cursor = call_node.walk();
        for child in call_node.children(&mut cursor) {
            if child.kind() == "identifier" {
                return Ok(extract_identifier(child, content));
            }
        }

        Err(GraphBuilderError::ParseError {
            span: Span::from_bytes(call_node.start_byte(), call_node.end_byte()),
            reason: "Method invocation missing name".into(),
        })
    }
}

fn extract_type_name(type_node: Node, content: &[u8]) -> String {
    // Type can be simple identifier or generic type
    match type_node.kind() {
        "generic_type" => {
            // Extract base type (e.g., "List" from "List<String>")
            if let Some(name_node) = type_node.child_by_field_name("name") {
                extract_identifier(name_node, content)
            } else {
                extract_identifier(type_node, content)
            }
        }
        "scoped_type_identifier" => {
            // e.g., "java.util.List"
            extract_full_identifier(type_node, content)
        }
        _ => extract_identifier(type_node, content),
    }
}

/// Extract the full return type including generics (e.g., `Optional<User>`, `List<String>`).
/// Unlike `extract_type_name` which extracts just the base type, this preserves the full type signature.
fn extract_full_return_type(type_node: Node, content: &[u8]) -> String {
    // For the `returns:` predicate, we need the full type representation
    // including generic parameters like Optional<User>, List<Map<String, Integer>>
    type_node.utf8_text(content).unwrap_or("").to_string()
}

fn has_modifier(node: Node, modifier: &str, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier_child in child.children(&mut mod_cursor) {
                if extract_identifier(modifier_child, content) == modifier {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract visibility modifier from a method or constructor node.
/// Returns "public", "private", "protected", or "package-private" (no explicit modifier).
#[allow(clippy::unnecessary_wraps)]
fn extract_visibility(node: Node, content: &[u8]) -> Option<String> {
    if has_modifier(node, "public", content) {
        Some("public".to_string())
    } else if has_modifier(node, "private", content) {
        Some("private".to_string())
    } else if has_modifier(node, "protected", content) {
        Some("protected".to_string())
    } else {
        // No explicit modifier means package-private in Java
        Some("package-private".to_string())
    }
}

// ================================
// Export Detection (public visibility)
// ================================

/// Check if a node has the `public` visibility modifier.
fn is_public(node: Node, content: &[u8]) -> bool {
    has_modifier(node, "public", content)
}

/// Check if a node has the `private` visibility modifier.
fn is_private(node: Node, content: &[u8]) -> bool {
    has_modifier(node, "private", content)
}

/// Create an export edge from the file module to the exported node.
fn export_from_file_module(
    helper: &mut GraphBuildHelper,
    exported: sqry_core::graph::unified::node::NodeId,
) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

/// Process public methods, constructors, and fields within a class body for export edges.
///
/// For interfaces, methods are implicitly public UNLESS explicitly marked private (Java 9+).
/// For classes, only explicitly public members are exported.
fn process_class_member_exports(
    body_node: Node,
    content: &[u8],
    class_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    is_interface: bool,
) {
    for i in 0..body_node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = body_node.child(i as u32) {
            match child.kind() {
                "method_declaration" => {
                    // Interface methods are implicitly public UNLESS explicitly private (Java 9+)
                    // Class methods need explicit public modifier
                    let should_export = if is_interface {
                        // Export interface method if NOT explicitly private
                        !is_private(child, content)
                    } else {
                        // Export class method only if explicitly public
                        is_public(child, content)
                    };

                    if should_export && let Some(name_node) = child.child_by_field_name("name") {
                        let method_name = extract_identifier(name_node, content);
                        let qualified_name = format!("{class_qualified_name}.{method_name}");
                        let span = Span::from_bytes(child.start_byte(), child.end_byte());
                        let is_static = has_modifier(child, "static", content);
                        let method_id =
                            helper.add_method(&qualified_name, Some(span), false, is_static);
                        export_from_file_module(helper, method_id);
                    }
                }
                "constructor_declaration" => {
                    if is_public(child, content) {
                        let qualified_name = format!("{class_qualified_name}.<init>");
                        let span = Span::from_bytes(child.start_byte(), child.end_byte());
                        let method_id =
                            helper.add_method(&qualified_name, Some(span), false, false);
                        export_from_file_module(helper, method_id);
                    }
                }
                "field_declaration" => {
                    if is_public(child, content) {
                        // Extract all field names from the declaration
                        let mut cursor = child.walk();
                        for field_child in child.children(&mut cursor) {
                            if field_child.kind() == "variable_declarator"
                                && let Some(name_node) = field_child.child_by_field_name("name")
                            {
                                let field_name = extract_identifier(name_node, content);
                                let qualified_name = format!("{class_qualified_name}.{field_name}");
                                let span = Span::from_bytes(
                                    field_child.start_byte(),
                                    field_child.end_byte(),
                                );

                                // Use constant for final fields, variable otherwise
                                let is_final = has_modifier(child, "final", content);
                                let field_id = if is_final {
                                    helper.add_constant(&qualified_name, Some(span))
                                } else {
                                    helper.add_variable(&qualified_name, Some(span))
                                };
                                export_from_file_module(helper, field_id);
                            }
                        }
                    }
                }
                "constant_declaration" => {
                    // Constants in interfaces are always public
                    let mut cursor = child.walk();
                    for const_child in child.children(&mut cursor) {
                        if const_child.kind() == "variable_declarator"
                            && let Some(name_node) = const_child.child_by_field_name("name")
                        {
                            let const_name = extract_identifier(name_node, content);
                            let qualified_name = format!("{class_qualified_name}.{const_name}");
                            let span =
                                Span::from_bytes(const_child.start_byte(), const_child.end_byte());
                            let const_id = helper.add_constant(&qualified_name, Some(span));
                            export_from_file_module(helper, const_id);
                        }
                    }
                }
                "enum_constant" => {
                    // Enum constants are always public
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let const_name = extract_identifier(name_node, content);
                        let qualified_name = format!("{class_qualified_name}.{const_name}");
                        let span = Span::from_bytes(child.start_byte(), child.end_byte());
                        let const_id = helper.add_constant(&qualified_name, Some(span));
                        export_from_file_module(helper, const_id);
                    }
                }
                _ => {}
            }
        }
    }
}

// ================================
// FFI Detection (JNI, JNA, Panama)
// ================================

/// Detect FFI-related imports in the file.
/// Returns (`has_jna_import`, `has_panama_import`).
fn detect_ffi_imports(node: Node, content: &[u8]) -> (bool, bool) {
    let mut has_jna = false;
    let mut has_panama = false;

    detect_ffi_imports_recursive(node, content, &mut has_jna, &mut has_panama);

    (has_jna, has_panama)
}

fn detect_ffi_imports_recursive(
    node: Node,
    content: &[u8],
    has_jna: &mut bool,
    has_panama: &mut bool,
) {
    if node.kind() == "import_declaration" {
        let import_text = node.utf8_text(content).unwrap_or("");

        // JNA: com.sun.jna.* or net.java.dev.jna.*
        if import_text.contains("com.sun.jna") || import_text.contains("net.java.dev.jna") {
            *has_jna = true;
        }

        // Panama Foreign Function API: java.lang.foreign.*
        if import_text.contains("java.lang.foreign") {
            *has_panama = true;
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        detect_ffi_imports_recursive(child, content, has_jna, has_panama);
    }
}

/// Find interfaces that extend JNA Library.
/// These interfaces define native function signatures.
fn find_jna_library_interfaces(node: Node, content: &[u8]) -> Vec<String> {
    let mut jna_interfaces = Vec::new();
    find_jna_library_interfaces_recursive(node, content, &mut jna_interfaces);
    jna_interfaces
}

fn find_jna_library_interfaces_recursive(
    node: Node,
    content: &[u8],
    jna_interfaces: &mut Vec<String>,
) {
    if node.kind() == "interface_declaration" {
        // Check if this interface extends Library
        if let Some(name_node) = node.child_by_field_name("name") {
            let interface_name = extract_identifier(name_node, content);

            // Look for extends clause
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "extends_interfaces" {
                    let extends_text = child.utf8_text(content).unwrap_or("");
                    // Check if extends Library or com.sun.jna.Library
                    if extends_text.contains("Library") {
                        jna_interfaces.push(interface_name.clone());
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_jna_library_interfaces_recursive(child, content, jna_interfaces);
    }
}

/// Check if a method call is an FFI call and build the appropriate edge.
/// Returns true if an FFI edge was created.
fn build_ffi_call_edge(
    call_node: Node,
    content: &[u8],
    caller_context: &MethodContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> bool {
    // Extract method name
    let Ok(method_name) = extract_method_invocation_name(call_node, content) else {
        return false;
    };

    // Check for JNA Native.load() call
    if ast_graph.has_jna_import && is_jna_native_load(call_node, content, &method_name) {
        let library_name = extract_jna_library_name(call_node, content);
        build_jna_native_load_edge(caller_context, &library_name, call_node, helper);
        return true;
    }

    // Check for JNA interface method call (calling methods on loaded library)
    if ast_graph.has_jna_import
        && let Some(object_node) = call_node.child_by_field_name("object")
    {
        let object_text = extract_node_text(object_node, content);

        // Try qualified field lookup first (ClassName::fieldName)
        let field_type = if let Some(class_name) = caller_context.class_stack.last() {
            let qualified_field = format!("{class_name}::{object_text}");
            ast_graph
                .field_types
                .get(&qualified_field)
                .or_else(|| ast_graph.field_types.get(&object_text))
        } else {
            ast_graph.field_types.get(&object_text)
        };

        // Check if the object type is a JNA Library interface
        if let Some((type_name, _is_final, _visibility, _is_static)) = field_type {
            let simple_type = simple_type_name(type_name);
            if ast_graph.jna_library_interfaces.contains(&simple_type) {
                build_jna_method_call_edge(
                    caller_context,
                    &simple_type,
                    &method_name,
                    call_node,
                    helper,
                );
                return true;
            }
        }
    }

    // Check for Panama Foreign Function API calls
    if ast_graph.has_panama_import {
        if let Some(object_node) = call_node.child_by_field_name("object") {
            let object_text = extract_node_text(object_node, content);

            // Linker.nativeLinker() and downcallHandle()
            if object_text == "Linker" && method_name == "nativeLinker" {
                build_panama_linker_edge(caller_context, call_node, helper);
                return true;
            }

            // SymbolLookup.libraryLookup()
            if object_text == "SymbolLookup" && method_name == "libraryLookup" {
                let library_name = extract_first_string_arg(call_node, content);
                build_panama_library_lookup_edge(caller_context, &library_name, call_node, helper);
                return true;
            }

            // MethodHandle.invokeExact() on a downcall handle
            if method_name == "invokeExact" || method_name == "invoke" {
                // Check if this might be a foreign function call
                // This is a heuristic - we mark it as FFI if in Panama context
                if is_potential_panama_invoke(call_node, content) {
                    build_panama_invoke_edge(caller_context, &method_name, call_node, helper);
                    return true;
                }
            }
        }

        // Direct Linker.nativeLinker() static call
        if method_name == "nativeLinker" {
            let full_text = call_node.utf8_text(content).unwrap_or("");
            if full_text.contains("Linker") {
                build_panama_linker_edge(caller_context, call_node, helper);
                return true;
            }
        }
    }

    false
}

/// Check if this is a JNA `Native.load()` or `Native.loadLibrary()` call.
fn is_jna_native_load(call_node: Node, content: &[u8], method_name: &str) -> bool {
    if method_name != "load" && method_name != "loadLibrary" {
        return false;
    }

    if let Some(object_node) = call_node.child_by_field_name("object") {
        let object_text = extract_node_text(object_node, content);
        return object_text == "Native" || object_text == "com.sun.jna.Native";
    }

    false
}

/// Extract the library name from JNA `Native.load()` call.
/// Native.load("c", CLibrary.class) -> "c"
fn extract_jna_library_name(call_node: Node, content: &[u8]) -> String {
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut cursor = args_node.walk();
        for child in args_node.children(&mut cursor) {
            if child.kind() == "string_literal" {
                let text = child.utf8_text(content).unwrap_or("\"unknown\"");
                // Remove quotes
                return text.trim_matches('"').to_string();
            }
        }
    }
    "unknown".to_string()
}

/// Extract the first string argument from a method call.
fn extract_first_string_arg(call_node: Node, content: &[u8]) -> String {
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut cursor = args_node.walk();
        for child in args_node.children(&mut cursor) {
            if child.kind() == "string_literal" {
                let text = child.utf8_text(content).unwrap_or("\"unknown\"");
                return text.trim_matches('"').to_string();
            }
        }
    }
    "unknown".to_string()
}

/// Check if this is potentially a Panama foreign function invoke.
fn is_potential_panama_invoke(call_node: Node, content: &[u8]) -> bool {
    // Check if the call is on a MethodHandle that might be a downcall
    if let Some(object_node) = call_node.child_by_field_name("object") {
        let object_text = extract_node_text(object_node, content);
        // Heuristics: variable names often contain "handle", "downcall", or "mh"
        let lower = object_text.to_lowercase();
        return lower.contains("handle")
            || lower.contains("downcall")
            || lower.contains("mh")
            || lower.contains("foreign");
    }
    false
}

/// Get simple type name from potentially qualified name.
fn simple_type_name(type_name: &str) -> String {
    type_name
        .rsplit('.')
        .next()
        .unwrap_or(type_name)
        .to_string()
}

/// Build FFI edge for JNA `Native.load()` call.
fn build_jna_native_load_edge(
    caller_context: &MethodContext,
    library_name: &str,
    call_node: Node,
    helper: &mut GraphBuildHelper,
) {
    let caller_id = helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    );

    let target_name = format!("native::{library_name}");
    let target_id = helper.add_function(
        &target_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Build FFI edge for JNA interface method call.
fn build_jna_method_call_edge(
    caller_context: &MethodContext,
    interface_name: &str,
    method_name: &str,
    call_node: Node,
    helper: &mut GraphBuildHelper,
) {
    let caller_id = helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    );

    let target_name = format!("native::{interface_name}::{method_name}");
    let target_id = helper.add_function(
        &target_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Build FFI edge for Panama `Linker.nativeLinker()` call.
fn build_panama_linker_edge(
    caller_context: &MethodContext,
    call_node: Node,
    helper: &mut GraphBuildHelper,
) {
    let caller_id = helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    );

    let target_name = "native::panama::nativeLinker";
    let target_id = helper.add_function(
        target_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Build FFI edge for Panama `SymbolLookup.libraryLookup()` call.
fn build_panama_library_lookup_edge(
    caller_context: &MethodContext,
    library_name: &str,
    call_node: Node,
    helper: &mut GraphBuildHelper,
) {
    let caller_id = helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    );

    let target_name = format!("native::panama::{library_name}");
    let target_id = helper.add_function(
        &target_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Build FFI edge for Panama `MethodHandle` invoke.
fn build_panama_invoke_edge(
    caller_context: &MethodContext,
    method_name: &str,
    call_node: Node,
    helper: &mut GraphBuildHelper,
) {
    let caller_id = helper.ensure_method(
        caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        caller_context.is_static,
    );

    let target_name = format!("native::panama::{method_name}");
    let target_id = helper.add_function(
        &target_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Build FFI edge for JNI native method declaration.
/// This is called when we encounter a native method declaration.
fn build_jni_native_method_edge(method_context: &MethodContext, helper: &mut GraphBuildHelper) {
    // The method itself is the caller (conceptually, calling into native code)
    let method_id = helper.ensure_method(
        method_context.qualified_name(),
        Some(Span::from_bytes(
            method_context.span.0,
            method_context.span.1,
        )),
        false,
        method_context.is_static,
    );

    // Create a synthetic target representing the native implementation
    // Convention: Java_<package>_<class>_<method>
    let native_target = format!("native::jni::{}", method_context.qualified_name());
    let target_id = helper.add_function(&native_target, None, false, false);

    helper.add_ffi_edge(method_id, target_id, FfiConvention::C);
}

// ================================
// Spring MVC Route Endpoint Detection
// ================================

/// Extract Spring MVC route information from a `method_declaration` node.
///
/// Detects annotations like `@GetMapping("/api/users")`, `@PostMapping("/api/items")`,
/// `@RequestMapping(path="/api/users", method=RequestMethod.GET)`, etc.
///
/// # Returns
///
/// `Some((http_method, path))` if a Spring route annotation is found, `None` otherwise.
/// For example: `Some(("GET", "/api/users"))`.
fn extract_spring_route_info(method_node: Node, content: &[u8]) -> Option<(String, String)> {
    // Navigate to the modifiers child node (contains annotations)
    let mut cursor = method_node.walk();
    let modifiers_node = method_node
        .children(&mut cursor)
        .find(|child| child.kind() == "modifiers")?;

    // Iterate through children of modifiers looking for annotation nodes
    let mut mod_cursor = modifiers_node.walk();
    for annotation_node in modifiers_node.children(&mut mod_cursor) {
        if annotation_node.kind() != "annotation" {
            continue;
        }

        // Extract the annotation name (identifier or scoped_identifier)
        let Some(annotation_name) = extract_annotation_name(annotation_node, content) else {
            continue;
        };

        // Map annotation name to HTTP method
        let http_method: String = match annotation_name.as_str() {
            "GetMapping" => "GET".to_string(),
            "PostMapping" => "POST".to_string(),
            "PutMapping" => "PUT".to_string(),
            "DeleteMapping" => "DELETE".to_string(),
            "PatchMapping" => "PATCH".to_string(),
            "RequestMapping" => {
                // For @RequestMapping, extract method from arguments or default to GET
                extract_request_mapping_method(annotation_node, content)
                    .unwrap_or_else(|| "GET".to_string())
            }
            _ => continue,
        };

        // Extract the path from the annotation arguments
        let Some(path) = extract_annotation_path(annotation_node, content) else {
            continue;
        };

        return Some((http_method, path));
    }

    None
}

/// Extract the simple name from an annotation node.
///
/// Handles both `@GetMapping` (identifier) and `@org.springframework...GetMapping`
/// (`scoped_identifier`) by returning just the final identifier segment.
fn extract_annotation_name(annotation_node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = annotation_node.walk();
    for child in annotation_node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                return Some(extract_identifier(child, content));
            }
            "scoped_identifier" => {
                // For scoped identifiers like org.springframework.web.bind.annotation.GetMapping,
                // extract just the last segment (the actual annotation name)
                let full_text = extract_identifier(child, content);
                return full_text.rsplit('.').next().map(String::from);
            }
            _ => {}
        }
    }
    None
}

/// Extract the path string from a Spring annotation's argument list.
///
/// Handles these patterns:
/// - `@GetMapping("/api/users")` -> `/api/users`
/// - `@RequestMapping(path = "/api/users")` -> `/api/users`
/// - `@RequestMapping(value = "/api/users")` -> `/api/users`
fn extract_annotation_path(annotation_node: Node, content: &[u8]) -> Option<String> {
    // Find the annotation_argument_list child
    let mut cursor = annotation_node.walk();
    let args_node = annotation_node
        .children(&mut cursor)
        .find(|child| child.kind() == "annotation_argument_list")?;

    // Iterate through the argument list children
    let mut args_cursor = args_node.walk();
    for arg_child in args_node.children(&mut args_cursor) {
        match arg_child.kind() {
            // Direct string literal: @GetMapping("/api/users")
            "string_literal" => {
                return extract_string_content(arg_child, content);
            }
            // Named argument: @RequestMapping(path = "/api/users") or value = "/api/users"
            "element_value_pair" => {
                if let Some(path) = extract_path_from_element_value_pair(arg_child, content) {
                    return Some(path);
                }
            }
            _ => {}
        }
    }

    None
}

/// Extract the HTTP method from a `@RequestMapping` annotation's `method` argument.
///
/// Handles patterns like:
/// - `@RequestMapping(method = RequestMethod.POST)` -> `Some("POST")`
/// - `@RequestMapping(method = RequestMethod.GET)` -> `Some("GET")`
///
/// Returns `None` if no method argument is found (caller defaults to GET).
fn extract_request_mapping_method(annotation_node: Node, content: &[u8]) -> Option<String> {
    // Find the annotation_argument_list child
    let mut cursor = annotation_node.walk();
    let args_node = annotation_node
        .children(&mut cursor)
        .find(|child| child.kind() == "annotation_argument_list")?;

    // Look for element_value_pair with key "method"
    let mut args_cursor = args_node.walk();
    for arg_child in args_node.children(&mut args_cursor) {
        if arg_child.kind() != "element_value_pair" {
            continue;
        }

        // Check if the key is "method"
        let Some(key_node) = arg_child.child_by_field_name("key") else {
            continue;
        };
        let key_text = extract_identifier(key_node, content);
        if key_text != "method" {
            continue;
        }

        // Extract the value — expect something like RequestMethod.GET
        let Some(value_node) = arg_child.child_by_field_name("value") else {
            continue;
        };
        let value_text = extract_identifier(value_node, content);

        // Handle RequestMethod.GET, RequestMethod.POST, etc.
        if let Some(method) = value_text.rsplit('.').next() {
            let method_upper = method.to_uppercase();
            if matches!(
                method_upper.as_str(),
                "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS"
            ) {
                return Some(method_upper);
            }
        }
    }

    None
}

/// Extract a path string from an `element_value_pair` node.
///
/// Matches `path = "/api/users"` or `value = "/api/users"` patterns.
fn extract_path_from_element_value_pair(pair_node: Node, content: &[u8]) -> Option<String> {
    let key_node = pair_node.child_by_field_name("key")?;
    let key_text = extract_identifier(key_node, content);

    // Only extract from "path" or "value" keys
    if key_text != "path" && key_text != "value" {
        return None;
    }

    let value_node = pair_node.child_by_field_name("value")?;
    if value_node.kind() == "string_literal" {
        return extract_string_content(value_node, content);
    }

    None
}

/// Extract the class-level `@RequestMapping` path prefix from the enclosing class.
///
/// Walks up the AST from a `method_declaration` to find the enclosing `class_declaration`,
/// then checks for a `@RequestMapping` annotation with a path value.
///
/// # Example
///
/// ```java
/// @RequestMapping("/api")
/// public class UserController {
///     @GetMapping("/users")
///     public List<User> getUsers() { ... }
/// }
/// ```
///
/// For the `getUsers` method node, returns `Some("/api")`.
fn extract_class_request_mapping_path(method_node: Node, content: &[u8]) -> Option<String> {
    // Walk up to find the enclosing class_declaration
    let mut current = method_node.parent()?;
    loop {
        if current.kind() == "class_declaration" {
            break;
        }
        current = current.parent()?;
    }

    // Look for modifiers → @RequestMapping annotation on the class
    let mut cursor = current.walk();
    let modifiers = current
        .children(&mut cursor)
        .find(|child| child.kind() == "modifiers")?;

    let mut mod_cursor = modifiers.walk();
    for annotation in modifiers.children(&mut mod_cursor) {
        if annotation.kind() != "annotation" {
            continue;
        }
        let Some(name) = extract_annotation_name(annotation, content) else {
            continue;
        };
        if name == "RequestMapping" {
            return extract_annotation_path(annotation, content);
        }
    }

    None
}

/// Extract the content of a string literal node, stripping surrounding quotes.
///
/// Handles `"path"` -> `path`.
fn extract_string_content(string_node: Node, content: &[u8]) -> Option<String> {
    let text = string_node.utf8_text(content).ok()?;
    let trimmed = text.trim();

    // Strip surrounding double quotes
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        Some(trimmed[1..trimmed.len() - 1].to_string())
    } else {
        None
    }
}
