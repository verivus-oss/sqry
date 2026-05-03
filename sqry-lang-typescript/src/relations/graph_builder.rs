use std::{collections::HashMap, path::Path};

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::edge::{ExportKind, FfiConvention, HttpMethod};
use sqry_core::graph::unified::{GraphBuildHelper, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use sqry_core::relations::SyntheticNameBuilder;
use tree_sitter::{Node, Tree};

use super::local_scopes;
use super::type_extractor::{extract_all_type_names_from_annotation, extract_type_string};

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// Graph builder for TypeScript files using unified `CodeGraph` architecture.
///
/// TypeScript extends JavaScript with static types but shares the same call expression syntax.
/// This builder reuses the JavaScript AST traversal logic while supporting TypeScript-specific
/// features like type annotations, interfaces, and namespaces.
#[derive(Debug, Clone, Copy)]
pub struct TypeScriptGraphBuilder {
    max_scope_depth: usize,
}

impl Default for TypeScriptGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl TypeScriptGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for TypeScriptGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Build AST graph to extract callable contexts
        let ast_graph =
            ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|err| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason: err,
                }
            })?;

        let mut helper = GraphBuildHelper::new(staging, file, Language::TypeScript);

        // Stage 1: Create function/class/interface nodes from contexts
        for context in ast_graph.contexts() {
            let span = Some(context.span);

            // Determine if this is a method (contains a dot in qualified name)
            let is_method = context.qualified_name.contains('.');

            // Combine class member visibility with export visibility
            let final_visibility = if is_method {
                // For methods, use class member visibility (public/private/protected)
                context.visibility.as_deref()
            } else {
                // For top-level functions, use export visibility
                // Note: context.visibility is None for top-level functions
                // (accessibility_modifier only exists in class members)
                if context.is_exported {
                    Some("public")
                } else {
                    None // Non-exported top-level functions are internal
                }
            };

            if is_method {
                helper.add_method_with_signature(
                    &context.qualified_name,
                    span,
                    context.is_async,
                    false, // is_static - could be enhanced
                    final_visibility,
                    context.return_type.as_deref(),
                );
            } else {
                helper.add_function_with_signature(
                    &context.qualified_name,
                    span,
                    context.is_async,
                    false, // is_unsafe - not applicable to TypeScript
                    final_visibility,
                    context.return_type.as_deref(),
                );
            }
        }

        // Build local scope tree for variable reference resolution
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Stage 2: Walk AST and build edges
        let mut namespace_map = HashMap::new();
        walk_for_edges_with_namespaces(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &mut namespace_map,
            &mut scope_tree,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::TypeScript
    }
}

/// Walk the AST to build call, constructor, import, export, OOP, and FFI edges
/// This function tracks namespaces across the entire file to support namespace augmentation
#[allow(clippy::too_many_lines)] // TS graph builder covers all AST node types
fn walk_for_edges_with_namespaces(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    namespace_map: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    scope_tree: &mut local_scopes::TypeScriptScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        // TypeScript namespace declarations
        // NOTE: tree-sitter-typescript uses "internal_module" for namespace declarations
        // Also matches "module" for the deprecated module keyword
        "namespace_declaration" | "module_declaration" | "internal_module" | "module" => {
            build_namespace_node(node, content, helper, namespace_map, ast_graph, scope_tree)?;
        }
        "call_expression" => {
            // Check for HTTP request patterns (fetch/axios)
            let _ = build_http_request_edge(ast_graph, node, content, helper);
            // Check for route endpoint registrations (Express/Koa/Fastify)
            let _ = detect_route_endpoint(node, content, helper);
            // Check for FFI patterns first (WebAssembly, native addons)
            let is_ffi = build_ffi_call_edge(ast_graph, node, content, helper)?;
            if !is_ffi {
                // Not an FFI call - process as regular call
                if let Some((edge, argument_count, uses_await)) =
                    build_call_edge_with_helper(ast_graph, node, content, helper)?
                {
                    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                    helper.add_call_edge_full_with_span(
                        edge.from,
                        edge.to,
                        argument_count,
                        uses_await,
                        vec![span_from_node(node)],
                    );
                }
            }
        }
        "new_expression" => {
            // Check for WebAssembly constructor patterns
            let is_ffi = build_ffi_new_edge(ast_graph, node, content, helper)?;
            if !is_ffi {
                // Not an FFI constructor - process as regular constructor
                if let Some((edge, argument_count, uses_await)) =
                    build_constructor_edge_with_helper(ast_graph, node, content, helper)?
                {
                    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                    helper.add_call_edge_full_with_span(
                        edge.from,
                        edge.to,
                        argument_count,
                        uses_await,
                        vec![span_from_node(node)],
                    );
                }
            }
        }
        "import_statement" => {
            if let Some(edge) = build_import_edge_with_helper(node, content, helper)? {
                helper.add_import_edge(edge.from, edge.to);
            }
        }
        // Export statements - various TypeScript/ESM export forms
        "export_statement" => {
            build_export_edges(node, content, helper)?;
        }
        // Class inheritance and interface implementation
        "class_declaration" | "class" => {
            build_class_oop_edges(node, content, helper);
            // Resolve the class identifier — fall back to a synthetic anonymous
            // name so qualified-name composition is always well-defined.
            let class_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map_or_else(
                    || SyntheticNameBuilder::from_node(&node, content, "class"),
                    |s| s.trim().to_string(),
                );
            // Process class fields (Property/Constant emission with class-stack
            // qualified names) and constructor-parameter promotion for the
            // `Class.field` short-name surface (REQ:R0001..R0005, R0007).
            if let Some(body) = node.child_by_field_name("body") {
                build_field_type_edges(body, content, helper, Some(&class_name), false)?;
                promote_ctor_parameters_for_class(body, content, helper, &class_name)?;
            }
            // Emit per-type-parameter Type nodes for generic classes
            // (REQ:R0030 / U21 AC-1, AC-2, AC-3, AC-5). Use a
            // namespace-qualified container name so a class declared
            // inside `namespace N { ... }` produces `N.Box.T`, not the
            // ambiguous bare `Box.T` (post-canonicalisation: `N::Box::T`).
            let qualified_class = namespace_qualified_container_name(node, content, &class_name);
            process_type_parameter_declarations(node, content, &qualified_class, helper);
        }
        // Interface inheritance
        "interface_declaration" => {
            build_interface_inheritance_edges(node, content, helper);
            let interface_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string());
            // Process interface properties for TypeOf edges with `Interface.prop`
            // qualified names. Interfaces have no accessibility_modifier, no
            // static, and no constructor parameters — handler short-circuits
            // those branches via `is_interface = true`.
            if let Some(body) = node.child_by_field_name("body")
                && let Some(name) = interface_name.as_deref()
            {
                build_field_type_edges(body, content, helper, Some(name), true)?;
            }
            // Emit per-type-parameter Type nodes for generic interfaces
            // (REQ:R0030 / U21 AC-1, AC-2, AC-3). Use a namespace-qualified
            // container name so an interface declared inside
            // `namespace N { ... }` produces `N.IFoo.T`, not the
            // ambiguous bare `IFoo.T`.
            if let Some(name) = interface_name.as_deref() {
                let qualified_iface = namespace_qualified_container_name(node, content, name);
                process_type_parameter_declarations(node, content, &qualified_iface, helper);
            }
        }
        // Variable declarations (non-export)
        "lexical_declaration" | "variable_declaration" => {
            build_variable_nodes(node, content, helper)?;
        }
        // Type alias declarations - extract referenced types
        "type_alias_declaration" => {
            build_type_alias_edges(node, content, helper)?;
            // Emit per-type-parameter Type nodes for generic type-alias
            // declarations + mapped-type binder Type nodes (REQ:R0030 /
            // U21 AC-1, AC-2, AC-3, AC-4).
            if let Some(name_node) = node.child_by_field_name("name") {
                let alias_name = name_node
                    .utf8_text(content)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !alias_name.is_empty() {
                    // Namespace-qualify so `namespace N { type Wrapper<T> = T[] }`
                    // emits `N.Wrapper.T` (and the mapped-type binder
                    // `N.Wrapper.K`), not the namespace-naked form.
                    let qualified_alias =
                        namespace_qualified_container_name(node, content, &alias_name);
                    process_type_parameter_declarations(node, content, &qualified_alias, helper);
                    process_mapped_type_binders(node, content, &qualified_alias, helper);
                }
            }
        }
        // Function and method declarations - process parameters and return types for type annotations.
        // `method_signature` covers interface method signatures (`get<T>(): T`),
        // which carry a `type_parameters` field but are distinct from
        // `method_definition` (class member with body). See REQ:R0030 / U21.
        "function_declaration"
        | "method_definition"
        | "method_signature"
        | "function_signature"
        | "arrow_function"
        | "function_expression" => {
            // Process parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                build_parameter_type_edges(params, content, helper)?;
            }

            // Process return type
            if node.child_by_field_name("return_type").is_some() {
                // Get function name for return type edge
                if let Some(context) = ast_graph.get_callable_context(node.id()) {
                    build_return_type_edges(node, &context.qualified_name, content, helper)?;
                }
            }

            // Emit per-type-parameter Type nodes for generic
            // function/method declarations (REQ:R0030 / U21 AC-1, AC-2,
            // AC-3, AC-5). Only declared functions/methods carry a
            // user-visible name suitable for qualified-name composition;
            // anonymous arrow_function / function_expression nodes are
            // skipped.
            //
            // `method_signature` (interface methods like `wrap<T>(): T`)
            // does not get a `CallContext` from `walk_ast` (interfaces
            // don't push class scopes there), so resolve its qualified
            // name explicitly via `compute_callable_qname` which walks
            // ancestors to find the enclosing interface (or class) and
            // namespace stack.
            if matches!(
                node.kind(),
                "function_declaration"
                    | "method_definition"
                    | "method_signature"
                    | "function_signature"
            ) {
                let parent_qname_owned = ast_graph
                    .get_callable_context(node.id())
                    .map(|ctx| ctx.qualified_name.clone())
                    .or_else(|| compute_callable_qname(node, content));
                if let Some(parent_qname) = parent_qname_owned
                    && !parent_qname.is_empty()
                    && !parent_qname.starts_with("<anon:")
                {
                    process_type_parameter_declarations(node, content, &parent_qname, helper);
                }
            }
        }
        "enum_declaration" => {
            // Emit the enum container node and qualified-name members.
            // For `const enum`, members are emitted as Constant; otherwise
            // Property (per AC-2 of the U08 acceptance set, FR-13/FR-24).
            build_enum_and_members(node, content, helper)?;
        }
        "identifier" => {
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_edges_with_namespaces(
            child,
            content,
            ast_graph,
            helper,
            namespace_map,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Build namespace node and track namespace declarations/augmentations
fn build_namespace_node(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    namespace_map: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    ast_graph: &ASTGraph,
    scope_tree: &mut local_scopes::TypeScriptScopeTree,
) -> GraphResult<()> {
    // Extract namespace name
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(());
    };

    let namespace_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "failed to read namespace name".to_string(),
        })?
        .trim()
        .to_string();

    if namespace_name.is_empty() {
        return Ok(());
    }

    // Check if this namespace already exists (augmentation case)
    let namespace_id = if let Some(&existing_id) = namespace_map.get(&namespace_name) {
        // This is an augmentation - reuse existing namespace NodeId
        existing_id
    } else {
        // First declaration - create new namespace Module node
        let ns_id = helper.add_module(&namespace_name, Some(span_from_node(node)));
        namespace_map.insert(namespace_name.clone(), ns_id);
        ns_id
    };

    // Process namespace body and link members
    if let Some(body) = node.child_by_field_name("body") {
        link_namespace_members(
            body,
            content,
            helper,
            namespace_id,
            namespace_map,
            ast_graph,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Link namespace members to the namespace via Contains edges
fn link_namespace_members(
    body_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    namespace_id: sqry_core::graph::unified::NodeId,
    namespace_map: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    ast_graph: &ASTGraph,
    scope_tree: &mut local_scopes::TypeScriptScopeTree,
) -> GraphResult<()> {
    let mut cursor = body_node.walk();

    for child in body_node.children(&mut cursor) {
        // Skip braces and other tokens
        if matches!(child.kind(), "{" | "}") {
            continue;
        }

        // Handle nested namespaces separately - they need full recursion
        if matches!(
            child.kind(),
            "namespace_declaration" | "module_declaration" | "internal_module" | "module"
        ) {
            // Recurse into nested namespace with full walk_for_edges logic
            walk_for_edges_with_namespaces(
                child,
                content,
                ast_graph,
                helper,
                namespace_map,
                scope_tree,
            )?;
            continue;
        }

        // Handle export statements - unwrap to find the actual declaration
        if child.kind() == "export_statement" {
            // Find the actual declaration inside the export
            let mut export_cursor = child.walk();
            for export_child in child.children(&mut export_cursor) {
                // Skip the export keyword
                if export_child.kind() == "export" {
                    continue;
                }
                let member_id = process_member_node(export_child, content, helper)?;
                if let Some(id) = member_id {
                    helper.add_contains_edge(namespace_id, id);
                }
            }
            continue;
        }

        // Process member declarations directly
        let member_id_opt = process_member_node(child, content, helper)?;

        // Create Contains edge if we found a member
        if let Some(member_id) = member_id_opt {
            helper.add_contains_edge(namespace_id, member_id);
        }
    }

    Ok(())
}

/// Process a single member node and return its `NodeId` if it's a valid member
fn process_member_node(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<sqry_core::graph::unified::NodeId>> {
    let member_id_opt = match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "failed to read function name".to_string(),
                    })?
                    .trim()
                    .to_string();
                Some(helper.add_function(&name, Some(span_from_node(node)), false, false))
            } else {
                None
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "failed to read class name".to_string(),
                    })?
                    .trim()
                    .to_string();
                Some(helper.add_class(&name, Some(span_from_node(node))))
            } else {
                None
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "failed to read interface name".to_string(),
                    })?
                    .trim()
                    .to_string();
                Some(helper.add_interface(&name, Some(span_from_node(node))))
            } else {
                None
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "failed to read type alias name".to_string(),
                    })?
                    .trim()
                    .to_string();
                Some(helper.add_type(&name, Some(span_from_node(node))))
            } else {
                None
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(node),
                        reason: "failed to read enum name".to_string(),
                    })?
                    .trim()
                    .to_string();
                Some(helper.add_enum(&name, Some(span_from_node(node))))
            } else {
                None
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // Handle variable/constant declarations
            extract_first_variable_name(node, content, helper)
        }
        _ => None,
    };

    Ok(member_id_opt)
}

/// Extract the first variable name from a lexical/variable declaration
fn extract_first_variable_name(
    decl_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Option<sqry_core::graph::unified::NodeId> {
    let mut cursor = decl_node.walk();

    for child in decl_node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            let Some(name_node) = child.child_by_field_name("name") else {
                break;
            };
            let Ok(name) = name_node.utf8_text(content) else {
                break;
            };
            let name = name.trim().to_string();

            // Check if it's a function (arrow or function expression)
            let is_function = child
                .child_by_field_name("value")
                .is_some_and(|v| matches!(v.kind(), "arrow_function" | "function"));

            if is_function {
                return Some(helper.add_function(&name, Some(span_from_node(child)), false, false));
            }
            return Some(helper.add_variable(&name, Some(span_from_node(child))));
        }
    }

    None
}

/// Build a call edge using `GraphBuildHelper` (returns (from, to) `NodeIds`)
fn build_call_edge_with_helper(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<(EdgeWithIds, usize, bool)>> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Synthetic module context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: Span::default(),
            is_async: false,
            return_type: None,
            visibility: None,
            is_exported: false,
        };
        &module_context
    };

    let Some(callee_expr) = call_node.child_by_field_name("function") else {
        return Ok(None);
    };

    let raw_callee_text = callee_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim()
        .to_string();

    // Normalize optional chain syntax (user?.getName -> user.getName)
    let callee_text = if raw_callee_text.contains("?.") {
        raw_callee_text
            .replace("?.", ".")
            .trim()
            .trim_end_matches('.')
            .to_string()
    } else {
        raw_callee_text
    };

    if callee_text.is_empty() {
        return Ok(None);
    }

    let callee_simple = simple_name(&callee_text);
    if callee_simple.is_empty() {
        return Ok(None);
    }

    // Derive qualified callee name with proper this/super resolution
    let caller_qname = call_context.qualified_name();
    let target_qname = if let Some(method_name) = callee_text.strip_prefix("this.") {
        // Resolve this.method() to ClassName.method()
        if let Some(scope_idx) = caller_qname.rfind('.') {
            let class_name = &caller_qname[..scope_idx];
            format!("{}.{}", class_name, simple_name(method_name))
        } else {
            callee_text.clone()
        }
    } else if callee_text.starts_with("super.") {
        // super.method() - keep as-is for now
        callee_text.clone()
    } else if callee_text.contains('.') {
        // Other qualified names (obj.method, module.func, etc.)
        callee_text.clone()
    } else {
        // Top-level unqualified name
        callee_simple.to_string()
    };

    let call_site_span = span_from_node(call_node);
    let source_id = helper.ensure_callee(&caller_qname, call_site_span, CalleeKindHint::Function);
    let target_id = helper.ensure_callee(&target_qname, call_site_span, CalleeKindHint::Function);

    let argument_count = count_arguments(call_node);
    let uses_await = is_awaited_call(call_node);

    // Create a minimal edge representation for returning
    let edge = EdgeWithIds {
        from: source_id,
        to: target_id,
    };

    Ok(Some((edge, argument_count, uses_await)))
}

/// Build a constructor edge using `GraphBuildHelper`
fn build_constructor_edge_with_helper(
    ast_graph: &ASTGraph,
    new_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<(EdgeWithIds, usize, bool)>> {
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(new_node.id()) {
        ctx
    } else {
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: Span::default(),
            is_async: false,
            return_type: None,
            visibility: None,
            is_exported: false,
        };
        &module_context
    };

    let Some(constructor_expr) = new_node.child_by_field_name("constructor") else {
        return Ok(None);
    };

    let constructor_text = constructor_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(new_node),
            reason: "failed to read constructor expression".to_string(),
        })?
        .trim()
        .to_string();

    if constructor_text.is_empty() {
        return Ok(None);
    }

    let constructor_simple = simple_name(&constructor_text);
    let new_site_span = span_from_node(new_node);
    let source_id = helper.ensure_callee(
        &call_context.qualified_name(),
        new_site_span,
        CalleeKindHint::Function,
    );
    let target_id =
        helper.ensure_callee(constructor_simple, new_site_span, CalleeKindHint::Function);

    let argument_count = count_arguments(new_node);
    let uses_await = is_awaited_call(new_node);

    let edge = EdgeWithIds {
        from: source_id,
        to: target_id,
    };

    Ok(Some((edge, argument_count, uses_await)))
}

/// Build import edges using `GraphBuildHelper`
///
/// Creates edges for:
/// - Module-level import (file -> module)
/// - Individual import specifiers (for `imports:X` predicate support)
///
/// Handles all import forms including:
/// - `import { a, b } from 'module'` - named imports
/// - `import type { T } from 'module'` - type-only imports
/// - `import { type T, value } from 'module'` - mixed type/value imports
/// - `import * as ns from 'module'` - namespace imports
/// - `import def from 'module'` - default imports
fn build_import_edge_with_helper(
    import_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<EdgeWithIds>> {
    // Get the source module path from the import statement
    let Some(source_node) = import_node.child_by_field_name("source") else {
        return Ok(None);
    };

    let source_text = source_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(import_node),
            reason: "failed to read import source".to_string(),
        })?
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();

    if source_text.is_empty() {
        return Ok(None);
    }

    // Resolve the import path to a canonical module identifier
    let file_path = helper.file_path().to_string();
    let resolved_path =
        sqry_core::graph::resolve_import_path(std::path::Path::new(&file_path), &source_text)?;

    // Create module nodes for current file and imported module
    let from_id = helper.add_module(&file_path, None);
    let to_id = helper.add_import(&resolved_path, Some(span_from_node(import_node)));

    // Extract individual imported names and create Import nodes for each
    // This enables `imports:SymbolName` queries to find the importing file
    extract_import_specifiers(import_node, content, helper);

    let edge = EdgeWithIds {
        from: from_id,
        to: to_id,
    };

    Ok(Some(edge))
}

/// Extract individual import specifiers and create Import nodes.
///
/// Walks the import clause to find named imports, default imports,
/// and namespace imports. Creates an Import node for each imported name
/// to support `imports:X` queries.
fn extract_import_specifiers(import_node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    let span = span_from_node(import_node);
    let mut cursor = import_node.walk();

    for child in import_node.children(&mut cursor) {
        match child.kind() {
            // Named imports: import { a, b, c } from 'module'
            // Also handles: import type { T } or import { type T, value }
            "import_clause" => {
                extract_from_import_clause(child, content, span, helper);
            }
            "named_imports" => {
                extract_from_named_imports(child, content, span, helper);
            }
            _ => {}
        }
    }
}

/// Extract imports from an `import_clause` node.
fn extract_from_import_clause(
    clause_node: Node<'_>,
    content: &[u8],
    span: Span,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = clause_node.walk();
    for child in clause_node.children(&mut cursor) {
        match child.kind() {
            // Default import: import foo from 'module'
            "identifier" => {
                if let Ok(name) = child.utf8_text(content) {
                    helper.add_import(name, Some(span));
                }
            }
            // Named imports: import { a, b } from 'module'
            "named_imports" => {
                extract_from_named_imports(child, content, span, helper);
            }
            // Namespace import: import * as ns from 'module'
            "namespace_import" => {
                // Extract the alias name (ns in `import * as ns`)
                if let Some(alias) = child.child_by_field_name("alias")
                    && let Ok(name) = alias.utf8_text(content)
                {
                    helper.add_import(name, Some(span));
                }
            }
            _ => {}
        }
    }
}

/// Extract imports from a `named_imports` node (the `{ a, b, c }` part).
fn extract_from_named_imports(
    named_imports: Node<'_>,
    content: &[u8],
    span: Span,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = named_imports.walk();
    for child in named_imports.children(&mut cursor) {
        if child.kind() == "import_specifier" {
            // import_specifier can be:
            // - just an identifier: import { foo }
            // - with alias: import { foo as bar }
            // - type-only: import { type Foo }
            // We want the imported name (original), not the alias
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(content) {
                    // Skip "type" keyword if it appears as a name
                    if name != "type" {
                        helper.add_import(name, Some(span));
                    }
                }
            } else {
                // Fallback: get first identifier child
                let mut spec_cursor = child.walk();
                for spec_child in child.children(&mut spec_cursor) {
                    if spec_child.kind() == "identifier"
                        && let Ok(name) = spec_child.utf8_text(content)
                        && name != "type"
                    {
                        helper.add_import(name, Some(span));
                        break;
                    }
                }
            }
        }
    }
}

/// Build export edges for TypeScript export statements
///
/// Handles all ESM export forms:
/// - `export default foo` - default export
/// - `export { name }` - named export
/// - `export { name as alias }` - aliased export
/// - `export * from 'module'` - wildcard re-export
/// - `export { name } from 'module'` - named re-export
/// - `export type { T }` - type export (TypeScript specific)
/// - `export interface/type/enum/class/function` - declaration exports
fn build_export_edges(
    export_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let file_path = helper.file_path().to_string();
    let module_id = helper.add_module(&file_path, None);

    // Check if this is a re-export (has source)
    let source_node = export_node.child_by_field_name("source");
    let is_reexport = source_node.is_some();

    let mut cursor = export_node.walk();
    for child in export_node.children(&mut cursor) {
        match child.kind() {
            // export default expression
            "default" => {
                // Look for the exported value after "default"
                if let Some(value_node) = get_default_export_value(export_node, content) {
                    let export_name = value_node
                        .utf8_text(content)
                        .ok()
                        .map_or_else(|| "default".to_string(), |s| s.trim().to_string());

                    let exported_id = helper.add_function(&export_name, None, false, false);
                    helper.add_export_edge_full(module_id, exported_id, ExportKind::Default, None);
                }
            }
            // export { name, name as alias, ... } or export { name } from "module"
            "export_clause" => {
                build_export_clause_edges(child, content, helper, module_id, is_reexport)?;
            }
            // export * from "module" or export * as ns from "module"
            "namespace_export" | "*" => {
                // Check for namespace alias: export * as ns from "module"
                let alias = get_namespace_export_alias(export_node, content);
                let kind = if alias.is_some() {
                    ExportKind::Namespace
                } else {
                    ExportKind::Reexport
                };

                // Create a node for the re-exported module
                if let Some(source) = source_node {
                    let source_text = source
                        .utf8_text(content)
                        .ok()
                        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                        .unwrap_or_default();

                    if !source_text.is_empty() {
                        let source_id =
                            helper.add_module(&source_text, Some(span_from_node(export_node)));
                        helper.add_export_edge_full(module_id, source_id, kind, alias.as_deref());
                    }
                }
            }
            // export function/class/interface/type/enum declarations
            "function_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(content)
                        .map_err(|_| GraphBuilderError::ParseError {
                            span: span_from_node(child),
                            reason: "failed to read exported declaration name".to_string(),
                        })?
                        .trim()
                        .to_string();

                    let exported_id = match child.kind() {
                        "function_declaration" => {
                            helper.add_function(&name, Some(span_from_node(child)), false, false)
                        }
                        "class_declaration" => helper.add_class(&name, Some(span_from_node(child))),
                        "interface_declaration" => {
                            helper.add_interface(&name, Some(span_from_node(child)))
                        }
                        "type_alias_declaration" => {
                            helper.add_type(&name, Some(span_from_node(child)))
                        }
                        "enum_declaration" => helper.add_enum(&name, Some(span_from_node(child))),
                        _ => helper.add_function(&name, Some(span_from_node(child)), false, false),
                    };

                    helper.add_export_edge_full(module_id, exported_id, ExportKind::Direct, None);
                }
            }
            // export const/let/var declarations
            "lexical_declaration" | "variable_declaration" => {
                build_variable_export_edges(child, content, helper, module_id)?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Build edges for export clause: `export { a, b as c }` or `export { a } from "mod"`
fn build_export_clause_edges(
    clause_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    is_reexport: bool,
) -> GraphResult<()> {
    let mut cursor = clause_node.walk();

    for child in clause_node.children(&mut cursor) {
        if child.kind() == "export_specifier" {
            // Get the local name (what's being exported)
            let name_node = child.child_by_field_name("name");
            let alias_node = child.child_by_field_name("alias");

            // Check if this is a type export: export type { T }
            // The "type" keyword appears as a sibling node before the export_clause in type exports
            let is_type_export = is_type_export_specifier(child, clause_node);

            if let Some(name) = name_node {
                let name_text = name
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(child),
                        reason: "failed to read export specifier name".to_string(),
                    })?
                    .trim()
                    .to_string();

                // Get alias if present
                let alias = alias_node
                    .and_then(|a| a.utf8_text(content).ok())
                    .map(|s| s.trim().to_string());

                // Determine export kind
                let kind = if is_reexport {
                    ExportKind::Reexport
                } else {
                    ExportKind::Direct
                };

                // Create the exported symbol node
                // For type exports, we use add_type; for value exports, we use add_function
                let exported_id = if is_type_export {
                    helper.add_type(&name_text, None)
                } else {
                    helper.add_function(&name_text, None, false, false)
                };

                helper.add_export_edge_full(module_id, exported_id, kind, alias.as_deref());
            }
        }
    }

    Ok(())
}

/// Check if an export specifier is a type export
fn is_type_export_specifier(specifier: Node<'_>, clause: Node<'_>) -> bool {
    // Check for inline type modifier: export { type Foo }
    let mut cursor = specifier.walk();
    for child in specifier.children(&mut cursor) {
        if child.kind() == "type" {
            return true;
        }
    }

    // Check for clause-level type modifier: export type { Foo }
    // The "type" keyword appears before the export_clause
    if let Some(parent) = clause.parent() {
        let mut parent_cursor = parent.walk();
        let mut found_type = false;
        for child in parent.children(&mut parent_cursor) {
            if child.kind() == "type" {
                found_type = true;
            }
            if child.id() == clause.id() && found_type {
                return true;
            }
        }
    }

    false
}

/// Get the value being default-exported
fn get_default_export_value<'a>(export_node: Node<'a>, content: &[u8]) -> Option<Node<'a>> {
    let mut cursor = export_node.walk();
    let mut found_default = false;

    for child in export_node.children(&mut cursor) {
        if child.kind() == "default" {
            found_default = true;
            continue;
        }

        if found_default {
            // Skip type annotations and other non-value nodes
            match child.kind() {
                "identifier"
                | "class_declaration"
                | "class"
                | "function_declaration"
                | "function"
                | "arrow_function"
                | "object"
                | "array"
                | "call_expression"
                | "new_expression"
                | "number"
                | "string"
                | "true"
                | "false"
                | "null" => {
                    return Some(child);
                }
                // For complex expressions, try to get the identifier
                _ if child.utf8_text(content).is_ok() => {
                    return Some(child);
                }
                _ => {}
            }
        }
    }

    None
}

/// Get namespace export alias: `export * as ns from "mod"` -> Some("ns")
fn get_namespace_export_alias(export_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = export_node.walk();
    let mut found_star = false;

    for child in export_node.children(&mut cursor) {
        if child.kind() == "*" || child.kind() == "namespace_export" {
            found_star = true;
            continue;
        }

        if found_star && child.kind() == "as" {
            continue;
        }

        if found_star && child.kind() == "identifier" {
            return child.utf8_text(content).ok().map(|s| s.trim().to_string());
        }

        // Handle namespace_export with nested structure
        if child.kind() == "namespace_export" {
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                if inner.kind() == "identifier" {
                    return inner.utf8_text(content).ok().map(|s| s.trim().to_string());
                }
            }
        }
    }

    None
}

/// Build export edges for variable declarations: `export const a = 1, b = 2;`
fn build_variable_export_edges(
    decl_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = decl_node.walk();

    for child in decl_node.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = name_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: span_from_node(child),
                    reason: "failed to read exported variable name".to_string(),
                })?
                .trim()
                .to_string();

            // Check if the value is an arrow function or function expression
            let is_function = child
                .child_by_field_name("value")
                .is_some_and(|v| matches!(v.kind(), "arrow_function" | "function"));

            let exported_id = if is_function {
                helper.add_function(&name, Some(span_from_node(child)), false, false)
            } else {
                helper.add_variable(&name, Some(span_from_node(child)))
            };

            helper.add_export_edge_full(module_id, exported_id, ExportKind::Direct, None);
        }
    }

    Ok(())
}

/// Build nodes for variable declarations that are not exports.
/// Build `TypeOf` and Reference edges for function/method parameters with type annotations.
///
/// This processes parameter nodes and creates:
/// - Type nodes for parameter type annotations
/// - `TypeOf` edges from parameter to type with Parameter context and index
/// - Reference edges from parameter to all types (including union/intersection branches)
fn build_parameter_type_edges(
    parameters_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut cursor = parameters_node.walk();
    let mut param_index: u16 = 0;

    for child in parameters_node.children(&mut cursor) {
        // Handle different parameter node types
        match child.kind() {
            "required_parameter" | "optional_parameter" => {
                // Get parameter name
                let name_node = child.child_by_field_name("pattern").or_else(|| {
                    // For simple parameters, name might be direct child
                    child
                        .named_children(&mut child.walk())
                        .find(|n| matches!(n.kind(), "identifier" | "this"))
                });

                let Some(name_node) = name_node else {
                    continue;
                };

                let name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(child),
                        reason: "failed to read parameter name".to_string(),
                    })?
                    .trim()
                    .to_string();

                // Create parameter variable node
                let param_id = helper.add_variable(&name, Some(span_from_node(child)));

                // Check for type annotation
                if let Some(type_node) = child.child_by_field_name("type") {
                    // Extract full type string for TypeOf edge
                    if let Some(type_text) = extract_type_string(type_node, content) {
                        let type_id = helper.add_type(&type_text, Some(span_from_node(type_node)));
                        helper.add_typeof_edge_with_context(
                            param_id,
                            type_id,
                            Some(TypeOfContext::Parameter),
                            Some(param_index),
                            Some(&name),
                        );
                    }

                    // Extract all types for Reference edges
                    let all_types = extract_all_type_names_from_annotation(type_node, content);
                    for type_name in all_types {
                        let type_id = helper.add_type(&type_name, Some(span_from_node(type_node)));
                        helper.add_reference_edge(param_id, type_id);
                    }
                }

                param_index += 1;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Build Reference edges for type aliases
/// Example: type Foo<T> = Bar | Baz creates Reference edges: Foo → Bar, Foo → Baz
fn build_type_alias_edges(
    type_alias_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Get the type alias name
    let Some(name_node) = type_alias_node.child_by_field_name("name") else {
        return Ok(());
    };

    let name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(type_alias_node),
            reason: "failed to read type alias name".to_string(),
        })?
        .trim()
        .to_string();

    // Get the type alias node ID (should already exist from walk_ast)
    let type_alias_id = helper.add_type(&name, Some(span_from_node(type_alias_node)));

    // Extract all referenced types from the value (right-hand side)
    if let Some(value_node) = type_alias_node.child_by_field_name("value") {
        let referenced_types = extract_all_type_names_from_annotation(value_node, content);

        // Create Reference edges for all referenced types
        for type_name in referenced_types {
            let referenced_type_id = helper.add_type(&type_name, Some(span_from_node(value_node)));
            helper.add_reference_edge(type_alias_id, referenced_type_id);
        }
    }

    Ok(())
}

/// Build `TypeOf` and Reference edges for function return types.
///
/// This processes function/method nodes and creates:
/// - `TypeOf` edges from function to return type with Return context (index 0)
/// - Reference edges from function to all types in return type annotation
///
/// Closures and synthetic-named arrow functions (e.g. `<anon:arrow@42>`) are
/// intentionally skipped — their return types lack a stable byte-exact spelling
/// users can search for via `returns:Foo`. Methods (qualified names containing
/// `.`) are routed through `add_method` so the return-type edge is anchored at
/// the same Method node that Stage 1 created.
#[allow(clippy::unnecessary_wraps)]
fn build_return_type_edges(
    function_node: Node<'_>,
    function_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find return type annotation
    let Some(return_type_node) = function_node.child_by_field_name("return_type") else {
        return Ok(());
    };

    // Skip emission for synthetic-named anonymous functions/arrows. They have
    // no stable name a user could query against.
    if function_name.is_empty() || function_name.starts_with("<anon:") {
        return Ok(());
    }

    // Get or create the matching node. Methods (qualified names with `.`) must
    // resolve to the same Method node that Stage 1 created via
    // `add_method_with_signature`; standalone functions resolve to a Function
    // node. Mismatching the kind would create a fresh dangling node and the
    // return edge would never connect to the user-visible function.
    let function_id = if function_name.contains('.') {
        helper.add_method(
            function_name,
            Some(span_from_node(function_node)),
            false,
            false,
        )
    } else {
        helper.add_function(
            function_name,
            Some(span_from_node(function_node)),
            false,
            false,
        )
    };

    // Extract full type string for TypeOf edge
    if let Some(type_text) = extract_type_string(return_type_node, content) {
        let type_id = helper.add_type(&type_text, Some(span_from_node(return_type_node)));
        helper.add_typeof_edge_with_context(
            function_id,
            type_id,
            Some(TypeOfContext::Return),
            Some(0),
            Some(function_name),
        );
    }

    // Extract all types for Reference edges
    let all_types = extract_all_type_names_from_annotation(return_type_node, content);
    for type_name in all_types {
        let type_id = helper.add_type(&type_name, Some(span_from_node(return_type_node)));
        helper.add_reference_edge(function_id, type_id);
    }

    Ok(())
}

/// Build `TypeOf` and Reference edges for class/interface fields with type annotations.
///
/// This processes field/property nodes and creates:
/// - `TypeOf` edges from field to type with Field context
/// - Reference edges from field to all types in type annotation
fn build_field_type_edges(
    body_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    parent_name: Option<&str>,
    is_interface: bool,
) -> GraphResult<()> {
    let mut cursor = body_node.walk();

    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "public_field_definition" | "property_signature" | "field_definition" => {
                emit_field_node(child, content, helper, parent_name, is_interface)?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Emit a single class/interface field node.
///
/// Selects `Property` vs `Constant` based on the `readonly` modifier (per
/// AC-2 / FR-13). Constructs the qualified name as `Parent.field` from the
/// class-stack-tracked `parent_name` (canonicalized to `Parent::field` by
/// the helper layer for TypeScript). Visibility is sourced from
/// `accessibility_modifier` or — when the identifier is a
/// `private_property_identifier` (`#name`) — forced to `"private"`.
///
/// Interfaces have no `accessibility_modifier`, no `static`, and no
/// constructor parameters; the handler short-circuits those branches when
/// `is_interface = true`.
fn emit_field_node(
    field_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    parent_name: Option<&str>,
    is_interface: bool,
) -> GraphResult<()> {
    let Some(name_node) = field_node.child_by_field_name("name") else {
        return Ok(());
    };

    let raw_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(field_node),
            reason: "failed to read field name".to_string(),
        })?
        .trim()
        .to_string();

    if raw_name.is_empty() {
        return Ok(());
    }

    let is_hash_private = name_node.kind() == "private_property_identifier";

    // Scan modifier tokens. Tree-sitter-typescript surfaces `static`,
    // `readonly`, and `accessibility_modifier` as direct children of
    // `public_field_definition`; interfaces (`property_signature`) only
    // surface `readonly`.
    let mut is_static = false;
    let mut is_readonly = false;
    let mut explicit_visibility: Option<String> = None;
    let mut mod_cursor = field_node.walk();
    for modifier in field_node.children(&mut mod_cursor) {
        match modifier.kind() {
            "static" if !is_interface => is_static = true,
            "readonly" => is_readonly = true,
            "accessibility_modifier" if !is_interface => {
                if let Ok(text) = modifier.utf8_text(content) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        explicit_visibility = Some(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // `#name` short-circuits visibility to "private" regardless of any
    // accessibility_modifier (TypeScript grammar does not co-emit one with
    // a private_property_identifier, but be defensive).
    let visibility: Option<&str> = if is_hash_private {
        Some("private")
    } else {
        explicit_visibility.as_deref()
    };

    let qualified_name = match parent_name {
        Some(parent) => format!("{parent}.{raw_name}"),
        None => raw_name.clone(),
    };

    let span = Some(span_from_node(field_node));

    let field_id = if is_readonly {
        helper.add_constant_with_static_and_visibility(&qualified_name, span, is_static, visibility)
    } else {
        helper.add_property_with_static_and_visibility(&qualified_name, span, is_static, visibility)
    };

    // Type annotation → TypeOf(Field) + Reference edges. The TypeOf edge
    // carries the BARE field name (AC-5) so set-membership queries on the
    // short name remain consistent across languages.
    if let Some(type_node) = field_node.child_by_field_name("type") {
        if let Some(type_text) = extract_type_string(type_node, content) {
            let type_id = helper.add_type(&type_text, Some(span_from_node(type_node)));
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(&raw_name),
            );
        }

        let all_types = extract_all_type_names_from_annotation(type_node, content);
        for type_name in all_types {
            let type_id = helper.add_type(&type_name, Some(span_from_node(type_node)));
            helper.add_reference_edge(field_id, type_id);
        }
    }

    Ok(())
}

/// Promote constructor parameters with parameter modifiers
/// (`public` / `private` / `protected` / `readonly`) into class fields.
///
/// TypeScript's "parameter properties" sugar (`constructor(public x: T)`)
/// declares both a constructor parameter AND a class field. This walker
/// emits the corresponding `Class.x` Property/Constant nodes.
///
/// Precedence (FR-13, AC-7): the explicit field declaration always wins.
/// If `helper.get_node` already maps the qualified name to an existing
/// node (the explicit field was emitted by `build_field_type_edges`), no
/// new node is created. The promoted-param branch never overrides the
/// existing visibility / kind. Note that the helper's
/// `update_node_entry` semantics already prevent visibility downgrades —
/// visibility is only filled when `entry.visibility.is_none()` — so
/// the explicit-field-wins guarantee holds even without the
/// pre-check, but the pre-check eliminates wasted node-cache thrash and
/// makes the precedence rule explicit at the call site.
///
/// Rejected (AC-8 corner case 6): `rest_pattern` parameters
/// (`...args: T[]`) never promote — TypeScript's grammar disallows
/// modifiers on rest parameters in practice, and even if a modifier
/// appears, the spec rejects it (no node, no panic).
fn promote_ctor_parameters_for_class(
    body_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_name: &str,
) -> GraphResult<()> {
    let mut cursor = body_node.walk();

    for child in body_node.children(&mut cursor) {
        if child.kind() != "method_definition" {
            continue;
        }

        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let Ok(method_name) = name_node.utf8_text(content) else {
            continue;
        };
        if method_name.trim() != "constructor" {
            continue;
        }

        let Some(params_node) = child.child_by_field_name("parameters") else {
            continue;
        };

        let mut param_cursor = params_node.walk();
        for param in params_node.children(&mut param_cursor) {
            // Only `required_parameter` / `optional_parameter` can carry
            // accessibility / readonly modifiers in TS. Anything else
            // (commas, parens, rest_pattern wrappers) is skipped.
            if !matches!(param.kind(), "required_parameter" | "optional_parameter") {
                continue;
            }

            promote_one_ctor_parameter(param, content, helper, class_name)?;
        }
    }

    Ok(())
}

/// Process a single `required_parameter` / `optional_parameter` for
/// constructor-parameter-promotion. See `promote_ctor_parameters_for_class`
/// for the FR-13 / AC-7 / AC-8 rules.
//
// Returns `GraphResult<()>` to keep the `?`-friendly call signature
// shared by neighboring helpers; current early-exit branches all yield
// `Ok`, but the surface intentionally mirrors the rest of the
// constructor-promotion pipeline.
#[allow(clippy::unnecessary_wraps)]
fn promote_one_ctor_parameter(
    param: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_name: &str,
) -> GraphResult<()> {
    let mut is_readonly = false;
    let mut visibility: Option<String> = None;
    let mut type_node: Option<Node<'_>> = None;
    let mut has_rest = false;

    let mut cursor = param.walk();
    for child in param.children(&mut cursor) {
        match child.kind() {
            "accessibility_modifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        visibility = Some(trimmed.to_string());
                    }
                }
            }
            "readonly" => is_readonly = true,
            "rest_pattern" => has_rest = true,
            "type_annotation" => type_node = Some(child),
            _ => {}
        }
    }

    // AC-8 corner case 6: rest parameters never promote.
    if has_rest {
        return Ok(());
    }

    // No promotion unless at least one parameter modifier is present.
    if visibility.is_none() && !is_readonly {
        return Ok(());
    }

    // The parameter's name comes from its `pattern` field (the
    // tree-sitter-typescript grammar models `required_parameter` /
    // `optional_parameter` with an explicit `pattern` field). Walking
    // for direct `identifier` children is unsafe: a defaulted parameter
    // such as `constructor(public y: U = fallback)` has the default
    // expression's identifier (`fallback`) as a direct child, which
    // would overwrite the real parameter name.
    //
    // Only promote when the pattern is an `identifier` node — other
    // patterns (`object_pattern`, `array_pattern`, etc.) are not
    // promotable as a single class field.
    let Some(pattern_node) = param.child_by_field_name("pattern") else {
        return Ok(());
    };
    if pattern_node.kind() != "identifier" {
        return Ok(());
    }
    let name_node = pattern_node;
    let Ok(raw_name) = name_node.utf8_text(content) else {
        return Ok(());
    };
    let raw_name = raw_name.trim();
    if raw_name.is_empty() {
        return Ok(());
    }

    let qualified_name = format!("{class_name}.{raw_name}");

    // FR-13 / AC-7 explicit-field-wins precedence: short-circuit if any
    // node with this qualified name already exists. The explicit-field
    // handler ran first (build_field_type_edges precedes ctor-promotion
    // in the class arm), so any pre-existing node was created by the
    // explicit declaration.
    //
    // The helper's node cache is keyed by *canonical* qualified name, so
    // we canonicalize the lookup probe to match. For TypeScript this
    // turns `Person.name` -> `Person::name`; without this step the
    // probe would always miss and the promoted-param branch would
    // always create a duplicate node, defeating FR-13.
    let canonical_probe = sqry_core::graph::unified::resolution::canonicalize_graph_qualified_name(
        Language::TypeScript,
        &qualified_name,
    );
    if helper.get_node(&canonical_probe).is_some() {
        return Ok(());
    }

    let span = Some(span_from_node(param));
    let visibility_ref = visibility.as_deref();
    let field_id = if is_readonly {
        helper.add_constant_with_static_and_visibility(&qualified_name, span, false, visibility_ref)
    } else {
        helper.add_property_with_static_and_visibility(&qualified_name, span, false, visibility_ref)
    };

    // Mirror the explicit-field TypeOf(Field) + Reference edges so the
    // promoted node is indistinguishable from a directly declared field
    // for downstream queries.
    if let Some(type_node) = type_node {
        if let Some(type_text) = extract_type_string(type_node, content) {
            let type_id = helper.add_type(&type_text, Some(span_from_node(type_node)));
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(raw_name),
            );
        }
        let all_types = extract_all_type_names_from_annotation(type_node, content);
        for type_name in all_types {
            let type_id = helper.add_type(&type_name, Some(span_from_node(type_node)));
            helper.add_reference_edge(field_id, type_id);
        }
    }

    Ok(())
}

/// Emit the enum container node and its members.
///
/// `const enum Colors { Red = 1 }` → members are `Constant`.
/// `enum Plain { A = 1 }` → members are `Property` (per AC-2 "otherwise
/// Property").
///
/// Member qualified names follow the `Enum.Member` form, which is
/// canonicalized to `Enum::Member` by the helper layer for TypeScript.
fn build_enum_and_members(
    enum_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let Some(name_node) = enum_node.child_by_field_name("name") else {
        return Ok(());
    };
    let enum_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(enum_node),
            reason: "failed to read enum name".to_string(),
        })?
        .trim()
        .to_string();
    if enum_name.is_empty() {
        return Ok(());
    }

    helper.add_enum(&enum_name, Some(span_from_node(enum_node)));

    // `const enum Colors { ... }` carries a leading `const` keyword child.
    let mut is_const = false;
    let mut cursor = enum_node.walk();
    for child in enum_node.children(&mut cursor) {
        if child.kind() == "const" {
            is_const = true;
            break;
        }
    }

    let Some(body) = enum_node.child_by_field_name("body") else {
        return Ok(());
    };
    let mut body_cursor = body.walk();
    for member in body.children(&mut body_cursor) {
        // tree-sitter-typescript represents enum members as either
        // `enum_assignment` (with `= value`) or a bare
        // `property_identifier`.
        let member_name_node = match member.kind() {
            "enum_assignment" => member.child_by_field_name("name"),
            "property_identifier" => Some(member),
            _ => None,
        };
        let Some(member_name_node) = member_name_node else {
            continue;
        };
        let Ok(member_name) = member_name_node.utf8_text(content) else {
            continue;
        };
        let member_name = member_name.trim();
        if member_name.is_empty() {
            continue;
        }
        let qualified = format!("{enum_name}.{member_name}");
        let span = Some(span_from_node(member));
        if is_const {
            helper.add_constant_with_static_and_visibility(&qualified, span, false, None);
        } else {
            helper.add_property_with_static_and_visibility(&qualified, span, false, None);
        }
    }

    Ok(())
}

fn build_variable_nodes(
    decl_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut cursor = decl_node.walk();

    for child in decl_node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };

            let name = name_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: span_from_node(child),
                    reason: "failed to read variable name".to_string(),
                })?
                .trim()
                .to_string();

            let is_function = child
                .child_by_field_name("value")
                .is_some_and(|v| matches!(v.kind(), "arrow_function" | "function"));

            // Create variable node for non-function variables only
            // (functions are already handled as CallContext in walk_ast)
            let variable_id = if is_function {
                None
            } else {
                Some(helper.add_variable(&name, Some(span_from_node(child))))
            };

            // Check for type annotation and create TypeOf + Reference edges
            // This applies to ALL typed variables, including typed arrow functions
            if let Some(type_node) = child.child_by_field_name("type") {
                // For typed arrow functions, we need to create or find the variable node
                let var_id = variable_id
                    .unwrap_or_else(|| helper.add_variable(&name, Some(span_from_node(child))));

                // Extract full type string for TypeOf edge
                if let Some(type_text) = extract_type_string(type_node, content) {
                    let type_id = helper.add_type(&type_text, Some(span_from_node(type_node)));
                    helper.add_typeof_edge_with_context(
                        var_id,
                        type_id,
                        Some(TypeOfContext::Variable),
                        None,
                        Some(&name),
                    );
                }

                // Extract all types for Reference edges (includes all union/intersection branches)
                let all_types = extract_all_type_names_from_annotation(type_node, content);
                for type_name in all_types {
                    let type_id = helper.add_type(&type_name, Some(span_from_node(type_node)));
                    helper.add_reference_edge(var_id, type_id);
                }
            }
        }
    }

    Ok(())
}

/// Build OOP edges for class declarations (extends, implements)
fn build_class_oop_edges(class_node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Get class name
    let class_name = class_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map_or_else(
            || SyntheticNameBuilder::from_node(&class_node, content, "class"),
            |s| s.trim().to_string(),
        );

    let class_id = helper.add_class(&class_name, Some(span_from_node(class_node)));

    // Look for class_heritage which contains extends and implements
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if child.kind() == "class_heritage" {
            build_class_heritage_edges(child, content, helper, class_id);
        }
    }
}

/// Build edges from `class_heritage` node (extends and implements clauses)
fn build_class_heritage_edges(
    heritage_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_id: sqry_core::graph::unified::NodeId,
) {
    let mut cursor = heritage_node.walk();

    for child in heritage_node.children(&mut cursor) {
        match child.kind() {
            "extends_clause" => {
                // class Child extends Parent
                build_extends_edges(child, content, helper, class_id, false);
            }
            "implements_clause" => {
                // class Foo implements IBar, IBaz
                build_implements_edges(child, content, helper, class_id);
            }
            _ => {}
        }
    }
}

/// Build Inherits edges from extends clause
fn build_extends_edges(
    extends_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    child_id: sqry_core::graph::unified::NodeId,
    is_interface: bool,
) {
    let mut cursor = extends_node.walk();

    for child in extends_node.children(&mut cursor) {
        // Skip the "extends" keyword
        if child.kind() == "extends" {
            continue;
        }

        // Get the parent type name
        let parent_name = extract_type_name(child, content);
        if let Some(name) = parent_name {
            let parent_id = if is_interface {
                helper.add_interface(&name, None)
            } else {
                helper.add_class(&name, None)
            };
            helper.add_inherits_edge(child_id, parent_id);
        }
    }
}

/// Build Implements edges from implements clause
fn build_implements_edges(
    implements_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_id: sqry_core::graph::unified::NodeId,
) {
    let mut cursor = implements_node.walk();

    for child in implements_node.children(&mut cursor) {
        // Skip keywords and commas
        if matches!(child.kind(), "implements" | ",") {
            continue;
        }

        // Get the interface name
        let interface_name = extract_type_name(child, content);
        if let Some(name) = interface_name {
            let interface_id = helper.add_interface(&name, None);
            helper.add_implements_edge(class_id, interface_id);
        }
    }
}

/// Build inheritance edges for interface declarations
fn build_interface_inheritance_edges(
    interface_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Get interface name
    let interface_name = interface_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map(|s| s.trim().to_string());

    let Some(name) = interface_name else {
        return;
    };

    let interface_id = helper.add_interface(&name, Some(span_from_node(interface_node)));

    // Look for extends_type_clause in interfaces
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        if child.kind() == "extends_type_clause" {
            build_interface_extends_edges(child, content, helper, interface_id);
        }
    }
}

/// Build Inherits edges from interface extends clause
fn build_interface_extends_edges(
    extends_clause: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    interface_id: sqry_core::graph::unified::NodeId,
) {
    let mut cursor = extends_clause.walk();

    for child in extends_clause.children(&mut cursor) {
        // Skip keywords and commas
        if matches!(child.kind(), "extends" | ",") {
            continue;
        }

        // Get the parent interface name
        let parent_name = extract_type_name(child, content);
        if let Some(name) = parent_name {
            let parent_id = helper.add_interface(&name, None);
            helper.add_inherits_edge(interface_id, parent_id);
        }
    }
}

/// Extract type name from a type node (handles generics, qualified names, etc.)
fn extract_type_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" => {
            node.utf8_text(content).ok().map(|s| s.trim().to_string())
        }
        "generic_type" => {
            // Generic<T> -> extract "Generic"
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
        }
        "nested_type_identifier" | "member_expression" => {
            // Namespace.Type -> extract full qualified name
            node.utf8_text(content).ok().map(|s| s.trim().to_string())
        }
        _ => {
            // Fallback: try to get the text directly
            node.utf8_text(content).ok().map(|s| s.trim().to_string())
        }
    }
}

/// Minimal edge representation for internal use
struct EdgeWithIds {
    from: sqry_core::graph::unified::NodeId,
    to: sqry_core::graph::unified::NodeId,
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

fn count_arguments(call_node: Node<'_>) -> usize {
    call_node
        .child_by_field_name("arguments")
        .or_else(|| call_node.child_by_field_name("type_arguments"))
        .map_or(0, |args| {
            args.named_children(&mut args.walk())
                .filter(|child| !matches!(child.kind(), "," | "(" | ")"))
                .count()
        })
}

fn is_awaited_call(call_node: Node<'_>) -> bool {
    let mut current = call_node.parent();
    while let Some(node) = current {
        let kind = node.kind();
        if kind == "await_expression" || kind == "await" {
            return true;
        }
        current = node.parent();
    }
    false
}

fn extract_return_type_annotation(node: Node<'_>, content: &[u8]) -> Option<String> {
    let return_node = node.child_by_field_name("return_type")?;
    let type_node = return_node.named_child(0).unwrap_or(return_node);
    let raw = type_node.utf8_text(content).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.trim_start_matches(':').trim().to_string())
}

fn simple_name(qualified: &str) -> &str {
    qualified.split('.').next_back().unwrap_or(qualified)
}

fn module_basename(path: &str) -> String {
    let trimmed = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_end_matches('/');
    let last_segment = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    last_segment.to_string()
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: Span,
    is_async: bool,
    return_type: Option<String>,
    visibility: Option<String>,
    is_exported: bool,
}

impl CallContext {
    fn qualified_name(&self) -> String {
        self.qualified_name.clone()
    }
}

struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();
        let mut scope_stack: Vec<String> = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        walk_ast(
            tree.root_node(),
            content,
            &mut contexts,
            &mut node_to_context,
            &mut scope_stack,
            max_depth,
            &mut guard,
            false, // not exported by default
        )?;

        // Collect exported names and update CallContext visibility
        let exported_names = Self::collect_exported_names(tree, content);
        let mut ast_graph = Self {
            contexts,
            node_to_context,
        };
        ast_graph.update_export_visibility(&exported_names);

        Ok(ast_graph)
    }

    fn contexts(&self) -> &[CallContext] {
        &self.contexts
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }

    /// Collect all exported names from export statements (clauses and default exports)
    fn collect_exported_names(tree: &Tree, content: &[u8]) -> std::collections::HashSet<String> {
        fn walk_for_exports(
            node: Node,
            content: &[u8],
            exported_names: &mut std::collections::HashSet<String>,
        ) {
            if node.kind() == "export_statement" {
                // Skip re-exports: export { foo } from "./mod"
                // Only collect names from local exports
                if node.child_by_field_name("source").is_some() {
                    return; // This is a re-export, don't collect names
                }

                // Process export clauses: export { foo, bar }
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "export_clause" => {
                            // Extract names from export_specifier nodes
                            let mut clause_cursor = child.walk();
                            for specifier in child.children(&mut clause_cursor) {
                                if specifier.kind() == "export_specifier"
                                    && let Some(name_node) = specifier.child_by_field_name("name")
                                    && let Ok(name_text) = name_node.utf8_text(content)
                                {
                                    exported_names.insert(name_text.trim().to_string());
                                }
                            }
                        }
                        "default" => {
                            // export default foo or export default function foo()
                            // Look for identifier after "default"
                            if let Some(default_value) = get_default_export_value(node, content) {
                                // If it's an identifier, it references an existing declaration
                                if default_value.kind() == "identifier" {
                                    if let Ok(name_text) = default_value.utf8_text(content) {
                                        exported_names.insert(name_text.trim().to_string());
                                    }
                                }
                                // If it's a declaration with a name, extract it
                                else if let Some(name_node) =
                                    default_value.child_by_field_name("name")
                                    && let Ok(name_text) = name_node.utf8_text(content)
                                {
                                    exported_names.insert(name_text.trim().to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                // Recurse into children
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_for_exports(child, content, exported_names);
                }
            }
        }

        let mut exported_names = std::collections::HashSet::new();
        let root = tree.root_node();
        walk_for_exports(root, content, &mut exported_names);
        exported_names
    }

    /// Update `CallContext` visibility for exported symbols
    fn update_export_visibility(&mut self, exported_names: &std::collections::HashSet<String>) {
        for context in &mut self.contexts {
            // Extract the base name from qualified name (strip namespace/class prefix)
            let base_name = context
                .qualified_name
                .split('.')
                .next_back()
                .unwrap_or(&context.qualified_name);

            // If this symbol is exported, mark it as such
            if exported_names.contains(base_name) {
                context.is_exported = true;
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "AST walk handles multiple relation builders in one traversal"
)]
/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
fn walk_ast(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallContext>,
    node_to_context: &mut HashMap<usize, usize>,
    scope_stack: &mut Vec<String>,
    max_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
    is_exported: bool,
) -> Result<(), String> {
    guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    if scope_stack.len() > max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        // TypeScript: namespace/module declarations
        "namespace_declaration" | "module_declaration" | "internal_module" | "module" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| format!("{} missing name", node.kind()))?;
            let name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read namespace name".to_string())?;

            scope_stack.push(name.to_string());

            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        max_depth,
                        guard,
                        false, // Namespace body is a new scope
                    )?;
                }
            }

            scope_stack.pop();
        }
        "class_declaration" | "class" => {
            let class_name = node
                .child_by_field_name("name")
                .and_then(|name_node| {
                    name_node
                        .utf8_text(content)
                        .ok()
                        .map(std::string::ToString::to_string)
                })
                .unwrap_or_else(|| SyntheticNameBuilder::from_node(&node, content, "class"));

            scope_stack.push(class_name);

            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        max_depth,
                        guard,
                        false, // Class body members are not individually exported
                    )?;
                }
            }

            scope_stack.pop();
        }
        "function_declaration"
        | "function_expression"
        | "function"
        | "method_definition"
        | "function_signature" => {
            let name_node = node.child_by_field_name("name");
            let func_name = if let Some(name) = name_node {
                name.utf8_text(content)
                    .map_err(|_| "failed to read function name".to_string())?
                    .to_string()
            } else {
                // Anonymous function expression - synthesize a stable name
                SyntheticNameBuilder::from_node(&node, content, "function")
            };

            // Check if async
            let is_async = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "async");
            let return_type = extract_return_type_annotation(node, content);
            let visibility = extract_visibility(node, content);

            let qualified_func = if scope_stack.is_empty() {
                func_name.clone()
            } else {
                format!("{}.{}", scope_stack.join("."), func_name)
            };

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: span_from_node(node),
                is_async,
                return_type,
                visibility,
                is_exported,
            });

            // Register the function-declaration node itself so that
            // `get_callable_context(function_node.id())` resolves to this
            // function's own context (not its enclosing scope). This is what
            // gates return-type edge emission in `walk_for_edges_with_namespaces`.
            node_to_context.insert(node.id(), context_idx);

            // Associate all descendants
            if let Some(body) = node.child_by_field_name("body") {
                associate_descendants(body, context_idx, node_to_context);
            }

            scope_stack.push(func_name);

            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        max_depth,
                        guard,
                        false, // Function body is local scope
                    )?;
                }
            }

            scope_stack.pop();
        }
        "lexical_declaration" => {
            // Handle arrow functions: const foo = () => {}
            // Check if this is an arrow function assignment
            let mut handled_arrow = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(init) = child.child_by_field_name("value")
                    && init.kind() == "arrow_function"
                {
                    // Process the arrow function with the variable name
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let is_async = init.children(&mut init.walk()).any(|c| c.kind() == "async");
                        let return_type = extract_return_type_annotation(init, content);

                        let func_name = match name_node.kind() {
                            "identifier" | "property_identifier" | "type_identifier" => name_node
                                .utf8_text(content)
                                .map_err(|_| "failed to read arrow function name".to_string())?
                                .trim()
                                .to_string(),
                            _ => SyntheticNameBuilder::from_node(&init, content, "arrow"),
                        };

                        let qualified_func = if scope_stack.is_empty() {
                            func_name.clone()
                        } else {
                            format!("{}.{}", scope_stack.join("."), func_name)
                        };

                        let context_idx = contexts.len();
                        contexts.push(CallContext {
                            qualified_name: qualified_func.clone(),
                            span: span_from_node(init),
                            is_async,
                            return_type,
                            visibility: None, // Arrow functions don't have visibility modifiers
                            is_exported,
                        });

                        // Register the arrow_function init node itself so that
                        // `walk_for_edges_with_namespaces` can resolve it to this
                        // context when emitting return-type edges.
                        node_to_context.insert(init.id(), context_idx);

                        if let Some(body) = init.child_by_field_name("body") {
                            associate_descendants(body, context_idx, node_to_context);
                        }

                        scope_stack.push(func_name);

                        if let Some(body) = init.child_by_field_name("body") {
                            let mut inner_cursor = body.walk();
                            for inner_child in body.children(&mut inner_cursor) {
                                walk_ast(
                                    inner_child,
                                    content,
                                    contexts,
                                    node_to_context,
                                    scope_stack,
                                    max_depth,
                                    guard,
                                    false, // Arrow function body is local scope
                                )?;
                            }
                        }

                        scope_stack.pop();

                        handled_arrow = true;
                    }
                }
            }

            // Only recurse if we didn't handle an arrow function
            // (otherwise we'd visit it twice)
            if !handled_arrow {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        max_depth,
                        guard,
                        is_exported, // Preserve export context for top-level variables
                    )?;
                }
            }
        }
        "arrow_function" => {
            // Standalone arrow function (not in a lexical_declaration)
            let func_name = SyntheticNameBuilder::from_node(&node, content, "arrow");
            let is_async = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "async");
            let return_type = extract_return_type_annotation(node, content);

            let qualified_func = if scope_stack.is_empty() {
                func_name.clone()
            } else {
                format!("{}.{}", scope_stack.join("."), func_name)
            };

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name: qualified_func,
                span: span_from_node(node),
                is_async,
                return_type,
                visibility: None, // Standalone arrow functions don't have visibility
                is_exported,
            });

            // Register the standalone arrow_function node itself so that
            // `walk_for_edges_with_namespaces` can resolve it to this context
            // when emitting return-type edges (synthetic-named arrows are
            // intentionally skipped at the emission site).
            node_to_context.insert(node.id(), context_idx);

            if let Some(body) = node.child_by_field_name("body") {
                associate_descendants(body, context_idx, node_to_context);
            }

            // Don't recurse into standalone arrow functions
            // (they're self-contained contexts)
        }
        "export_statement" => {
            // Export statements mark their children as exported
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(
                    child,
                    content,
                    contexts,
                    node_to_context,
                    scope_stack,
                    max_depth,
                    guard,
                    true, // Children of export statements are exported
                )?;
            }
        }
        _ => {
            // Recurse into children, preserving export context
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(
                    child,
                    content,
                    contexts,
                    node_to_context,
                    scope_stack,
                    max_depth,
                    guard,
                    is_exported,
                )?;
            }
        }
    }

    guard.exit();
    Ok(())
}

fn associate_descendants(
    node: Node,
    context_idx: usize,
    node_to_context: &mut HashMap<usize, usize>,
) {
    node_to_context.insert(node.id(), context_idx);

    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        node_to_context.insert(current.id(), context_idx);

        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ============================================================================
// FFI Detection - WebAssembly and Node.js native addons
// ============================================================================

/// Build FFI edges for call expressions.
///
/// Detects:
/// - `WebAssembly.instantiate(buffer)` / `WebAssembly.instantiateStreaming(fetch(...))`
/// - `WebAssembly.compile(buffer)` / `WebAssembly.compileStreaming(fetch(...))`
/// - `require('./native.node')` - Node.js native addons
/// - `process.dlopen(module, filename)` - Node.js dynamic loading
///
/// Returns true if an FFI edge was created, false otherwise.
fn build_ffi_call_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<bool> {
    let Some(callee_expr) = call_node.child_by_field_name("function") else {
        return Ok(false);
    };

    let callee_text = callee_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim();

    // Check for WebAssembly API calls
    if callee_text.starts_with("WebAssembly.") {
        return Ok(build_webassembly_call_edge(
            ast_graph,
            call_node,
            content,
            callee_text,
            helper,
        ));
    }

    // Check for Node.js native addon require
    if callee_text == "require" {
        return Ok(build_require_ffi_edge(
            ast_graph, call_node, content, helper,
        ));
    }

    // Check for process.dlopen
    if callee_text == "process.dlopen" {
        return Ok(build_dlopen_edge(ast_graph, call_node, content, helper));
    }

    Ok(false)
}

/// Build FFI edges for new expressions (constructor calls).
///
/// Detects:
/// - `new WebAssembly.Module(buffer)`
/// - `new WebAssembly.Instance(module, imports)`
///
/// Returns true if an FFI edge was created, false otherwise.
fn build_ffi_new_edge(
    ast_graph: &ASTGraph,
    new_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<bool> {
    let Some(constructor_expr) = new_node.child_by_field_name("constructor") else {
        return Ok(false);
    };

    let constructor_text = constructor_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(new_node),
            reason: "failed to read constructor expression".to_string(),
        })?
        .trim();

    // Check for WebAssembly constructors
    if constructor_text == "WebAssembly.Module" || constructor_text == "WebAssembly.Instance" {
        return Ok(build_webassembly_constructor_edge(
            ast_graph,
            new_node,
            content,
            constructor_text,
            helper,
        ));
    }

    Ok(false)
}

/// Build WebAssembly call edge for API calls like instantiate/compile.
fn build_webassembly_call_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    callee_text: &str,
    helper: &mut GraphBuildHelper,
) -> bool {
    // Extract the method name
    let method_name = callee_text
        .strip_prefix("WebAssembly.")
        .unwrap_or(callee_text);

    // Only handle known WebAssembly methods that load/instantiate WASM
    let is_wasm_load = matches!(
        method_name,
        "instantiate" | "instantiateStreaming" | "compile" | "compileStreaming" | "validate"
    );

    if !is_wasm_load {
        return false;
    }

    // Get caller context
    let caller_id = get_caller_node_id(ast_graph, call_node, content, helper);

    // Try to extract module path from arguments (if it's a fetch() call or string literal)
    let wasm_module_name = extract_wasm_module_name(call_node, content)
        .unwrap_or_else(|| format!("wasm::{method_name}"));

    // Create WASM module node with qualified name
    let wasm_node_id = helper.add_module(&wasm_module_name, Some(span_from_node(call_node)));

    // Add WebAssembly edge
    helper.add_webassembly_edge(caller_id, wasm_node_id);

    true
}

/// Build WebAssembly edge for constructor calls (new WebAssembly.Module/Instance).
fn build_webassembly_constructor_edge(
    ast_graph: &ASTGraph,
    new_node: Node<'_>,
    content: &[u8],
    constructor_text: &str,
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get caller context
    let caller_id = get_caller_node_id(ast_graph, new_node, content, helper);

    // Determine module name
    let type_name = constructor_text
        .strip_prefix("WebAssembly.")
        .unwrap_or(constructor_text);
    let wasm_module_name = format!("wasm::{type_name}");

    // Create WASM module node
    let wasm_node_id = helper.add_module(&wasm_module_name, Some(span_from_node(new_node)));

    // Add WebAssembly edge
    helper.add_webassembly_edge(caller_id, wasm_node_id);

    true
}

/// Build FFI edge for `require()` calls that load native addons.
fn build_require_ffi_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get the first argument (module path)
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return false;
    };

    let mut cursor = args.walk();
    let first_arg = args
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","));

    let Some(arg_node) = first_arg else {
        return false;
    };

    // Extract the module path
    let module_path = extract_string_literal(&arg_node, content);
    let Some(path) = module_path else {
        return false;
    };

    // Check if this is a native addon (.node file or known native packages)
    let is_native_addon = std::path::Path::new(&path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("node"))
        || is_known_native_addon(&path);

    if !is_native_addon {
        return false;
    }

    // Get caller context
    let caller_id = get_caller_node_id(ast_graph, call_node, content, helper);

    // Create FFI target node
    let ffi_name = format!("native::{}", module_basename(&path));
    let ffi_node_id = helper.add_module(&ffi_name, Some(span_from_node(call_node)));

    // Add FFI edge with C convention (Node.js native addons use N-API/C ABI)
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Build FFI edge for `process.dlopen()` calls.
fn build_dlopen_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get caller context
    let caller_id = get_caller_node_id(ast_graph, call_node, content, helper);

    // Try to extract filename from second argument
    let module_name = call_node
        .child_by_field_name("arguments")
        .and_then(|args| {
            let mut cursor = args.walk();
            args.children(&mut cursor)
                .filter(|child| !matches!(child.kind(), "(" | ")" | ","))
                .nth(1) // Second argument is the filename
        })
        .and_then(|node| extract_string_literal(&node, content))
        .map_or_else(
            || "native::dlopen".to_string(),
            |path| format!("native::{}", module_basename(&path)),
        );

    // Create FFI target node
    let ffi_node_id = helper.add_module(&module_name, Some(span_from_node(call_node)));

    // Add FFI edge
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Get the caller node ID from AST context.
fn get_caller_node_id(
    ast_graph: &ASTGraph,
    node: Node<'_>,
    _content: &[u8],
    helper: &mut GraphBuildHelper,
) -> sqry_core::graph::unified::NodeId {
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(node.id()) {
        ctx
    } else {
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: Span::default(),
            is_async: false,
            return_type: None,
            visibility: None,
            is_exported: false,
        };
        &module_context
    };

    let caller_span = Some(call_context.span);
    helper.ensure_function(
        &call_context.qualified_name(),
        caller_span,
        call_context.is_async,
        false,
    )
}

/// Try to extract WASM module name from call arguments.
///
/// Handles patterns like:
/// - `WebAssembly.instantiate(fetch('./module.wasm'))` -> "./module.wasm"
/// - `WebAssembly.instantiate(buffer)` -> None (can't determine statically)
fn extract_wasm_module_name(call_node: Node<'_>, content: &[u8]) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;

    let mut cursor = args.walk();
    let first_arg = args
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;

    // Check if it's a fetch() call
    if first_arg.kind() == "call_expression"
        && let Some(func) = first_arg.child_by_field_name("function")
    {
        let func_text = func.utf8_text(content).ok()?.trim();
        if func_text == "fetch" {
            // Extract URL from fetch argument
            if let Some(fetch_args) = first_arg.child_by_field_name("arguments") {
                let mut fetch_cursor = fetch_args.walk();
                let url_arg = fetch_args
                    .children(&mut fetch_cursor)
                    .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;

                if let Some(url) = extract_string_literal(&url_arg, content) {
                    return Some(format!("wasm::{}", module_basename(&url)));
                }
            }
        }
    }

    // Check if it's a string literal (file path)
    if let Some(path) = extract_string_literal(&first_arg, content) {
        return Some(format!("wasm::{}", module_basename(&path)));
    }

    None
}

/// Extract string literal value from a node.
fn extract_string_literal(node: &Node, content: &[u8]) -> Option<String> {
    let text = node.utf8_text(content).ok()?;
    let trimmed = text.trim();

    // Remove quotes
    trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .or_else(|| trimmed.strip_prefix('`').and_then(|s| s.strip_suffix('`')))
        .map(std::string::ToString::to_string)
}

// ========== HTTP Request Edge Detection ==========

#[derive(Debug, Clone)]
struct HttpRequestInfo {
    method: HttpMethod,
    url: Option<String>,
}

fn build_http_request_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    let Some(info) = extract_http_request_info(call_node, content) else {
        return false;
    };

    let caller_id = get_caller_node_id(ast_graph, call_node, content, helper);
    let target_name = info.url.as_ref().map_or_else(
        || format!("http::{}", info.method.as_str()),
        |url| format!("http::{url}"),
    );
    let target_id = helper.add_module(&target_name, Some(span_from_node(call_node)));

    helper.add_http_request_edge(caller_id, target_id, info.method, info.url.as_deref());
    true
}

fn extract_http_request_info(call_node: Node<'_>, content: &[u8]) -> Option<HttpRequestInfo> {
    let callee = call_node.child_by_field_name("function")?;
    let callee_text = callee.utf8_text(content).ok()?.trim().to_string();

    if callee_text == "fetch" {
        return Some(extract_fetch_http_info(call_node, content));
    }

    if callee_text == "axios" {
        return extract_axios_http_info(call_node, content);
    }

    if let Some(method_name) = callee_text.strip_prefix("axios.") {
        let method = http_method_from_name(method_name)?;
        let url = extract_first_arg_url(call_node, content);
        return Some(HttpRequestInfo { method, url });
    }

    None
}

fn extract_fetch_http_info(call_node: Node<'_>, content: &[u8]) -> HttpRequestInfo {
    let url = extract_first_arg_url(call_node, content);
    let method = extract_method_from_options(call_node, content).unwrap_or(HttpMethod::Get);
    HttpRequestInfo { method, url }
}

fn extract_axios_http_info(call_node: Node<'_>, content: &[u8]) -> Option<HttpRequestInfo> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut non_trivia = args
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","));

    let first_arg = non_trivia.next()?;
    let second_arg = non_trivia.next();

    if first_arg.kind() == "object" {
        let (method, url) = extract_method_and_url_from_object(first_arg, content);
        return Some(HttpRequestInfo {
            method: method.unwrap_or(HttpMethod::Get),
            url,
        });
    }

    let url = extract_string_literal(&first_arg, content);
    let method = if let Some(config) = second_arg {
        if config.kind() == "object" {
            extract_method_from_object(config, content)
        } else {
            None
        }
    } else {
        None
    };

    Some(HttpRequestInfo {
        method: method.unwrap_or(HttpMethod::Get),
        url,
    })
}

fn extract_first_arg_url(call_node: Node<'_>, content: &[u8]) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let first_arg = args
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;
    extract_string_literal(&first_arg, content)
}

fn extract_method_from_options(call_node: Node<'_>, content: &[u8]) -> Option<HttpMethod> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut non_trivia = args
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","));

    let _first_arg = non_trivia.next()?;
    let second_arg = non_trivia.next()?;
    if second_arg.kind() != "object" {
        return None;
    }

    extract_method_from_object(second_arg, content)
}

fn extract_method_from_object(obj_node: Node<'_>, content: &[u8]) -> Option<HttpMethod> {
    let (method, _url) = extract_method_and_url_from_object(obj_node, content);
    method
}

fn extract_method_and_url_from_object(
    obj_node: Node<'_>,
    content: &[u8],
) -> (Option<HttpMethod>, Option<String>) {
    let mut method = None;
    let mut url = None;
    let mut cursor = obj_node.walk();

    for child in obj_node.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }

        let Some(key_node) = child.child_by_field_name("key") else {
            continue;
        };
        let key_text = extract_object_key_text(&key_node, content);

        let Some(value_node) = child.child_by_field_name("value") else {
            continue;
        };

        if key_text.as_deref() == Some("method") {
            if let Some(value) = extract_string_literal(&value_node, content) {
                method = http_method_from_name(&value);
            }
        } else if key_text.as_deref() == Some("url") {
            url = extract_string_literal(&value_node, content);
        }
    }

    (method, url)
}

fn extract_object_key_text(node: &Node<'_>, content: &[u8]) -> Option<String> {
    let raw = node.utf8_text(content).ok()?.trim().to_string();
    if let Some(value) = extract_string_literal(node, content) {
        return Some(value);
    }
    if raw.is_empty() {
        return None;
    }
    Some(raw)
}

fn http_method_from_name(name: &str) -> Option<HttpMethod> {
    match name.trim().to_ascii_lowercase().as_str() {
        "get" => Some(HttpMethod::Get),
        "post" => Some(HttpMethod::Post),
        "put" => Some(HttpMethod::Put),
        "delete" => Some(HttpMethod::Delete),
        "patch" => Some(HttpMethod::Patch),
        "head" => Some(HttpMethod::Head),
        "options" => Some(HttpMethod::Options),
        _ => None,
    }
}

// ========== Route Endpoint Detection ==========

/// HTTP method names recognized as route registration methods.
///
/// When a call expression uses one of these as a member property (e.g., `app.get(...)`,
/// `router.post(...)`), it is treated as an Express/Koa/Fastify-style route registration.
const ROUTE_METHOD_NAMES: &[&str] = &["get", "post", "put", "delete", "patch", "all"];

/// Detect Express/Koa/Fastify-style route endpoint registrations.
///
/// Matches patterns like:
/// - `app.get("/api/users", handler)`
/// - `router.post("/api/items", createItem)`
/// - `server.delete("/api/items/:id", deleteItem)`
///
/// When a route is detected, an `Endpoint` node is created with the qualified name
/// format `route::METHOD::/path`. If the handler argument is an identifier, a
/// `Contains` edge is added from the endpoint to the handler function.
///
/// # Arguments
/// * `call_node` - A `call_expression` AST node
/// * `content` - Source file content as bytes
/// * `helper` - Graph build helper for creating nodes and edges
///
/// # Returns
/// `true` if a route endpoint was detected and created, `false` otherwise.
fn detect_route_endpoint(
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    // The callee must be a member_expression (e.g., `app.get`)
    let callee = match call_node.child_by_field_name("function") {
        Some(c) if c.kind() == "member_expression" => c,
        _ => return false,
    };

    // Extract the property name (the HTTP method)
    let Some(property_node) = callee.child_by_field_name("property") else {
        return false;
    };

    let property_text = match property_node.utf8_text(content) {
        Ok(t) => t.trim(),
        Err(_) => return false,
    };

    // Check if the property is a known route method name
    if !ROUTE_METHOD_NAMES.contains(&property_text) {
        return false;
    }

    // Extract the HTTP method
    let method = if property_text == "all" {
        HttpMethod::All
    } else {
        let Some(m) = http_method_from_name(property_text) else {
            return false;
        };
        m
    };

    // Extract the path from the first argument
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return false;
    };

    let mut cursor = args.walk();
    let mut non_trivia = args
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","));

    let Some(first_arg) = non_trivia.next() else {
        return false;
    };

    let Some(path) = extract_string_literal(&first_arg, content) else {
        return false;
    };

    // Build the qualified name: route::METHOD::/path
    let qualified_name = format!("route::{}::{path}", method.as_str());
    let endpoint_id = helper.add_endpoint(&qualified_name, Some(span_from_node(call_node)));

    // If there is a handler argument that is an identifier, create a Contains edge
    // from the endpoint to the handler
    if let Some(handler_arg) = non_trivia.next()
        && handler_arg.kind() == "identifier"
        && let Ok(handler_name) = handler_arg.utf8_text(content)
    {
        let handler_name = handler_name.trim();
        if !handler_name.is_empty() {
            let handler_id = helper.ensure_function(
                handler_name,
                Some(span_from_node(handler_arg)),
                false,
                false,
            );
            helper.add_contains_edge(endpoint_id, handler_id);
        }
    }

    true
}

/// Check if a package name is a known native addon.
fn is_known_native_addon(package_name: &str) -> bool {
    // Common native addon packages
    const NATIVE_PACKAGES: &[&str] = &[
        "better-sqlite3",
        "sqlite3",
        "bcrypt",
        "sharp",
        "canvas",
        "node-sass",
        "leveldown",
        "bufferutil",
        "utf-8-validate",
        "fsevents",
        "cpu-features",
        "node-gyp",
        "node-pre-gyp",
        "prebuild",
        "nan",
        "node-addon-api",
        "ref-napi",
        "ffi-napi",
    ];

    NATIVE_PACKAGES
        .iter()
        .any(|&pkg| package_name == pkg || package_name.starts_with(&format!("{pkg}/")))
}

// ============================================================================
// TypeOf/Reference Edge Support
// ============================================================================

/// Extract visibility modifier from a TypeScript node.
///
/// Looks for `accessibility_modifier` child nodes which can be:
/// - `public`
/// - `private`
/// - `protected`
///
/// # Arguments
/// * `node` - The AST node to check (typically a method or property)
/// * `content` - Source file content as bytes
///
/// # Returns
/// * `Some("public")`, `Some("private")`, or `Some("protected")`
/// * `None` - If no visibility modifier is present (defaults to public in TypeScript)
fn extract_visibility(node: Node<'_>, content: &[u8]) -> Option<String> {
    // Look for accessibility_modifier child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "accessibility_modifier" {
            return child.utf8_text(content).ok().map(|s| s.trim().to_string());
        }
    }
    None
}

// ============================================================================
// Generic Type Parameter Emission (REQ:R0030 / U21 — C2_GEN_TP_TS)
// ============================================================================
//
// Emits per-type-parameter `Type` nodes for generic TypeScript declarations
// of the five shapes that carry a `type_parameters` field on
// tree-sitter-typescript 0.23.x:
//
//   - `function_declaration`    → `<FunctionName>::<ParamName>`
//   - `method_definition`       → `<ClassName>::<MethodName>::<ParamName>`
//   - `function_signature`      → `<FunctionName>::<ParamName>`
//   - `class_declaration`       → `<ClassName>::<ParamName>`
//   - `interface_declaration`   → `<InterfaceName>::<ParamName>`
//   - `type_alias_declaration`  → `<TypeAlias>::<ParamName>`
//
// Tree-sitter grammar shape:
//
// ```text
// type_parameters: '<' commaSep1(type_parameter) '>'
// type_parameter:
//   name:       type_identifier
//   constraint: optional(constraint)        // <T extends X>
//   value:      optional(default_type)      // <T = X>
// constraint: 'extends' type
// default_type: '=' type
// ```
//
// AC-3: `extends` constraints emit `TypeOf{Constraint}` edges; defaults
// (`<T = number>`) emit `References` edges to the default type.
//
// AC-5: variadic tuples like `<T extends unknown[]>` go through the same
// path; the constraint type's source text is taken verbatim (so
// `unknown[]`, `[A, B, ...C]`, etc. become synthetic Type nodes named
// after their textual form). Cross-file unification (Phase 4c-prime)
// can collapse the synthetic stub into the canonical declaration when
// one exists.
//
// AC-6: conditional types (`type R<T> = T extends X ? Y : Z`) are
// handled correctly without special-casing here. The generic-parameter
// list of `R` carries `<T>` only — `T extends X` lives inside the body
// of a `conditional_type` node within the `value` field, which is
// processed by `build_type_alias_edges` via
// `extract_all_type_names_from_annotation` and emits References edges
// only. `process_type_parameter_declarations` walks the
// `type_parameters` field exclusively, so it never touches the
// conditional path and never emits a spurious Constraint edge for
// `R::T`.

/// Walk ancestors of `node` collecting enclosing TypeScript namespace
/// names (deepest-last in source order, so they appear outermost-first
/// when joined). Used to namespace-qualify class / interface /
/// type-alias names emitted by the U21 (REQ:R0030) generic
/// type-parameter pipeline.
///
/// Function and method declarations already receive a namespace-aware
/// qualified name via `ASTGraph::get_callable_context` (the `walk_ast`
/// pass pushes namespace names onto its own `scope_stack`); the
/// container declarations did not have an equivalent and dropped the
/// namespace base, producing `Box::T` instead of `N::Box::T`. This
/// helper restores parity by reading the namespace stack straight from
/// the AST.
///
/// Tree-sitter-typescript spells namespace declarations as one of
/// `namespace_declaration`, `module_declaration`, `internal_module`, or
/// `module` (the last covers the deprecated `module` keyword). Each of
/// these carries its name under the `name` field as either an
/// `identifier` or a `nested_identifier` (for `namespace A.B { ... }`).
/// `nested_identifier` UTF-8 text already contains the dotted path, so
/// we keep it verbatim.
fn collect_namespace_prefix(node: Node<'_>, content: &[u8]) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "namespace_declaration" | "module_declaration" | "internal_module" | "module"
        ) && let Some(name_node) = parent.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
        {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                prefixes.push(trimmed.to_string());
            }
        }
        current = parent.parent();
    }
    // Outermost-first.
    prefixes.reverse();
    prefixes
}

/// Build a namespace-qualified container name for a class /
/// interface / type-alias declaration. When `node` sits inside one or
/// more enclosing namespaces, the result is `N1.N2...Nk.<local_name>`;
/// otherwise it is just `<local_name>`. The `.` separator is the same
/// one `walk_ast` uses on its `scope_stack` (graph canonicalisation
/// rewrites it to `::`).
fn namespace_qualified_container_name(node: Node<'_>, content: &[u8], local_name: &str) -> String {
    let prefixes = collect_namespace_prefix(node, content);
    if prefixes.is_empty() {
        local_name.to_string()
    } else {
        format!("{}.{}", prefixes.join("."), local_name)
    }
}

/// Best-effort fallback qualified-name resolver for callable nodes
/// that `ASTGraph` does not register in its callable-context table —
/// notably `method_signature` (interface methods) — and for cases where
/// the lookup misses for any reason.
///
/// Walks up the AST collecting namespace declarations and the nearest
/// enclosing class / interface / type-alias as a method/function
/// container, then appends the local name. Anonymous nodes are skipped.
/// The resulting separator is `.` (matching `walk_ast`); graph
/// canonicalisation rewrites it to `::`.
fn compute_callable_qname(node: Node<'_>, content: &[u8]) -> Option<String> {
    let local_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let mut segments: Vec<String> = collect_namespace_prefix(node, content);

    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_declaration" | "class" | "interface_declaration" | "type_alias_declaration"
        ) && let Some(name_node) = parent.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(content)
        {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                segments.push(trimmed.to_string());
                break;
            }
        }
        current = parent.parent();
    }

    segments.push(local_name);
    Some(segments.join("."))
}

/// Emit per-type-parameter `Type` nodes and `TypeOf{Constraint}` /
/// `References` edges for a generic TypeScript declaration.
fn process_type_parameter_declarations(
    decl_node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    let Some(params_node) = decl_node.child_by_field_name("type_parameters") else {
        return;
    };

    let mut cursor = params_node.walk();
    for param_node in params_node.children(&mut cursor) {
        if param_node.kind() != "type_parameter" {
            continue;
        }

        // Parameter name lives under the `name` field.
        let Some(name_node) = param_node.child_by_field_name("name") else {
            continue;
        };
        let Ok(param_name) = name_node.utf8_text(content) else {
            continue;
        };

        // AC-2: qualified name `<Parent>.<Param>` — the canonicaliser
        // rewrites the source `.` separator to graph-internal `::`.
        let qualified_param = format!("{parent_qualified_name}.{param_name}");
        // AC-2: span anchored on the parameter identifier so
        // "Find Definition" / hover navigation lands on the declaration
        // site rather than the synthetic `(0, 0)` sentinel.
        let param_id = helper.add_type(&qualified_param, Some(span_from_node(name_node)));

        // AC-3 / AC-5: `extends` constraint → TypeOf{Constraint} edge.
        if let Some(constraint_node) = param_node.child_by_field_name("constraint") {
            emit_type_parameter_constraint_edges(constraint_node, content, param_id, helper);
        }

        // AC-3: default-type (`<T = X>`) → References edge.
        if let Some(default_node) = param_node.child_by_field_name("value") {
            emit_type_parameter_default_edges(default_node, content, param_id, helper);
        }
    }
}

/// Emit `TypeOf{Constraint}` edges for the `constraint` child of a
/// `type_parameter`.
///
/// The grammar shape is `constraint: 'extends' type`. The `type` child
/// can be any TypeScript type expression — a named identifier, a
/// generic-instantiation (`Comparable<T>`), an array type
/// (`unknown[]`), a tuple type (`[A, B]`), etc. We take the verbatim
/// source-text spelling of the bound as the synthetic Type node name
/// so that searches like `kind:type name:unknown[]` find the
/// constraint. Cross-file unification will collapse the synthetic stub
/// into the canonical declaration when one exists in the workspace.
fn emit_type_parameter_constraint_edges(
    constraint_node: Node<'_>,
    content: &[u8],
    param_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // The `constraint` node has exactly one named `type` child.
    let mut cursor = constraint_node.walk();
    for child in constraint_node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        let Ok(type_text) = child.utf8_text(content) else {
            continue;
        };
        let trimmed = type_text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let constraint_id = helper.add_type(trimmed, Some(span_from_node(child)));
        helper.add_typeof_edge_with_context(
            param_id,
            constraint_id,
            Some(TypeOfContext::Constraint),
            None,
            None,
        );
    }
}

/// Emit `References` edges for the `value` (default-type) child of a
/// `type_parameter`.
///
/// The grammar shape is `default_type: '=' type`. The default value
/// becomes a Reference edge from the parameter to the default Type
/// node — symmetric with how `build_type_alias_edges` records type
/// references on the right-hand side of a type alias.
fn emit_type_parameter_default_edges(
    default_node: Node<'_>,
    content: &[u8],
    param_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = default_node.walk();
    for child in default_node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        let Ok(type_text) = child.utf8_text(content) else {
            continue;
        };
        let trimmed = type_text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let default_id = helper.add_type(trimmed, Some(span_from_node(child)));
        helper.add_reference_edge(param_id, default_id);
    }
}

/// Walk a `type_alias_declaration` body searching for
/// `mapped_type_clause` nodes and emit a `Type` node for each binder
/// (the `K` in `[K in keyof T]`).
///
/// AC-4: mapped-type binders are scoped to the enclosing type-alias,
/// so the qualified name is `<TypeAlias>::<BinderName>`. Span is
/// anchored on the binder identifier itself.
fn process_mapped_type_binders(
    type_alias_node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    let Some(value_node) = type_alias_node.child_by_field_name("value") else {
        return;
    };
    walk_for_mapped_binders(value_node, content, parent_qualified_name, helper);
}

/// Recursive descent through type-alias values looking for
/// `mapped_type_clause` nodes. Mapped types can nest inside
/// intersection / union / conditional types, so we walk the whole
/// subtree rather than only inspecting the top-level value child.
fn walk_for_mapped_binders(
    node: Node<'_>,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    if node.kind() == "mapped_type_clause"
        && let Some(name_node) = node.child_by_field_name("name")
        && let Ok(binder_name) = name_node.utf8_text(content)
    {
        let trimmed = binder_name.trim();
        if !trimmed.is_empty() {
            let qualified_binder = format!("{parent_qualified_name}.{trimmed}");
            helper.add_type(&qualified_binder, Some(span_from_node(name_node)));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_mapped_binders(child, content, parent_qualified_name, helper);
    }
}
