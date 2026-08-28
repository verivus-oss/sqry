use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, OnceLock},
};

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::edge::{ExportKind, FfiConvention, HttpMethod};
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span};
use sqry_core::relations::SyntheticNameBuilder;
use tree_sitter::{Node, Tree};

use super::jsdoc_parser::{extract_jsdoc_comment, parse_jsdoc_tags};
use super::local_scopes;
use super::type_extractor::{canonical_type_string, extract_type_names};

const DEFAULT_SCOPE_DEPTH: usize = 4;
type CallEdgeData = (NodeId, NodeId, u8, bool, Option<Span>);
type ConstructorEdgeData = (NodeId, NodeId, u8, Option<Span>);

/// Graph builder for JavaScript files using unified `CodeGraph` architecture.
#[derive(Debug, Clone, Copy)]
pub struct JavaScriptGraphBuilder {
    max_scope_depth: usize,
}

impl Default for JavaScriptGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl JavaScriptGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

/// Infer visibility from JavaScript naming convention.
/// Functions/methods starting with underscore are considered private.
fn infer_visibility(qualified_name: &str) -> &'static str {
    // For qualified names like "MyClass._privateMethod", check the method name part
    let name_part = qualified_name.rsplit('.').next().unwrap_or(qualified_name);
    if name_part.starts_with('_') {
        "private"
    } else {
        "public"
    }
}

impl GraphBuilder for JavaScriptGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Initialize the helper for this file
        let mut helper = GraphBuildHelper::new(staging, file, Language::JavaScript);
        let file_arc = Arc::from(file.to_string_lossy().to_string());

        // Build AST graph for context resolution
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Create function/method nodes for all callables
        for context in ast_graph.contexts() {
            let span = Some(context.decl_span);
            // Infer visibility from naming convention: leading underscore = private
            let visibility = infer_visibility(&context.qualified_name);

            // Determine if this is a method (contains a dot indicating it's in a class)
            if context.qualified_name.contains('.') {
                helper.add_method_with_visibility(
                    &context.qualified_name,
                    span,
                    context.is_async,
                    false, // is_static - we don't track this in CallContext
                    Some(visibility),
                );
            } else {
                helper.add_function_with_visibility(
                    &context.qualified_name,
                    span,
                    context.is_async,
                    false, // is_unsafe - N/A for JavaScript
                    Some(visibility),
                );
            }
        }

        // Build local scope tree for variable reference resolution
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Walk the AST to find and build edges
        let mut cursor = tree.root_node().walk();
        extract_edges_recursive(
            tree.root_node(),
            &mut cursor,
            content,
            &file_arc,
            &ast_graph,
            &mut helper,
            &mut scope_tree,
        )?;

        // Second pass: Process JSDoc annotations for TypeOf and Reference edges
        process_jsdoc_annotations(tree.root_node(), content, &mut helper)?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(javascript_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for JavaScript.
///
/// Holds a precomputed `kind_id -> CfBucket` table built once from the
/// tree-sitter-javascript grammar and shared process-wide via
/// [`javascript_shape_mapping`]. Everything except this mapping is the one
/// shared `compute_shape_descriptor` routine.
pub struct JavaScriptShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl JavaScriptShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-javascript grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let count = lang.node_kind_count();
        let mut cf_by_kind_id = vec![None; count];
        for (id, slot) in cf_by_kind_id.iter_mut().enumerate() {
            let Ok(kind_id) = u16::try_from(id) else {
                break;
            };
            if !lang.node_kind_is_named(kind_id) {
                continue;
            }
            if let Some(name) = lang.node_kind_for_id(kind_id) {
                *slot = cf_bucket_for_javascript_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for JavaScriptShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        if let Some(params) = fn_node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                match child.kind() {
                    // Plain or destructured positional parameter.
                    "identifier" | "object_pattern" | "array_pattern" => {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                    }
                    // `x = 1` default initializer.
                    "assignment_pattern" => {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                        shape.has_defaults = true;
                    }
                    // `...rest` variadic.
                    "rest_pattern" => shape.has_varargs = true,
                    _ => {}
                }
            }
        }
        // JavaScript carries no return-type annotation in the grammar.
        shape
    }
}

/// Map one tree-sitter-javascript grammar node-kind name to its canonical
/// control-flow bucket. Additive-only; the bucket set is frozen.
fn cf_bucket_for_javascript_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "ternary_expression" => CfBucket::Branch,
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => CfBucket::Loop,
        "switch_statement" | "switch_case" | "switch_default" => CfBucket::Match,
        "try_statement" => CfBucket::Try,
        "catch_clause" => CfBucket::Catch,
        "throw_statement" => CfBucket::Throw,
        "return_statement" => CfBucket::Return,
        "yield_expression" => CfBucket::Yield,
        "await_expression" => CfBucket::Await,
        "break_statement" | "continue_statement" => CfBucket::BreakContinue,
        "call_expression" | "new_expression" => CfBucket::Call,
        "lexical_declaration"
        | "variable_declaration"
        | "assignment_expression"
        | "augmented_assignment_expression" => CfBucket::Assign,
        "arrow_function" | "function_expression" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide JavaScript shape mapping, built once on first use.
#[must_use]
pub fn javascript_shape_mapping() -> &'static JavaScriptShapeMapping {
    static MAPPING: OnceLock<JavaScriptShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(JavaScriptShapeMapping::build)
}

/// Recursively extract edges (calls, constructors, imports) from the AST
fn extract_edges_recursive<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    file: &Arc<str>,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut local_scopes::JavaScriptScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        "call_expression" => {
            // Add HTTP request edges when applicable (fetch/axios patterns)
            let _ = build_http_request_edge(ast_graph, node, content, helper);
            // Detect Express/Koa/Fastify route endpoint registrations
            let _ = detect_route_endpoint(node, content, helper);
            // Check for FFI patterns first (WebAssembly, native addons)
            let is_ffi = build_ffi_call_edge(ast_graph, node, content, helper)?;
            if !is_ffi {
                // Not an FFI call - process as regular call
                if let Some((caller_id, callee_id, argument_count, is_async, span)) =
                    build_call_edge_with_helper(ast_graph, node, content, helper)?
                {
                    helper.add_call_edge_full_with_span(
                        caller_id,
                        callee_id,
                        argument_count,
                        is_async,
                        span.into_iter().collect(),
                    );
                }
            }
        }
        "new_expression" => {
            // Check for WebAssembly constructor patterns
            let is_ffi = build_ffi_new_edge(ast_graph, node, content, helper)?;
            if !is_ffi {
                // Not an FFI constructor - process as regular constructor
                if let Some((caller_id, callee_id, argument_count, span)) =
                    build_constructor_edge_with_helper(ast_graph, node, content, helper)?
                {
                    helper.add_call_edge_full_with_span(
                        caller_id,
                        callee_id,
                        argument_count,
                        false,
                        span.into_iter().collect(),
                    );
                }
            }
        }
        "import_statement" => {
            if let Some((from_id, to_id)) =
                build_import_edge_with_helper(node, content, file, helper)?
            {
                helper.add_import_edge(from_id, to_id);
            }
        }
        "export_statement" => {
            build_export_edges_with_helper(node, content, file, helper);
        }
        "expression_statement" => {
            // Check for CommonJS export patterns
            build_commonjs_export_edges(node, content, helper);
        }
        "class_declaration" | "class" => {
            build_inherits_edge_with_helper(node, content, helper);
        }
        "identifier" => {
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recursively process children
    // Collect children into a vec to avoid borrowing issues
    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        extract_edges_recursive(
            child,
            &mut child_cursor,
            content,
            file,
            ast_graph,
            helper,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Build a call edge using `GraphBuildHelper`
fn build_call_edge_with_helper(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<CallEdgeData>> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            // Whole-file synthetic context: start of file is the honest position.
            decl_span: Span::default(),
            is_async: false,
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

    // Normalize optional chain syntax
    let callee_text = if raw_callee_text.contains("?.") {
        normalize_optional_chain(&raw_callee_text)
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
    } else if callee_text.starts_with("super.") || callee_text.contains('.') {
        callee_text.clone()
    } else {
        callee_simple.to_string()
    };

    // Ensure nodes exist using helper
    let source_id = ensure_caller_node(helper, call_context);
    let call_site_span = span_from_node(call_node);
    let target_id = helper.ensure_callee(&target_qname, call_site_span, CalleeKindHint::Function);

    let span = Some(call_site_span);
    let argument_count = u8::try_from(count_arguments(call_node)).unwrap_or(u8::MAX);
    let is_async = check_uses_await(call_node);

    Ok(Some((source_id, target_id, argument_count, is_async, span)))
}

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

    let caller_id = get_caller_node_id(ast_graph, call_node, helper);
    let target_name = info.url.as_ref().map_or_else(
        || format!("http::{}", info.method.as_str()),
        |url| format!("http::{url}"),
    );
    let target_id = helper.add_module(&target_name, Some(span_from_node(call_node)));

    helper.add_http_request_edge(caller_id, target_id, info.method, info.url.as_deref());
    true
}

/// Detect Express/Koa/Fastify-style route endpoint registrations.
///
/// Matches patterns like:
/// - `app.get("/api/users", handler)` -> Endpoint node `route::GET::/api/users`
/// - `router.post("/api/items", handler)` -> Endpoint node `route::POST::/api/items`
/// - `app.delete("/api/items/:id", handler)` -> Endpoint node `route::DELETE::/api/items/:id`
/// - `server.all("/health", handler)` -> Endpoint node `route::ALL::/health`
///
/// The receiver can be any variable name (app, router, server, etc.).
/// Creates an Endpoint node with qualified name `route::METHOD::/path` and a
/// Contains edge from the endpoint to the handler function if identifiable.
///
/// Returns `true` if a route endpoint was detected, `false` otherwise.
fn detect_route_endpoint(
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    // The callee must be a member_expression (e.g., `app.get`)
    let Some(callee) = call_node.child_by_field_name("function") else {
        return false;
    };

    if callee.kind() != "member_expression" {
        return false;
    }

    // Extract the property name (the HTTP method)
    let Some(property) = callee.child_by_field_name("property") else {
        return false;
    };

    let Ok(method_name) = property.utf8_text(content) else {
        return false;
    };
    let method_name = method_name.trim();

    // Map the property name to an HTTP method string for the qualified name
    let method_str = match method_name {
        "get" => "GET",
        "post" => "POST",
        "put" => "PUT",
        "delete" => "DELETE",
        "patch" => "PATCH",
        "all" => "ALL",
        _ => return false,
    };

    // Extract the first argument which should be the route path string
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return false;
    };

    let mut cursor = args.walk();
    let first_arg = args
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","));

    let Some(first_arg) = first_arg else {
        return false;
    };

    // The first argument must be a string literal containing the path
    let Some(path) = extract_string_literal(&first_arg, content) else {
        return false;
    };

    // Build the qualified endpoint name: route::METHOD::/path
    let qualified_name = format!("route::{method_str}::{path}");

    // Create the Endpoint node
    let endpoint_id = helper.add_endpoint(&qualified_name, Some(span_from_node(call_node)));

    // Try to find and link the handler function (second argument)
    // Supports: identifier references, member expressions
    let mut handler_cursor = args.walk();
    let handler_arg = args
        .children(&mut handler_cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","))
        .nth(1);

    if let Some(handler_node) = handler_arg
        && let Ok(handler_text) = handler_node.utf8_text(content)
    {
        let handler_name = handler_text.trim();
        if !handler_name.is_empty()
            && matches!(handler_node.kind(), "identifier" | "member_expression")
        {
            let handler_id = helper.ensure_callee(
                handler_name,
                span_from_node(handler_node),
                CalleeKindHint::Function,
            );
            helper.add_contains_edge(endpoint_id, handler_id);
        }
    }

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

/// Build a constructor edge using `GraphBuildHelper`
fn build_constructor_edge_with_helper(
    ast_graph: &ASTGraph,
    new_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<ConstructorEdgeData>> {
    // Get or create module-level context
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(new_node.id()) {
        ctx
    } else {
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            // Whole-file synthetic context: start of file is the honest position.
            decl_span: Span::default(),
            is_async: false,
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
    let source_id = ensure_caller_node(helper, call_context);
    let new_site_span = span_from_node(new_node);
    let target_id =
        helper.ensure_callee(constructor_simple, new_site_span, CalleeKindHint::Function);

    let span = Some(new_site_span);
    let argument_count = u8::try_from(count_arguments(new_node)).unwrap_or(u8::MAX);

    Ok(Some((source_id, target_id, argument_count, span)))
}

/// Build an import edge using `GraphBuildHelper`
fn build_import_edge_with_helper(
    import_node: Node<'_>,
    content: &[u8],
    file: &Arc<str>,
    helper: &mut GraphBuildHelper,
) -> GraphResult<
    Option<(
        sqry_core::graph::unified::NodeId,
        sqry_core::graph::unified::NodeId,
    )>,
> {
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

    // Resolve the import path
    let resolved_path =
        sqry_core::graph::resolve_import_path(std::path::Path::new(file.as_ref()), &source_text)?;

    // Create module nodes
    let from_id = helper.add_module("<module>", None);
    let to_id = helper.add_import(&resolved_path, Some(span_from_node(import_node)));

    Ok(Some((from_id, to_id)))
}

/// Build export edges from an `export_statement` node.
///
/// Handles all JavaScript/ESM export forms:
/// - `export default foo` -> Default export
/// - `export { name }` -> Named export (Direct)
/// - `export { name as alias }` -> Named export with alias
/// - `export * from 'module'` -> Wildcard re-export
/// - `export { name } from 'module'` -> Named re-export
/// - `export * as ns from 'module'` -> Namespace re-export
/// - `export function/class/const` -> Declaration exports (Direct)
#[allow(clippy::too_many_lines)]
fn build_export_edges_with_helper(
    export_node: Node<'_>,
    content: &[u8],
    file: &Arc<str>,
    helper: &mut GraphBuildHelper,
) {
    // Get the module node (exporter)
    let module_id = helper.add_module("<module>", None);

    // Check for re-export: has a "source" (from clause)
    let source_node = export_node.child_by_field_name("source");
    let is_reexport = source_node.is_some();

    // Check for default export
    let has_default = export_node
        .children(&mut export_node.walk())
        .any(|child| child.kind() == "default");

    // Check for namespace export: `export * as ns from 'module'`
    let namespace_export = export_node
        .children(&mut export_node.walk())
        .find(|child| child.kind() == "namespace_export");

    // Check for wildcard: `export * from 'module'`
    let has_wildcard = export_node
        .children(&mut export_node.walk())
        .any(|child| child.kind() == "*");

    // Check for export clause: `export { foo, bar }`
    let export_clause = export_node
        .children(&mut export_node.walk())
        .find(|child| child.kind() == "export_clause");

    // Check for declaration export: `export function/class/const/let/var`
    let declaration = export_node.children(&mut export_node.walk()).find(|child| {
        matches!(
            child.kind(),
            "function_declaration"
                | "class_declaration"
                | "lexical_declaration"
                | "variable_declaration"
                | "generator_function_declaration"
        )
    });

    if has_default {
        // Default export: `export default foo` or `export default function foo() {}`
        // Find the exported item (identifier, function, class, etc.)
        let exported_name = if let Some(ref decl) = declaration {
            // export default function foo() {} or export default class Bar {}
            decl.child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map_or_else(|| "default".to_string(), |s| s.trim().to_string())
        } else {
            // export default identifier
            export_node
                .children(&mut export_node.walk())
                .find(|child| child.kind() == "identifier")
                .and_then(|n| n.utf8_text(content).ok())
                .map_or_else(|| "default".to_string(), |s| s.trim().to_string())
        };

        let exported_id = helper.add_function(&exported_name, None, false, false);
        if declaration
            .as_ref()
            .is_some_and(|decl| matches!(decl.kind(), "function_declaration" | "class_declaration"))
        {
            helper.mark_definition(exported_id);
        }
        helper.add_export_edge_full(module_id, exported_id, ExportKind::Default, None);
    } else if let Some(ns_export) = namespace_export {
        // Namespace re-export: `export * as ns from 'module'`
        // Get the namespace alias from the namespace_export node
        let alias = ns_export
            .children(&mut ns_export.walk())
            .find(|child| child.kind() == "identifier")
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string());

        // Create a node representing the source module
        let source_path = source_node
            .and_then(|s| s.utf8_text(content).ok())
            .map_or_else(
                || "<unknown>".to_string(),
                |s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string(),
            );

        let resolved_path = sqry_core::graph::resolve_import_path(
            std::path::Path::new(file.as_ref()),
            &source_path,
        )
        .unwrap_or(source_path);

        let source_module_id = helper.add_module(&resolved_path, None);
        helper.add_export_edge_full(
            module_id,
            source_module_id,
            ExportKind::Namespace,
            alias.as_deref(),
        );
    } else if has_wildcard && is_reexport {
        // Wildcard re-export: `export * from 'module'`
        let source_path = source_node
            .and_then(|s| s.utf8_text(content).ok())
            .map_or_else(
                || "<unknown>".to_string(),
                |s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string(),
            );

        let resolved_path = sqry_core::graph::resolve_import_path(
            std::path::Path::new(file.as_ref()),
            &source_path,
        )
        .unwrap_or(source_path);

        let source_module_id = helper.add_module(&resolved_path, None);
        // Wildcard re-export uses Reexport kind with no alias
        helper.add_export_edge_full(module_id, source_module_id, ExportKind::Reexport, None);
    } else if let Some(clause) = export_clause {
        // Named exports: `export { foo, bar }` or `export { foo } from 'module'`
        let mut cursor = clause.walk();
        for child in clause.children(&mut cursor) {
            if child.kind() == "export_specifier" {
                // Get the identifiers from the export specifier
                // First identifier is the local name, second (if present) is the alias
                let identifiers: Vec<_> = child
                    .children(&mut child.walk())
                    .filter(|n| n.kind() == "identifier")
                    .collect();

                if let Some(first_ident) = identifiers.first() {
                    let local_name = first_ident
                        .utf8_text(content)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();

                    if local_name.is_empty() {
                        continue;
                    }

                    // Check if there's an alias (second identifier)
                    let alias = identifiers.get(1).and_then(|n| {
                        n.utf8_text(content)
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                    });

                    let exported_id = helper.add_function(&local_name, None, false, false);

                    let kind = if is_reexport {
                        ExportKind::Reexport
                    } else {
                        ExportKind::Direct
                    };

                    helper.add_export_edge_full(module_id, exported_id, kind, alias.as_deref());
                }
            }
        }
    } else if let Some(decl) = declaration {
        // Declaration export: `export function foo() {}` or `export const x = 1;`
        match decl.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name_node) = decl.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        let exported_id = helper.add_function(&name, None, false, false);
                        helper.mark_definition(exported_id);
                        helper.add_export_edge_full(
                            module_id,
                            exported_id,
                            ExportKind::Direct,
                            None,
                        );
                    }
                }
            }
            "class_declaration" => {
                if let Some(name_node) = decl.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        let exported_id = helper.add_class(&name, None);
                        helper.mark_definition(exported_id);
                        helper.add_export_edge_full(
                            module_id,
                            exported_id,
                            ExportKind::Direct,
                            None,
                        );
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // export const/let/var - can have multiple declarators
                let mut cursor = decl.walk();
                for child in decl.children(&mut cursor) {
                    if child.kind() == "variable_declarator"
                        && let Some(name_node) = child.child_by_field_name("name")
                        && let Ok(name) = name_node.utf8_text(content)
                    {
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            let exported_id = helper.add_variable(&name, None);
                            helper.mark_definition(exported_id);
                            helper.add_export_edge_full(
                                module_id,
                                exported_id,
                                ExportKind::Direct,
                                None,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Build export edges for `CommonJS` patterns.
///
/// Handles:
/// - `module.exports = { foo, bar }` -> Named exports from object literal
/// - `module.exports = foo` -> Default export
/// - `exports.foo = bar` -> Named export
/// - `module.exports.foo = bar` -> Named export
fn build_commonjs_export_edges(
    expr_stmt_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Get the assignment expression from the expression statement
    let Some(assignment) = expr_stmt_node
        .children(&mut expr_stmt_node.walk())
        .find(|child| child.kind() == "assignment_expression")
    else {
        return;
    };

    let Some(left) = assignment.child_by_field_name("left") else {
        return;
    };
    let Some(right) = assignment.child_by_field_name("right") else {
        return;
    };

    let left_text = left.utf8_text(content).ok().map(|s| s.trim().to_string());
    let Some(left_text) = left_text else {
        return;
    };

    let module_id = helper.add_module("<module>", None);

    // Pattern 1: `module.exports = { foo, bar }` or `module.exports = foo`
    if left_text == "module.exports" {
        if right.kind() == "object" {
            // Object literal: export each property as a named export
            let mut cursor = right.walk();
            for child in right.children(&mut cursor) {
                if child.kind() == "shorthand_property_identifier" {
                    // `{ foo }` - shorthand, name is both local and exported
                    if let Ok(name) = child.utf8_text(content) {
                        let name = name.trim();
                        if !name.is_empty() {
                            let exported_id = helper.add_function(name, None, false, false);
                            helper.add_export_edge_full(
                                module_id,
                                exported_id,
                                ExportKind::Direct,
                                None,
                            );
                        }
                    }
                } else if child.kind() == "pair" {
                    // `{ foo: bar }` - key is export name, value is local
                    if let Some(key_node) = child.child_by_field_name("key")
                        && let Ok(export_name) = key_node.utf8_text(content)
                    {
                        let export_name = export_name.trim();
                        if !export_name.is_empty() {
                            let exported_id = helper.add_function(export_name, None, false, false);
                            helper.add_export_edge_full(
                                module_id,
                                exported_id,
                                ExportKind::Direct,
                                None,
                            );
                        }
                    }
                } else if child.kind() == "spread_element" {
                    // `{ ...other }` - spread export (complex to resolve statically)
                }
            }
        } else if right.kind() == "identifier" || right.kind() == "member_expression" {
            // Single value export: `module.exports = foo` -> default export
            let export_name = right
                .utf8_text(content)
                .ok()
                .map_or_else(|| "default".to_string(), |s| s.trim().to_string());

            if !export_name.is_empty() {
                let exported_id = helper.add_function(&export_name, None, false, false);
                helper.add_export_edge_full(module_id, exported_id, ExportKind::Default, None);
            }
        } else if matches!(
            right.kind(),
            "function_expression"
                | "arrow_function"
                | "class"
                | "call_expression"
                | "new_expression"
        ) {
            // Anonymous/inline export: `module.exports = function() {}` -> default export
            let exported_id = helper.add_function("default", None, false, false);
            helper.mark_definition(exported_id);
            helper.add_export_edge_full(module_id, exported_id, ExportKind::Default, None);
        }
    }
    // Pattern 2: `exports.foo = bar` or `module.exports.foo = bar`
    else if left_text.starts_with("exports.") || left_text.starts_with("module.exports.") {
        // Extract the property name being exported
        let export_name = if let Some(name) = left_text.strip_prefix("module.exports.") {
            name
        } else if let Some(name) = left_text.strip_prefix("exports.") {
            name
        } else {
            return;
        };

        if !export_name.is_empty() {
            let exported_id = helper.add_function(export_name, None, false, false);
            helper.add_export_edge_full(module_id, exported_id, ExportKind::Direct, None);
        }
    }
}

/// Build inherits edge for class declarations with extends clause.
///
/// Handles:
/// - `class Child extends Parent {}` (`class_declaration` with simple identifier)
/// - `class Child extends Module.Parent {}` (`class_declaration` with qualified path)
/// - `const Foo = class extends Base {}` (class expression)
fn build_inherits_edge_with_helper(
    class_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Look for class_heritage child which contains the extends clause
    let heritage = class_node
        .children(&mut class_node.walk())
        .find(|child| child.kind() == "class_heritage");

    let Some(heritage_node) = heritage else {
        return; // No inheritance
    };

    // Get the class name
    let class_name = if class_node.kind() == "class_declaration" {
        class_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string())
    } else {
        // For class expressions, try to get the name from parent variable_declarator
        class_node
            .parent()
            .filter(|p| p.kind() == "variable_declarator")
            .and_then(|p| p.child_by_field_name("name"))
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string())
            .or_else(|| {
                // Anonymous class expression - use synthetic name
                Some(SyntheticNameBuilder::from_node_with_hash(
                    &class_node,
                    content,
                    "class",
                ))
            })
    };

    // Get the parent class name from heritage
    // Handles both simple identifiers and qualified paths (member_expression)
    let parent_name = extract_parent_class_name(heritage_node, content);

    // Only create edge if we have both names
    if let (Some(child_name), Some(parent_name)) = (class_name, parent_name)
        && !child_name.is_empty()
        && !parent_name.is_empty()
    {
        let child_id = helper.add_class(&child_name, None);
        let parent_id = helper.add_class(&parent_name, None);
        helper.add_inherits_edge(child_id, parent_id);
    }
}

/// Extract the parent class name from a `class_heritage` node.
///
/// Handles:
/// - Simple identifier: `extends Parent` -> "Parent"
/// - Member expression: `extends Module.Parent` -> "Module.Parent"
/// - Nested member: `extends a.b.c.Parent` -> "a.b.c.Parent"
/// - Call expression: `extends mixin(Base)` -> "mixin(Base)" (full expression for clarity)
///
/// **Note on mixin patterns**: For call expressions like `extends mixin(Base)` or
/// `extends WithLogging(Component)`, we store the full expression text rather than
/// just the function name. This provides clearer semantics for consumers:
/// - The node name shows the actual mixin composition
/// - Graph queries can distinguish `mixin(A)` from `mixin(B)`
/// - The pattern remains compatible with standard inheritance queries
fn extract_parent_class_name(heritage_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = heritage_node.walk();
    for child in heritage_node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // Simple extends: `extends Parent`
                return child.utf8_text(content).ok().map(|s| s.trim().to_string());
            }
            "member_expression" => {
                // Qualified extends: `extends Module.Parent` or `extends a.b.c.Parent`
                // Get the full text of the member expression
                return child.utf8_text(content).ok().map(|s| s.trim().to_string());
            }
            "call_expression" => {
                // Mixin pattern: `extends mixin(Base)` or `extends WithLogging(Component)`
                // Store full call expression for semantic clarity - consumers can see
                // the actual composition, not just the mixin factory function name.
                // This avoids ambiguity when the same mixin is used with different bases.
                return child.utf8_text(content).ok().map(|s| s.trim().to_string());
            }
            _ => {}
        }
    }
    None
}

fn simple_name(name: &str) -> &str {
    // Split on . and / to get the last segment of a qualified name
    // Do NOT split on '?' - it's part of ternary (?:) and nullish coalescing (??) operators
    name.rsplit(['.', '/']).next().unwrap_or(name)
}

/// Normalizes optional chain syntax by removing `?.` operators
/// Converts `user?.getName` to `user.getName` for consistent processing
/// Preserves standalone `?` characters (from ternary/nullish operators) by only replacing `?.`
fn normalize_optional_chain(text: &str) -> String {
    text.replace("?.", ".")
        .trim()
        .trim_end_matches('.')
        .to_string()
}

fn check_uses_await(call_node: Node<'_>) -> bool {
    // Check if the call_node's parent is an await_expression
    let mut current = call_node;
    for _ in 0..2 {
        // Check up to 2 levels up
        if let Some(parent) = current.parent() {
            if parent.kind() == "await_expression" {
                return true;
            }
            current = parent;
        } else {
            break;
        }
    }
    false
}

fn count_arguments(node: Node<'_>) -> usize {
    node.child_by_field_name("arguments").map_or(0, |args| {
        let mut count = 0;
        let mut cursor = args.walk();
        for child in args.children(&mut cursor) {
            if !matches!(child.kind(), "(" | ")" | ",") {
                count += 1;
            }
        }
        count
    })
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

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

// ========== ASTGraph: Pre-computed AST metadata ==========

#[derive(Debug, Clone)]
pub struct CallContext {
    pub qualified_name: String,
    /// Real line/column span of the declaration.
    pub decl_span: Span,
    pub is_async: bool,
}

impl CallContext {
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
}

pub struct ASTGraph {
    /// Maps node ID to its enclosing callable node ID
    callable_map: HashMap<usize, usize>,
    /// Maps callable node ID to its context (name, scope, etc.)
    context_map: HashMap<usize, CallContext>,
}

impl ASTGraph {
    /// Build the graph structure from the AST in a single O(n) pass
    pub fn from_tree(tree: &Tree, content: &[u8], max_scope_depth: usize) -> Result<Self, String> {
        let mut builder = ASTGraphBuilder::new(content, max_scope_depth);

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        builder
            .visit(tree.root_node(), None, &mut guard)
            .map_err(|e| format!("JavaScript AST traversal hit recursion limit: {e}"))?;
        Ok(builder.build())
    }

    /// Get the enclosing callable context for a node (O(1) lookup)
    pub fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        let callable_id = self.callable_map.get(&node_id)?;
        self.context_map.get(callable_id)
    }

    /// Get all callable contexts
    pub fn contexts(&self) -> impl Iterator<Item = &CallContext> {
        self.context_map.values()
    }
}

struct ASTGraphBuilder<'a> {
    content: &'a [u8],
    max_scope_depth: usize,
    callable_map: HashMap<usize, usize>,
    context_map: HashMap<usize, CallContext>,
    current_scope: Vec<Arc<str>>,
}

impl<'a> ASTGraphBuilder<'a> {
    fn new(content: &'a [u8], max_scope_depth: usize) -> Self {
        Self {
            content,
            max_scope_depth,
            callable_map: HashMap::new(),
            context_map: HashMap::new(),
            current_scope: Vec::new(),
        }
    }

    fn build(self) -> ASTGraph {
        ASTGraph {
            callable_map: self.callable_map,
            context_map: self.context_map,
        }
    }

    /// # Errors
    ///
    /// Returns [`sqry_core::query::security::RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
    fn visit(
        &mut self,
        node: Node<'_>,
        parent_callable: Option<usize>,
        guard: &mut sqry_core::query::security::RecursionGuard,
    ) -> Result<(), sqry_core::query::security::RecursionError> {
        guard.enter()?;

        let node_id = node.id();

        // Check if this node is a callable (function, method, arrow function)
        let callable_name = callable_node_name(node, self.content);

        let new_callable = if let Some(name) = callable_name {
            // This is a callable - create context
            let is_async = is_async_function(node, self.content);

            let qualified_name = if self.current_scope.is_empty() {
                name.clone()
            } else if self.current_scope.len() <= self.max_scope_depth {
                format!("{}.{}", self.current_scope.join("."), name)
            } else {
                // Truncate deep scopes
                let truncated = &self.current_scope[..self.max_scope_depth];
                format!("{}.{}", truncated.join("."), name)
            };

            let context = CallContext {
                qualified_name,
                decl_span: Span::from_node(&node),
                is_async,
            };

            self.context_map.insert(node_id, context);
            Some(node_id)
        } else {
            None
        };

        // Use new callable context if we entered one, otherwise inherit parent's
        let effective_callable = new_callable.or(parent_callable);

        // Map this node to its enclosing callable
        if let Some(callable_id) = effective_callable {
            self.callable_map.insert(node_id, callable_id);
        }

        // Handle scope tracking (classes, objects, etc.)
        let scope_name = scope_node_name(node, self.content);
        let pushed_scope = if let Some(name) = scope_name {
            self.current_scope.push(Arc::from(name));
            true
        } else {
            false
        };

        // Recursively visit children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, effective_callable, guard)?;
        }

        // Pop scope if we pushed one
        if pushed_scope {
            self.current_scope.pop();
        }

        guard.exit();
        Ok(())
    }
}

/// Check if a node represents a callable (function, method, arrow function, etc.)
fn callable_node_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string())),
        "function_expression" | "generator_function" => {
            // Named function expression
            node.child_by_field_name("name")
                .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string()))
                .or_else(|| {
                    Some(SyntheticNameBuilder::from_node_with_hash(
                        &node, content, "function",
                    ))
                })
        }
        "arrow_function" => {
            // FR-JS-PATCH-1/2 compliance: Differentiate truly anonymous vs variable-assigned
            // Variable-assigned: const foo = () => {} → use "foo" (declared name)
            // Truly anonymous: [].map(() => {}) → use anon:arrow:<hash> (FR-JS-PATCH-2)
            if let Some(parent) = node.parent()
                && parent.kind() == "variable_declarator"
                && let Some(name_node) = parent.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
            {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            // Fallback: truly anonymous arrow functions (callbacks, IIFEs, etc.)
            // Use hash-based synthetic naming per FR-JS-PATCH-2
            Some(SyntheticNameBuilder::from_node_with_hash(
                &node, content, "arrow",
            ))
        }
        "method_definition" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string())),
        _ => None,
    }
}

fn scope_node_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "class_declaration" | "class" => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(content).ok().map(|s| s.trim().to_string()))
            .or_else(|| {
                Some(SyntheticNameBuilder::from_node_with_hash(
                    &node, content, "class",
                ))
            }),
        _ => None,
    }
}

fn is_async_function(node: Node<'_>, _content: &[u8]) -> bool {
    // Check if function has async modifier
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "async")
}

// ========== JSDoc TypeOf/Reference Processing ==========

/// Process `JSDoc` annotations to create `TypeOf` and Reference edges
/// This is a post-processing pass that runs after all nodes are created
fn process_jsdoc_annotations(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Recursively walk the tree looking for nodes with JSDoc
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            process_function_jsdoc(node, content, helper)?;
        }
        "method_definition" => {
            process_method_jsdoc(node, content, helper)?;
        }
        "lexical_declaration" | "variable_declaration" => {
            process_variable_jsdoc(node, content, helper)?;
        }
        "class_declaration" | "class" => {
            process_class_fields(node, content, helper)?;
            process_constructor_this_assignments(node, content, helper)?;
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        process_jsdoc_annotations(child, content, helper)?;
    }

    Ok(())
}

/// Process `JSDoc` for function declarations
fn process_function_jsdoc(
    func_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract JSDoc comment
    let Some(jsdoc_text) = extract_jsdoc_comment(func_node, content) else {
        return Ok(());
    };

    // Parse JSDoc tags
    let tags = parse_jsdoc_tags(&jsdoc_text);

    // Get function name
    let Some(name_node) = func_node.child_by_field_name("name") else {
        return Ok(());
    };

    let function_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(func_node),
            reason: "failed to read function name".to_string(),
        })?
        .trim()
        .to_string();

    if function_name.is_empty() {
        return Ok(());
    }

    // Get or create function node
    let func_node_id = helper.ensure_callee(
        &function_name,
        span_from_node(func_node),
        CalleeKindHint::Function,
    );

    // ISSUE 1 FIX: Extract AST parameter list with indices
    // Map JSDoc tags to AST parameters by name, use AST index (not JSDoc order)
    let ast_params = extract_ast_parameters(func_node, content);
    let ast_param_map: HashMap<&str, usize> = ast_params
        .iter()
        .map(|(idx, name)| (name.as_str(), *idx))
        .collect();

    // Process @param tags - map to AST indices by name
    for param_tag in &tags.params {
        // Find AST index for this JSDoc parameter name
        // Handle optional params [name], rest params ...name, dotted names (options.foo)
        let mut normalized_name = param_tag
            .name
            .trim_start_matches("...")
            .trim_matches(|c| c == '[' || c == ']');

        // Handle dotted parameter names (e.g., "options.name" -> "options")
        // For property-path JSDoc tags, use the base parameter name
        if let Some(base_name) = normalized_name.split('.').next() {
            normalized_name = base_name;
        }

        let Some(&ast_index) = ast_param_map.get(normalized_name) else {
            // JSDoc tag doesn't match any AST parameter - skip it
            continue;
        };

        // Create TypeOf edge: function -> parameter type
        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            func_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            ast_index.try_into().ok(), // Use AST index, not JSDoc order
            Some(&param_tag.name),
        );

        // Create Reference edges: function -> each referenced type
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(func_node_id, ref_type_id);
        }
    }

    // Process @returns tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            func_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges for return type
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(func_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process `JSDoc` for method definitions
fn process_method_jsdoc(
    method_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract JSDoc comment
    let Some(jsdoc_text) = extract_jsdoc_comment(method_node, content) else {
        return Ok(());
    };

    // Parse JSDoc tags
    let tags = parse_jsdoc_tags(&jsdoc_text);

    // Get method name
    let Some(name_node) = method_node.child_by_field_name("name") else {
        return Ok(());
    };

    let method_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(method_node),
            reason: "failed to read method name".to_string(),
        })?
        .trim()
        .to_string();

    if method_name.is_empty() {
        return Ok(());
    }

    // Find the class name by walking up the tree
    let class_name = get_enclosing_class_name(method_node, content)?;
    let Some(class_name) = class_name else {
        return Ok(());
    };

    // Create qualified method name: ClassName.methodName
    let qualified_name = format!("{class_name}.{method_name}");

    // Get existing method node (should already exist from main traversal)
    // Use ensure_method to handle case where it might not exist yet
    let method_node_id = helper.ensure_method(&qualified_name, None, false, false);

    // ISSUE 1 FIX: Extract AST parameter list with indices
    // Map JSDoc tags to AST parameters by name, use AST index (not JSDoc order)
    let ast_params = extract_ast_parameters(method_node, content);
    let ast_param_map: HashMap<&str, usize> = ast_params
        .iter()
        .map(|(idx, name)| (name.as_str(), *idx))
        .collect();

    // Process @param tags - map to AST indices by name
    for param_tag in &tags.params {
        // Find AST index for this JSDoc parameter name
        // Handle optional params [name], rest params ...name, dotted names (options.foo)
        let mut normalized_name = param_tag
            .name
            .trim_start_matches("...")
            .trim_matches(|c| c == '[' || c == ']');

        // Handle dotted parameter names (e.g., "options.name" -> "options")
        // For property-path JSDoc tags, use the base parameter name
        if let Some(base_name) = normalized_name.split('.').next() {
            normalized_name = base_name;
        }

        let Some(&ast_index) = ast_param_map.get(normalized_name) else {
            // JSDoc tag doesn't match any AST parameter - skip it
            continue;
        };

        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            ast_index.try_into().ok(), // Use AST index, not JSDoc order
            Some(&param_tag.name),
        );

        // Create Reference edges
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    // Process @returns tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process `JSDoc` @type annotations for variables
fn process_variable_jsdoc(
    decl_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Check if this is a top-level variable (not inside a function)
    if !is_top_level_variable(decl_node) {
        return Ok(());
    }

    // Extract JSDoc comment
    let Some(jsdoc_text) = extract_jsdoc_comment(decl_node, content) else {
        return Ok(());
    };

    // Parse JSDoc tags
    let tags = parse_jsdoc_tags(&jsdoc_text);

    // Only process if there's a @type annotation
    let Some(type_annotation) = &tags.type_annotation else {
        return Ok(());
    };

    // Find all variable declarators in this declaration
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let var_name = name_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: span_from_node(child),
                    reason: "failed to read variable name".to_string(),
                })?
                .trim()
                .to_string();

            if !var_name.is_empty() {
                // Get or create variable node
                let var_node_id = helper.add_variable(&var_name, None);

                // Create TypeOf edge
                let canonical_type = canonical_type_string(type_annotation);
                let type_node_id = helper.add_type(&canonical_type, None);
                helper.add_typeof_edge_with_context(
                    var_node_id,
                    type_node_id,
                    Some(TypeOfContext::Variable),
                    None,
                    None,
                );

                // Create Reference edges
                let type_names = extract_type_names(type_annotation);
                for type_name in type_names {
                    let ref_type_id = helper.add_type(&type_name, None);
                    helper.add_reference_edge(var_node_id, ref_type_id);
                }
            }
        }
    }

    Ok(())
}

/// Resolve the class name for a `class_declaration` or `class` expression node.
///
/// For named classes: reads the `name` field child.
/// For anonymous class expressions: falls back to the binding identifier when
/// the class is assigned to a `variable_declarator` or `assignment_expression`
/// (mirrors the historic behaviour of `process_class_fields_jsdoc`).
///
/// Returns `None` for anonymous classes that are not bound to an identifier
/// (e.g. immediately invoked or passed as an argument). Callers must skip
/// emission in that case to avoid creating ill-formed `Class.field` names.
fn resolve_class_name_for_fields(
    class_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<String>> {
    if let Some(name_node) = class_node.child_by_field_name("name") {
        let name = name_node
            .utf8_text(content)
            .map_err(|_| GraphBuilderError::ParseError {
                span: span_from_node(class_node),
                reason: "failed to read class name".to_string(),
            })?
            .trim()
            .to_string();
        if name.is_empty() {
            return Ok(None);
        }
        return Ok(Some(name));
    }

    // Anonymous class expression — try to find the binding identifier.
    let Some(parent) = class_node.parent() else {
        return Ok(None);
    };

    match parent.kind() {
        "variable_declarator" => {
            if let Some(name_node) = parent.child_by_field_name("name")
                && let Ok(var_name) = name_node.utf8_text(content)
            {
                let var_name = var_name.trim().to_string();
                if var_name.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(var_name));
            }
            Ok(None)
        }
        "assignment_expression" => {
            if let Some(left) = parent.child_by_field_name("left")
                && let Ok(assign_name) = left.utf8_text(content)
            {
                let assign_name = assign_name.trim().to_string();
                if assign_name.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(assign_name));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Emit Property nodes for every `field_definition` in a class body, and
/// optionally enrich them with `TypeOf{Field}` + `References` edges when a
/// `JSDoc` `@type` annotation is present (REQ:R0001..R0006, R0008, R0023).
///
/// Replaces the historic JSDoc-gated `process_class_fields_jsdoc` function:
/// emission is now unconditional. `JSDoc`, when present, is treated as
/// enrichment for the type edge rather than a gate.
///
/// AC mapping:
/// - AC-1 unconditional Property emission on every `field_definition`
/// - AC-2 span sourced from the field-definition node
/// - AC-3 `static` modifier → `is_static = true`
/// - AC-4 `private_property_identifier` (`#name`) → visibility = "private"
/// - AC-5 `TypeOf` edge name = bare field name (not `Class.field`)
/// - AC-7 `JSDoc` `@type` is preserved as enrichment, not a gate
fn process_class_fields(
    class_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let Some(class_name) = resolve_class_name_for_fields(class_node, content)? else {
        return Ok(());
    };

    let Some(body_node) = class_node.child_by_field_name("body") else {
        return Ok(());
    };

    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        if child.kind() != "field_definition" {
            continue;
        }
        emit_class_field_node(child, content, helper, &class_name)?;
    }

    Ok(())
}

/// Emit a single class field as a Property node and (when `JSDoc` `@type` is
/// present) the corresponding `TypeOf{Field}` + Reference edges.
fn emit_class_field_node(
    field_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_name: &str,
) -> GraphResult<()> {
    // Field name lives under the `property` field for `field_definition` in
    // tree-sitter-javascript. The child node is either an identifier or a
    // `private_property_identifier` (the `#name` form).
    let Some(name_node) = field_node.child_by_field_name("property") else {
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

    // Scan modifier-like direct children. tree-sitter-javascript surfaces
    // `static` as an anonymous keyword child of `field_definition`; there is
    // no accessibility-modifier surface in the JS grammar (visibility is
    // inferred from the `#`-prefix only).
    let mut is_static = false;
    let mut mod_cursor = field_node.walk();
    for modifier in field_node.children(&mut mod_cursor) {
        if modifier.kind() == "static" {
            is_static = true;
        }
    }

    // Per design §3.3 + AC-4: JS field visibility is syntactic.
    // `#`-prefix → "private"; otherwise → "public". Underscore-prefix
    // naming heuristics (e.g. `_foo`) are deliberately NOT applied at the
    // field call site — the field contract is grammar-level, not
    // naming-convention-based.
    let visibility: Option<&str> = if is_hash_private {
        Some("private")
    } else {
        Some("public")
    };

    let qualified_name = format!("{class_name}.{raw_name}");
    let span = Some(span_from_node(field_node));

    let field_id = helper.add_property_with_static_and_visibility(
        &qualified_name,
        span,
        is_static,
        visibility,
    );

    // JSDoc `@type` is now enrichment, not a gate. When present, emit the
    // `TypeOf{Field}` edge with the BARE field name (AC-5) and add
    // Reference edges for every named type appearing in the annotation.
    if let Some(jsdoc_text) = extract_jsdoc_comment(field_node, content) {
        let tags = parse_jsdoc_tags(&jsdoc_text);
        if let Some(type_annotation) = &tags.type_annotation {
            let canonical_type = canonical_type_string(type_annotation);
            let type_node_id = helper.add_type(&canonical_type, None);
            helper.add_typeof_edge_with_context(
                field_id,
                type_node_id,
                Some(TypeOfContext::Field),
                None,
                Some(&raw_name),
            );

            let type_names = extract_type_names(type_annotation);
            for type_name in type_names {
                let ref_type_id = helper.add_type(&type_name, None);
                helper.add_reference_edge(field_id, ref_type_id);
            }
        }
    }

    Ok(())
}

/// Walk a class body and, for every constructor body, emit Property nodes
/// for each `this.<identifier> = ...` assignment encountered (AC-6).
///
/// The walker recurses through all assignment expressions in the constructor
/// body — including those inside nested arrow functions (which inherit
/// `this`). Non-`this` assignments, `this.x.y = ...` deep paths, and
/// computed `this[expr] = ...` accesses are skipped.
///
/// Deduplication with explicit field declarations (FR-13) is handled by the
/// helper's `node_cache`: an existing `Property` with the same canonical
/// qualified name is returned without creating a duplicate node.
fn process_constructor_this_assignments(
    class_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let Some(class_name) = resolve_class_name_for_fields(class_node, content)? else {
        return Ok(());
    };

    let Some(body_node) = class_node.child_by_field_name("body") else {
        return Ok(());
    };

    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        if child.kind() != "method_definition" {
            continue;
        }

        // Only process the constructor — `this.x = ...` in other methods is
        // not necessarily a field declaration site (it may shadow or
        // mutate). Constructor-time assignments are the standard
        // class-field discovery surface.
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let Ok(method_name) = name_node.utf8_text(content) else {
            continue;
        };
        if method_name.trim() != "constructor" {
            continue;
        }

        let Some(method_body) = child.child_by_field_name("body") else {
            continue;
        };

        walk_for_this_assignments(method_body, content, helper, &class_name);
    }

    Ok(())
}

/// Recursively scan a subtree for `assignment_expression` nodes whose left
/// side is `this.<identifier>` and emit Property nodes for the corresponding
/// `Class.<identifier>` qualified names.
fn walk_for_this_assignments(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_name: &str,
) {
    if node.kind() == "assignment_expression"
        && let Some(left) = node.child_by_field_name("left")
        && left.kind() == "member_expression"
        && let Some(object) = left.child_by_field_name("object")
        && object.kind() == "this"
        && let Some(property) = left.child_by_field_name("property")
        && property.kind() == "property_identifier"
        && let Ok(field_name) = property.utf8_text(content)
    {
        let field_name = field_name.trim();
        if !field_name.is_empty() {
            let qualified_name = format!("{class_name}.{field_name}");
            // Span sourced from the `this.<name>` member expression so the
            // node carries a useful location even when the explicit-field
            // path did not run.
            // Per design §3.3 + AC-4: JS field visibility is syntactic.
            // `this.<name>` discovered fields lack a `#`-prefix surface
            // (the property_identifier branch only matches non-private
            // identifiers — private-instance access uses
            // `private_property_identifier` and is filtered out above),
            // so they default to "public".
            let _ = helper.add_property_with_static_and_visibility(
                &qualified_name,
                Some(span_from_node(left)),
                false,
                Some("public"),
            );
        }
    }

    // Recurse into all children. Nested arrow functions are intentionally
    // walked because they inherit `this`. Non-arrow nested functions also
    // recurse, but `this` inside them is rebound so a `this.x = ...` there
    // would be misattributed; this is a known limitation that mirrors the
    // best-effort behaviour of class-field discovery elsewhere in the
    // ecosystem (the JS grammar offers no static way to distinguish at
    // tree-walk time without full scope analysis).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_this_assignments(child, content, helper, class_name);
    }
}

/// Helper: Get enclosing class name for a method
/// Supports both named classes and anonymous classes assigned to variables
/// ISSUE 3 FIX: Handle anonymous class expressions
fn get_enclosing_class_name(method_node: Node, content: &[u8]) -> GraphResult<Option<String>> {
    // Walk up the tree to find the class declaration or expression
    let mut current = method_node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            // Named class declaration
            if let Some(name_node) = parent.child_by_field_name("name") {
                let class_name = name_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(parent),
                        reason: "failed to read class name".to_string(),
                    })?
                    .trim()
                    .to_string();

                if !class_name.is_empty() {
                    return Ok(Some(class_name));
                }
            }
        } else if parent.kind() == "class" {
            // Anonymous class expression - check if assigned to variable
            // Example: const MyClass = class { ... }
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "variable_declarator" {
                    // Get variable name
                    if let Some(name_node) = grandparent.child_by_field_name("name")
                        && let Ok(var_name) = name_node.utf8_text(content)
                    {
                        let var_name = var_name.trim().to_string();
                        if !var_name.is_empty() {
                            return Ok(Some(var_name));
                        }
                    }
                } else if grandparent.kind() == "assignment_expression" {
                    // Assignment: SomeClass = class { ... }
                    if let Some(left) = grandparent.child_by_field_name("left")
                        && let Ok(assign_name) = left.utf8_text(content)
                    {
                        let assign_name = assign_name.trim().to_string();
                        if !assign_name.is_empty() {
                            return Ok(Some(assign_name));
                        }
                    }
                }
            }
            // If anonymous and not assigned, return None
            // (Methods won't get JSDoc edges, but won't crash)
            return Ok(None);
        }
        current = parent;
    }
    Ok(None)
}

/// Extract parameter names and AST indices from function/method parameter list
/// Returns Vec<(`ast_index`, `param_name`)> for mapping `JSDoc` tags to AST positions
fn extract_ast_parameters(func_node: Node, content: &[u8]) -> Vec<(usize, String)> {
    let Some(params_node) = func_node.child_by_field_name("parameters") else {
        return Vec::new();
    };

    let mut cursor = params_node.walk();
    params_node
        .named_children(&mut cursor)
        .enumerate()
        .filter_map(|(ast_index, param)| {
            // Handle different parameter node types
            let param_name = match param.kind() {
                "identifier" => param
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string),
                "required_parameter" | "optional_parameter" => {
                    // Get the pattern node (identifier)
                    param
                        .child_by_field_name("pattern")
                        .and_then(|p| p.utf8_text(content).ok())
                        .map(std::string::ToString::to_string)
                }
                "rest_pattern" => {
                    // Rest parameters: ...args
                    // Get identifier inside rest pattern
                    param
                        .named_child(0)
                        .and_then(|n| n.utf8_text(content).ok())
                        .map(|s| s.trim_start_matches("...").to_string())
                }
                "assignment_pattern" => {
                    // Default parameters: x = 10
                    // Get left side identifier
                    param
                        .child_by_field_name("left")
                        .filter(|left| left.kind() == "identifier")
                        .and_then(|left| left.utf8_text(content).ok())
                        .map(std::string::ToString::to_string)
                }
                _ => None,
            };

            param_name.map(|name| (ast_index, name))
        })
        .collect()
}

/// Helper: Check if a variable declaration is top-level (module-scope only)
/// Excludes variables inside functions, methods, AND block scopes (if, for, while, try, etc.)
fn is_top_level_variable(decl_node: Node) -> bool {
    let mut current = decl_node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            // Functions/methods - not top-level
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition" => return false,

            // Block scopes - not top-level (Issue 2 fix)
            "statement_block" | "if_statement" | "for_statement" | "for_in_statement"
            | "for_of_statement" | "while_statement" | "do_statement" | "try_statement"
            | "catch_clause" | "finally_clause" | "switch_statement" | "switch_case"
            | "switch_default" | "class_body" | "class_static_block" | "with_statement" => {
                return false;
            }

            // Program/module root - is top-level
            // Export statements are top-level
            "program" | "export_statement" => return true,

            _ => {}
        }
        current = parent;
    }
    true
}

// ========== FFI Detection ==========

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
    let caller_id = get_caller_node_id(ast_graph, call_node, helper);

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
    constructor_text: &str,
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get caller context
    let caller_id = get_caller_node_id(ast_graph, new_node, helper);

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

/// Build Import edge for `CommonJS` `require()` calls, plus FFI edge for native addons.
///
/// Creates an Import edge for all `require()` calls (`CommonJS` module system).
/// Additionally creates an FFI edge if the module is a native addon.
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

    // Always create an Import edge for CommonJS require() calls
    let from_id = helper.add_module("<module>", None);

    // Resolve the import path and create import node
    let resolved_path = if path.starts_with('.') {
        // Relative import - resolve against file path
        sqry_core::graph::resolve_import_path(std::path::Path::new(helper.file_path()), &path)
            .unwrap_or_else(|_| simple_name(&path).to_string())
    } else {
        // Package import - use as-is (simple name)
        simple_name(&path).to_string()
    };

    let to_id = helper.add_import(&resolved_path, Some(span_from_node(call_node)));
    helper.add_import_edge(from_id, to_id);

    // Check if this is a native addon (.node file or known native packages)
    let is_native_addon = std::path::Path::new(&path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("node"))
        || is_known_native_addon(&path);

    if is_native_addon {
        // Get caller context
        let caller_id = get_caller_node_id(ast_graph, call_node, helper);

        // Create FFI target node
        let ffi_name = format!("native::{}", simple_name(&path));
        let ffi_node_id = helper.add_module(&ffi_name, Some(span_from_node(call_node)));

        // Add FFI edge with C convention (Node.js native addons use N-API/C ABI)
        helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);
    }

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
    let caller_id = get_caller_node_id(ast_graph, call_node, helper);

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
            |path| format!("native::{}", simple_name(&path)),
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
    helper: &mut GraphBuildHelper,
) -> sqry_core::graph::unified::NodeId {
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(node.id()) {
        ctx
    } else {
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            // Whole-file synthetic context: start of file is the honest position.
            decl_span: Span::default(),
            is_async: false,
        };
        &module_context
    };

    ensure_caller_node(helper, call_context)
}

fn ensure_caller_node(
    helper: &mut GraphBuildHelper,
    call_context: &CallContext,
) -> sqry_core::graph::unified::NodeId {
    let caller_span = Some(call_context.decl_span);
    let qualified_name = call_context.qualified_name();
    if qualified_name.contains('.') {
        helper.ensure_method(qualified_name, caller_span, call_context.is_async, false)
    } else {
        helper.ensure_function(qualified_name, caller_span, call_context.is_async, false)
    }
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
                    return Some(format!("wasm::{}", simple_name(&url)));
                }
            }
        }
    }

    // Check if it's a string literal (file path)
    if let Some(path) = extract_string_literal(&first_arg, content) {
        return Some(format!("wasm::{}", simple_name(&path)));
    }

    None
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
        .any(|&pkg| package_name.contains(pkg))
}

#[cfg(test)]
mod shape_tests {
    use super::{cf_bucket_for_javascript_kind, javascript_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/reference/sample.js"
    ));

    fn parse(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let mut p = tree_sitter::Parser::new();
        p.set_language(&lang).expect("load javascript grammar");
        p.parse(src, None).expect("parse")
    }

    fn function_named<'t>(tree: &'t tree_sitter::Tree, name: &str) -> tree_sitter::Node<'t> {
        let root = tree.root_node();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration"
                && node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(SAMPLE.as_bytes()).ok())
                    == Some(name)
            {
                return node;
            }
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no function_declaration named {name}");
    }

    #[test]
    fn cf_table_is_non_empty() {
        let mapping = javascript_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let mut covered = 0;
        for id in 0..lang.node_kind_count() {
            if mapping.cf_bucket(id as u16).is_some() {
                covered += 1;
            }
        }
        assert!(
            covered >= 10,
            "expected many JS CF kinds mapped, got {covered}"
        );
    }

    #[test]
    fn histogram_covers_real_control_flow() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            javascript_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(!d.is_unhashable());
        for bucket in [
            CfBucket::Branch,
            CfBucket::Loop,
            CfBucket::Match,
            CfBucket::Try,
            CfBucket::Catch,
            CfBucket::Throw,
            CfBucket::Return,
            CfBucket::BreakContinue,
            CfBucket::Call,
            CfBucket::Assign,
            CfBucket::Closure,
        ] {
            assert!(
                d.cf_histogram[bucket.index()] >= 1,
                "classify must exercise {bucket:?}"
            );
        }
    }

    #[test]
    fn async_body_covers_await() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "fetchValue");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            javascript_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(d.cf_histogram[CfBucket::Await.index()] >= 1, "await");
    }

    #[test]
    fn signature_shape_reads_arity_defaults_varargs() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let mapping = javascript_shape_mapping();
        let shape = mapping.signature_shape(func, SAMPLE.as_bytes());
        // classify(values, threshold = 0, ...extra)
        assert_eq!(shape.arity_positional, 2, "values + threshold = 0");
        assert!(shape.has_defaults, "threshold = 0");
        assert!(shape.has_varargs, "...extra");
        assert!(!shape.has_return_annotation, "JS has no return annotation");
    }

    #[test]
    fn unknown_kind_maps_to_none() {
        assert!(cf_bucket_for_javascript_kind("program").is_none());
        assert!(cf_bucket_for_javascript_kind("identifier").is_none());
    }
}
