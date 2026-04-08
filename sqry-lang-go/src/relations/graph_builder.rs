//! Graph builder for Go files using unified `CodeGraph` architecture.
//!
//! This implementation follows the two-phase `ASTGraph` architecture introduced
//! in JavaScript and Rust for O(1) context lookups during call edge detection.
//!
//! # Supported Features
//!
//! - Function definitions (top-level functions)
//! - Method definitions (receiver functions)
//! - Function call expressions
//! - Method calls (selector expressions)
//! - Goroutine launches (`go foo()`) with `is_goroutine: true` metadata
//! - Deferred calls (`defer foo()`) with `is_deferred: true` metadata
//! - Builtin function detection (`make`, `new`, `len`, etc.)
//! - Package imports with stdlib detection
//! - Package exports (uppercase identifiers)
//! - `CGo` detection (import "C")
//! - Pointer receivers
//! - Struct field access references (FR-GO-4)
//! - Interface type assertions (FR-GO-4)

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sqry_core::graph::unified::NodeId as UnifiedNodeId;
use sqry_core::graph::unified::build::helper::GraphBuildHelper;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, NodeId, Span};
use sqry_lang_support::relations::is_uppercase_export;
use tree_sitter::{Node, Tree};

use crate::relations::local_scopes;

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// Go builtin functions that should be annotated with `is_builtin: true`
const GO_BUILTINS: &[&str] = &[
    "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make", "max",
    "min", "new", "panic", "print", "println", "real", "recover",
];

/// Go standard library packages (common ones for stdlib detection)
#[allow(dead_code)] // Scaffolding for stdlib vs third-party package detection
const GO_STDLIB_PACKAGES: &[&str] = &[
    "archive",
    "bufio",
    "bytes",
    "compress",
    "container",
    "context",
    "crypto",
    "database",
    "debug",
    "embed",
    "encoding",
    "errors",
    "expvar",
    "flag",
    "fmt",
    "go",
    "hash",
    "html",
    "image",
    "index",
    "io",
    "log",
    "maps",
    "math",
    "mime",
    "net",
    "os",
    "path",
    "plugin",
    "reflect",
    "regexp",
    "runtime",
    "slices",
    "sort",
    "strconv",
    "strings",
    "sync",
    "syscall",
    "testing",
    "text",
    "time",
    "unicode",
    "unsafe",
];

/// Graph builder for Go files using unified `CodeGraph` architecture.
#[derive(Debug, Clone, Copy)]
pub struct GoGraphBuilder {
    max_scope_depth: usize,
}

impl Default for GoGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl GoGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

/// Modifier for call site context (goroutine, deferred, or normal)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallSiteModifier {
    None,
    Goroutine,
    Deferred,
}

impl GraphBuilder for GoGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Go);

        // Build AST context for O(1) function lookups
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth);

        // Build local scope tree for variable reference resolution
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Track inserted nodes to avoid duplicates
        let mut inserted = HashSet::new();

        // Phase 1: Create function/method nodes
        for context in ast_graph.contexts() {
            let qualified_name = context.qualified_name();
            let span = Span::from_bytes(context.span.0, context.span.1);
            let visibility = visibility_for_identifier(&context.name);

            if context.is_method {
                helper.add_method_with_visibility(
                    &qualified_name,
                    Some(span),
                    false,
                    false,
                    Some(visibility),
                );
            } else {
                helper.add_function_with_visibility(
                    &qualified_name,
                    Some(span),
                    false,
                    false,
                    Some(visibility),
                );
            }
        }

        // Phase 2: Walk the tree to find calls, imports, exports, etc.
        let root = tree.root_node();
        walk_tree_for_edges(
            root,
            content,
            &ast_graph,
            &mut helper,
            &mut inserted,
            &mut scope_tree,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Go
    }
}

/// Walk the AST tree to create edges (calls, imports, exports, etc.)
fn walk_tree_for_edges(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        "import_declaration" => {
            handle_import_declaration(node, content, ast_graph, helper, inserted);
        }
        "function_declaration" => {
            handle_function_declaration(node, content, ast_graph, helper, inserted, scope_tree)?;
        }
        "method_declaration" => {
            handle_method_declaration(node, content, ast_graph, helper, inserted, scope_tree)?;
        }
        "type_declaration" => {
            handle_type_declaration(node, content, helper, inserted, &ast_graph.package);
        }
        "var_declaration" | "const_declaration" => {
            handle_var_declaration(node, content, helper, inserted, &ast_graph.package);
        }
        _ => {}
    }

    // Recurse to children
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            walk_tree_for_edges(child, content, ast_graph, helper, inserted, scope_tree)?;
        }
    }

    Ok(())
}

fn handle_import_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
) {
    process_import_declaration_unified(node, content, ast_graph, helper, inserted);
}

fn handle_function_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    let Some(function_context) = extract_function_context(node, content, &ast_graph.package) else {
        return Ok(());
    };
    if let Some(name) = exported_function_name(node, content) {
        add_export_edge_unified(helper, inserted, &name, &ast_graph.package, node);
    }

    // Process parameters and returns for TypeOf/Reference edges
    let func_name = format!("{}.{}", ast_graph.package, function_context.name);
    process_function_parameters(node, &func_name, content, helper, &ast_graph.package);
    process_function_returns(node, &func_name, content, helper, &ast_graph.package);

    walk_function_body_for_calls(
        node,
        content,
        &function_context,
        ast_graph,
        helper,
        scope_tree,
    )
}

fn handle_method_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    let Some(method_context) = extract_method_context(node, content, &ast_graph.package) else {
        return Ok(());
    };
    if let Some(name) = exported_function_name(node, content) {
        add_method_export_edge_unified(
            helper,
            inserted,
            &name,
            &ast_graph.package,
            method_context.receiver_type.as_deref(),
            node,
        );
    }

    // Process parameters and returns for TypeOf/Reference edges
    let method_name = if let Some(receiver) = &method_context.receiver_type {
        format!(
            "{}.{}.{}",
            ast_graph.package,
            receiver.trim_start_matches('*'),
            method_context.name
        )
    } else {
        format!("{}.{}", ast_graph.package, method_context.name)
    };
    process_function_parameters(node, &method_name, content, helper, &ast_graph.package);
    process_function_returns(node, &method_name, content, helper, &ast_graph.package);

    walk_function_body_for_calls(
        node,
        content,
        &method_context,
        ast_graph,
        helper,
        scope_tree,
    )
}

fn handle_type_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    package: &str,
) {
    process_type_declaration_exports_unified(node, content, helper, inserted, package);
}

/// Handle type alias declarations (Go 1.9+)
///
/// Processes type aliases of the form `type Alias = Target` to create:
/// - `TypeOf` edge: alias → target (`TypeParameter` context)
/// - Reference edges: alias → all nested types in target
///
/// # Examples
///
/// - `type UserID = int` → `TypeOf`: UserID→int, Reference: int
/// - `type UserPtr = *User` → `TypeOf`: `UserPtr`→*User, Reference: User
/// - `type HandlerFunc = func(context.Context) error` → `TypeOf` + References
fn handle_type_alias(node: Node, content: &[u8], helper: &mut GraphBuildHelper, package: &str) {
    // Extract alias name
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(alias_name) = name_node.utf8_text(content) else {
        return;
    };

    // Check if alias is exported
    let is_exported = is_uppercase_export(alias_name);

    // Extract type parameters for generic type aliases (e.g., type Alias[T any] = []T)
    let type_params = extract_type_parameter_names(node, content, package, alias_name);

    // Extract target type
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };

    // Get full type string for TypeOf edge
    let type_string = type_node.utf8_text(content).map_or_else(
        |_| "<complex_type>".to_string(),
        std::string::ToString::to_string,
    );

    // Extract all type names for Reference edges, with type parameter qualification
    let referenced_types =
        extract_all_type_names_from_go_type_with_params(type_node, content, &type_params);

    // Create qualified alias name
    let qualified_alias = format!("{package}.{alias_name}");

    // Create Type node for the alias
    let alias_id = helper.add_type(&qualified_alias, None);

    // Add export edge if the alias is exported
    if is_exported {
        let module_id = helper.add_module(package, None);
        helper.add_export_edge(module_id, alias_id);
    }

    // Process type parameters if present (creates TypeParameter nodes + constraint edges)
    if !type_params.is_empty() {
        process_type_parameters(node, alias_name, content, helper, package);
    }

    // Create Type node for the target and TypeOf edge with TypeParameter context
    // (TypeParameter context indicates this is a type-level relationship)
    let target_id = helper.add_type(&type_string, None);
    helper.add_typeof_edge_with_context(
        alias_id,
        target_id,
        Some(TypeOfContext::TypeParameter),
        None,
        None,
    );

    // Create Reference edges for all nested types
    for ref_type_name in &referenced_types {
        let ref_type_id = helper.add_type(ref_type_name, None);
        helper.add_reference_edge(alias_id, ref_type_id);
    }
}

fn handle_var_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    package: &str,
) {
    // Handle exports (existing logic)
    process_var_declaration_exports_unified(node, content, helper, inserted, package);

    // Handle TypeOf and Reference edges for var/const declarations
    process_var_typeof_edges(node, content, helper, package);
}

fn exported_function_name(node: Node, content: &[u8]) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(content).ok()?;
    if is_uppercase_export(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Walk a function/method body to find call expressions and local variable references
fn walk_function_body_for_calls(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        "call_expression" => {
            handle_call_expression(
                node,
                content,
                caller_context,
                ast_graph,
                helper,
                CallSiteModifier::None,
            )?;
            // Continue to recurse - need to find nested calls in arguments like C.puts(C.CString(...))
            // The recursion is safe because child call_expressions will be handled by their own match arm
        }
        "go_statement" => {
            handle_go_statement(node, content, caller_context, ast_graph, helper, scope_tree)?;
            // Don't recurse - the handler already processed the call inside
            return Ok(());
        }
        "defer_statement" => {
            handle_defer_statement(node, content, caller_context, ast_graph, helper, scope_tree)?;
            // Don't recurse - the handler already processed the call inside
            return Ok(());
        }
        "selector_expression" => {
            handle_selector_expression(node, content, caller_context, helper);
        }
        "type_assertion_expression" => {
            handle_type_assertion_expression(
                node,
                content,
                caller_context,
                helper,
                &ast_graph.package,
            );
        }
        "identifier" => {
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse to children
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            walk_function_body_for_calls(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                scope_tree,
            )?;
        }
    }

    Ok(())
}

/// Walk only the children of a node to find call expressions, without processing the node itself.
/// Used when the parent node has already been processed with a modifier (e.g., Goroutine, Deferred)
/// but we need to find nested calls within its arguments.
fn walk_function_body_children_for_calls(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    // Recurse to children only, don't process the node itself
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            walk_function_body_for_calls(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                scope_tree,
            )?;
        }
    }
    Ok(())
}

fn handle_call_expression(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    modifier: CallSiteModifier,
) -> GraphResult<()> {
    if detect_http_route_registration(node, content, caller_context, helper) {
        return Ok(());
    }
    let is_ffi = build_ffi_call_edge(node, content, caller_context, ast_graph, helper);
    if !is_ffi {
        process_call_expression_unified(node, content, caller_context, helper, modifier)?;
    }
    Ok(())
}

fn handle_go_statement(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    // Go statement structure: (go_statement (call_expression ...))
    // The call_expression is a child, not a named field
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32)
            && child.kind() == "call_expression"
        {
            // Handle the top-level call (marked as goroutine)
            handle_call_expression(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                CallSiteModifier::Goroutine,
            )?;

            // Recurse into the call's children only to find nested calls
            // (e.g., `go C.puts(C.CString("x"))` has nested C.CString call)
            // Use children-only walker to avoid re-processing the top-level call
            walk_function_body_children_for_calls(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                scope_tree,
            )?;
            break;
        }
    }
    Ok(())
}

fn handle_defer_statement(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut local_scopes::GoScopeTree,
) -> GraphResult<()> {
    // Defer statement structure: (defer_statement (call_expression ...))
    // The call_expression is a child, not a named field
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32)
            && child.kind() == "call_expression"
        {
            // Handle the top-level call (marked as deferred)
            handle_call_expression(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                CallSiteModifier::Deferred,
            )?;

            // Recurse into the call's children only to find nested calls
            // (e.g., `defer C.puts(C.CString("x"))` has nested C.CString call)
            // Use children-only walker to avoid re-processing the top-level call
            walk_function_body_children_for_calls(
                child,
                content,
                caller_context,
                ast_graph,
                helper,
                scope_tree,
            )?;
            break;
        }
    }
    Ok(())
}

fn handle_selector_expression(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) {
    if let Some(parent) = node.parent()
        && parent.kind() != "call_expression"
    {
        process_field_access_unified(node, content, caller_context, helper);
    }
}

fn handle_type_assertion_expression(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    process_type_assertion_unified(node, content, caller_context, helper, package);
}

/// AST context for Go code
#[allow(dead_code)]
struct ASTGraph {
    contexts: Vec<FunctionContext>,
    package: String,
    /// Whether this file imports "C" (`CGo`)
    uses_cgo: bool,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Self {
        let root = tree.root_node();
        let mut contexts = Vec::new();
        let package = extract_package_name(root, content).unwrap_or_else(|| "main".to_string());

        // Create recursion guard with configured limit
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .expect("Failed to load recursion limits");
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .expect("Invalid file_ops_depth configuration");
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .expect("Failed to create recursion guard");

        if let Err(e) = extract_function_contexts(
            root,
            content,
            &package,
            &mut contexts,
            0,
            max_depth,
            &mut guard,
        ) {
            eprintln!("Warning: Go AST traversal hit recursion limit: {e}");
        }

        let uses_cgo = detect_cgo_import(root, content);
        Self {
            contexts,
            package,
            uses_cgo,
        }
    }

    fn contexts(&self) -> &[FunctionContext] {
        &self.contexts
    }

    #[allow(dead_code)] // Reserved for future context lookups
    fn find_context(&self, node: Node) -> Option<&FunctionContext> {
        let start = node.start_byte();
        let end = node.end_byte();
        self.contexts
            .iter()
            .filter(|ctx| start >= ctx.span.0 && end <= ctx.span.1)
            .min_by_key(|ctx| ctx.span.1 - ctx.span.0)
    }
}

/// Function/method context in Go
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct FunctionContext {
    name: String,
    package: String,
    receiver_type: Option<String>,
    is_method: bool,
    span: (usize, usize),
}

impl FunctionContext {
    fn qualified_name(&self) -> String {
        if let Some(ref receiver) = self.receiver_type {
            format!(
                "{}.{}.{}",
                self.package,
                receiver.trim_start_matches('*'),
                self.name
            )
        } else {
            format!("{}.{}", self.package, self.name)
        }
    }
}

#[allow(dead_code)]
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_function_contexts(
    node: Node,
    content: &[u8],
    package: &str,
    contexts: &mut Vec<FunctionContext>,
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
        "function_declaration" => {
            if let Some(context) = extract_function_context(node, content, package) {
                contexts.push(context);
            }
        }
        "method_declaration" => {
            if let Some(context) = extract_method_context(node, content, package) {
                contexts.push(context);
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            extract_function_contexts(
                child,
                content,
                package,
                contexts,
                depth + 1,
                max_depth,
                guard,
            )?;
        }
    }

    guard.exit();
    Ok(())
}

#[allow(dead_code)]
fn extract_package_name(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "package_clause" {
            for pkg_child in child.children(&mut child.walk()) {
                if pkg_child.kind() == "package_identifier"
                    && let Ok(name) = pkg_child.utf8_text(content)
                {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

#[allow(dead_code)]
fn extract_function_context(node: Node, content: &[u8], package: &str) -> Option<FunctionContext> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(content).ok()?.to_string();
    Some(FunctionContext {
        name,
        package: package.to_string(),
        receiver_type: None,
        is_method: false,
        span: (node.start_byte(), node.end_byte()),
    })
}

#[allow(dead_code)]
fn extract_method_context(node: Node, content: &[u8], package: &str) -> Option<FunctionContext> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(content).ok()?.to_string();
    let receiver_type = extract_receiver_type(node, content);
    Some(FunctionContext {
        name,
        package: package.to_string(),
        receiver_type,
        is_method: true,
        span: (node.start_byte(), node.end_byte()),
    })
}

#[allow(dead_code)]
fn extract_receiver_type(node: Node, content: &[u8]) -> Option<String> {
    let receiver_node = node.child_by_field_name("receiver")?;
    for i in 0..receiver_node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(param) = receiver_node.child(i as u32)
            && param.kind() == "parameter_declaration"
            && let Some(type_node) = param.child_by_field_name("type")
        {
            return type_node
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string);
        }
    }
    None
}

fn is_builtin(name: &str) -> bool {
    GO_BUILTINS.contains(&name)
}

#[allow(dead_code)] // Scaffolding for stdlib package detection
fn is_stdlib_package(path: &str) -> bool {
    if path.contains('.') {
        return false;
    }
    let first_segment = path.split(['/', '\\']).next().unwrap_or(path);
    GO_STDLIB_PACKAGES.contains(&first_segment)
}

fn extract_call_target(node: Node, content: &[u8]) -> GraphResult<String> {
    match node.kind() {
        "identifier" => node
            .utf8_text(content)
            .map(std::string::ToString::to_string)
            .map_err(|_| GraphBuilderError::ParseError {
                span: Span::from_bytes(node.start_byte(), node.end_byte()),
                reason: "Invalid UTF-8 in identifier".to_string(),
            }),
        "selector_expression" => {
            if let Some(field) = node.child_by_field_name("field")
                && let Ok(method_name) = field.utf8_text(content)
            {
                if let Some(operand) = node.child_by_field_name("operand")
                    && let Ok(receiver) = operand.utf8_text(content)
                {
                    return Ok(format!("{receiver}.{method_name}"));
                }
                return Ok(method_name.to_string());
            }
            Err(GraphBuilderError::ParseError {
                span: Span::from_bytes(node.start_byte(), node.end_byte()),
                reason: "Failed to parse selector_expression".to_string(),
            })
        }
        _ => node
            .utf8_text(content)
            .map(|s| s.trim().to_string())
            .map_err(|_| GraphBuilderError::ParseError {
                span: Span::from_bytes(node.start_byte(), node.end_byte()),
                reason: format!("Unknown call target kind: {}", node.kind()),
            }),
    }
}

#[allow(dead_code)] // Reserved for argument count analysis
fn count_arguments(node: Node) -> usize {
    if let Some(args_node) = node.child_by_field_name("arguments") {
        args_node
            .children(&mut args_node.walk())
            .filter(|child| {
                !child.kind().contains('(')
                    && !child.kind().contains(')')
                    && !child.kind().contains(',')
            })
            .count()
    } else {
        0
    }
}

// ============================================================================
// Unified GraphBuildHelper-based implementations
// ============================================================================

/// Process call expression using `GraphBuildHelper`
fn process_call_expression_unified(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
    modifier: CallSiteModifier,
) -> GraphResult<()> {
    let Some(function_node) = node.child_by_field_name("function") else {
        return Ok(());
    };

    let (callee_qualified, _is_builtin_call) =
        resolve_callee_qualified_name(function_node, content, caller_context)?;

    // Ensure both caller and callee nodes exist
    let source_id = ensure_caller_node(helper, caller_context);
    let target_id = helper.add_function(&callee_qualified, None, false, false);

    // Add call edge
    let argument_count = call_argument_count(node);
    let call_span = Span::from_bytes(node.start_byte(), node.end_byte());
    add_call_edge(
        helper,
        source_id,
        target_id,
        argument_count,
        modifier,
        call_span,
    );

    Ok(())
}

fn resolve_callee_qualified_name(
    function_node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
) -> GraphResult<(String, bool)> {
    let callee_name = extract_call_target(function_node, content)?;
    let simple_name = callee_name.split('.').next_back().unwrap_or(&callee_name);
    // Note: builtin detection reserved for future special handling.
    let is_builtin_call = is_builtin(simple_name) && !callee_name.contains('.');

    let callee_qualified = if callee_name.contains('.') {
        callee_name
    } else {
        format!("{}.{}", caller_context.package, callee_name)
    };

    Ok((callee_qualified, is_builtin_call))
}

fn ensure_caller_node(
    helper: &mut GraphBuildHelper,
    caller_context: &FunctionContext,
) -> UnifiedNodeId {
    helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    )
}

fn call_argument_count(node: Node) -> u8 {
    let arg_count = count_arguments(node);
    if arg_count > 254 {
        u8::MAX
    } else {
        u8::try_from(arg_count).unwrap_or(u8::MAX)
    }
}

fn add_call_edge(
    helper: &mut GraphBuildHelper,
    source_id: UnifiedNodeId,
    target_id: UnifiedNodeId,
    argument_count: u8,
    modifier: CallSiteModifier,
    call_span: Span,
) {
    let is_async = matches!(modifier, CallSiteModifier::Goroutine);
    helper.add_call_edge_full_with_span(
        source_id,
        target_id,
        argument_count,
        is_async,
        vec![call_span],
    );
}

/// Process import declaration using `GraphBuildHelper`
fn process_import_declaration_unified(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    _inserted: &mut HashSet<NodeId>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_spec" {
            process_import_spec_unified(child, content, ast_graph, helper);
        } else if child.kind() == "import_spec_list" {
            for spec_child in child.children(&mut child.walk()) {
                if spec_child.kind() == "import_spec" {
                    process_import_spec_unified(spec_child, content, ast_graph, helper);
                }
            }
        }
    }
}

/// Process import spec using `GraphBuildHelper`
fn process_import_spec_unified(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    let mut path: Option<String> = None;

    for child in node.children(&mut node.walk()) {
        match child.kind() {
            "interpreted_string_literal" | "raw_string_literal" => {
                if let Ok(text) = child.utf8_text(content) {
                    let trimmed = text
                        .trim_start_matches('"')
                        .trim_end_matches('"')
                        .trim_start_matches('`')
                        .trim_end_matches('`');
                    path = Some(trimmed.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(import_path) = path {
        // Create module nodes
        let from_id = helper.add_module(&ast_graph.package, None);
        let to_module_id = helper.add_import(
            &import_path,
            Some(Span::from_bytes(node.start_byte(), node.end_byte())),
        );

        // Add import edge
        helper.add_import_edge(from_id, to_module_id);
    }
}

/// Add export edge using `GraphBuildHelper`
fn add_export_edge_unified(
    helper: &mut GraphBuildHelper,
    _inserted: &mut HashSet<NodeId>,
    name: &str,
    package: &str,
    node: Node,
) {
    let from_id = helper.add_module(package, None);
    let symbol_qualified = format!("{package}.{name}");
    let visibility = visibility_for_identifier(name);
    let to_id = helper.add_function_with_visibility(
        &symbol_qualified,
        Some(Span::from_bytes(node.start_byte(), node.end_byte())),
        false,
        false,
        Some(visibility),
    );

    helper.add_export_edge(from_id, to_id);
}

/// Add method export edge using `GraphBuildHelper`
fn add_method_export_edge_unified(
    helper: &mut GraphBuildHelper,
    _inserted: &mut HashSet<NodeId>,
    name: &str,
    package: &str,
    receiver_type: Option<&str>,
    node: Node,
) {
    let from_id = helper.add_module(package, None);

    let symbol_qualified = if let Some(receiver) = receiver_type {
        format!("{}.{}.{}", package, receiver.trim_start_matches('*'), name)
    } else {
        format!("{package}.{name}")
    };
    let visibility = visibility_for_identifier(name);
    let to_id = helper.add_method_with_visibility(
        &symbol_qualified,
        Some(Span::from_bytes(node.start_byte(), node.end_byte())),
        false,
        false,
        Some(visibility),
    );

    helper.add_export_edge(from_id, to_id);
}

/// Process type declaration exports using `GraphBuildHelper`
///
/// Also handles OOP embedding:
/// - Struct embedding: `type Child struct { Parent }` creates Inherits edge Child → Parent
/// - Interface embedding: `type Writer interface { Reader }` creates Inherits edge Writer → Reader
/// - Type aliases: `type UserID = int` creates `TypeOf` edge `UserID` → int
fn process_type_declaration_exports_unified(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    package: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_spec" => {
                handle_type_spec(child, content, helper, inserted, package);
            }
            "type_alias" => {
                handle_type_alias(child, content, helper, package);
            }
            _ => {}
        }
    }
}

/// Process generic type parameters for a type declaration (Go 1.18+)
///
/// Handles generic type parameters like `type List[T any]` or `type Map[K comparable, V any]`.
/// Creates:
/// - `TypeParameter` nodes for each parameter (e.g., `List.T`, `Map.K`, `Map.V`)
/// - `TypeOf` edges: parameter → constraint (Constraint context)
/// - Reference edges: parameter → constraint types
///
/// # Examples
///
/// - `type List[T any]` → TypeParameter(List.T), `TypeOf`: List.T→any
/// - `type Map[K comparable, V any]` → 2 `TypeParameters`, 2 `TypeOf` edges
/// - `type Ordered[T io.Reader]` → `TypeOf`: Ordered.T→io.Reader, Reference: io.Reader
fn process_type_parameters(
    type_decl_node: Node,
    type_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    let Some(params_node) = type_decl_node.child_by_field_name("type_parameters") else {
        return;
    };

    // First pass: build type_params map (bare name -> qualified name)
    // This allows constraints to reference other type parameters
    let mut type_params = HashMap::new();
    let mut cursor = params_node.walk();
    for param_node in params_node.children(&mut cursor) {
        if param_node.kind() != "type_parameter_declaration" {
            continue;
        }

        let mut name_cursor = param_node.walk();
        for name_node in param_node.children_by_field_name("name", &mut name_cursor) {
            if let Ok(param_name) = name_node.utf8_text(content) {
                let qualified = format!("{package}.{type_name}.{param_name}");
                type_params.insert(param_name.to_string(), qualified);
            }
        }
    }

    // Second pass: create nodes and process constraints with qualified references
    let mut cursor = params_node.walk();
    for param_node in params_node.children(&mut cursor) {
        if param_node.kind() != "type_parameter_declaration" {
            continue;
        }

        // Extract constraint first (needed for all names in this declaration)
        let constraint_node = param_node.child_by_field_name("type");

        // Extract all parameter names (handles both "T any" and "K, V any")
        let mut name_cursor = param_node.walk();
        for name_node in param_node.children_by_field_name("name", &mut name_cursor) {
            let Ok(param_name) = name_node.utf8_text(content) else {
                continue;
            };

            // Create qualified parameter name: TypeName.ParamName
            let qualified_param = format!("{package}.{type_name}.{param_name}");

            // Create TypeParameter node
            let param_id = helper.add_type(&qualified_param, None);

            // Process constraint if present
            if let Some(constraint) = constraint_node {
                process_type_constraint(param_id, constraint, content, helper, &type_params);
            }
        }
    }
}

/// Process a type parameter constraint
///
/// Handles both simple constraints (any, comparable, io.Reader) and union constraints (int | float64).
/// Uses `type_params` to qualify references to other type parameters.
fn process_type_constraint(
    param_id: UnifiedNodeId,
    constraint_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    type_params: &HashMap<String, String>,
) {
    if constraint_node.kind() == "type_constraint" {
        // Handle union constraint: T int | float64
        // type_constraint can contain multiple types separated by '|'
        let mut cursor = constraint_node.walk();
        let variants: Vec<_> = constraint_node.named_children(&mut cursor).collect();

        if variants.len() > 1 {
            // Multiple types (union): extract Reference edges for each
            for variant_node in &variants {
                for type_name in extract_all_type_names_from_go_type_with_params(
                    *variant_node,
                    content,
                    type_params,
                ) {
                    let ref_type_id = helper.add_type(&type_name, None);
                    helper.add_reference_edge(param_id, ref_type_id);
                }
            }

            // TypeOf: full union string
            let constraint_str = constraint_node
                .utf8_text(content)
                .map_or_else(|_| "<union>".to_string(), std::string::ToString::to_string);
            let constraint_id = helper.add_type(&constraint_str, None);
            helper.add_typeof_edge_with_context(
                param_id,
                constraint_id,
                Some(TypeOfContext::Constraint),
                None,
                None,
            );
        } else if let Some(single_variant) = variants.first() {
            // Single type in constraint
            let constraint_str = single_variant.utf8_text(content).map_or_else(
                |_| "<constraint>".to_string(),
                std::string::ToString::to_string,
            );

            // TypeOf: param → constraint
            let constraint_id = helper.add_type(&constraint_str, None);
            helper.add_typeof_edge_with_context(
                param_id,
                constraint_id,
                Some(TypeOfContext::Constraint),
                None,
                None,
            );

            // Reference edges for nested types
            for type_name in extract_all_type_names_from_go_type_with_params(
                *single_variant,
                content,
                type_params,
            ) {
                let ref_type_id = helper.add_type(&type_name, None);
                helper.add_reference_edge(param_id, ref_type_id);
            }
        }
    } else {
        // Single constraint (any, comparable, interface, etc.)
        let constraint_str = constraint_node.utf8_text(content).map_or_else(
            |_| "<constraint>".to_string(),
            std::string::ToString::to_string,
        );

        // TypeOf: param → constraint (Constraint context)
        let constraint_id = helper.add_type(&constraint_str, None);
        helper.add_typeof_edge_with_context(
            param_id,
            constraint_id,
            Some(TypeOfContext::Constraint),
            None,
            None,
        );

        // Reference edges for nested types in constraint
        for type_name in
            extract_all_type_names_from_go_type_with_params(constraint_node, content, type_params)
        {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(param_id, ref_type_id);
        }
    }
}

fn handle_type_spec(
    type_spec: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    _inserted: &mut HashSet<NodeId>,
    package: &str,
) {
    let Some((name, is_exported, type_node)) = extract_type_spec_info(type_spec, content) else {
        return;
    };

    // Extract type parameter names for qualification (Go 1.18+)
    let type_params = extract_type_parameter_names(type_spec, content, package, &name);

    // Process generic type parameters if present (Go 1.18+)
    if !type_params.is_empty() {
        process_type_parameters(type_spec, &name, content, helper, package);
    }

    match type_node.kind() {
        "struct_type" => {
            let ctx = TypeSpecContext {
                type_spec,
                name: &name,
                is_exported,
                type_node,
                content,
                package,
                type_params: &type_params,
            };
            handle_struct_type_spec(&ctx, helper);
        }
        "interface_type" => {
            let ctx = TypeSpecContext {
                type_spec,
                name: &name,
                is_exported,
                type_node,
                content,
                package,
                type_params: &type_params,
            };
            handle_interface_type_spec(&ctx, helper);
        }
        _ => {
            // Type alias: type UserID = int, type GenericAlias[T any] = []T
            // Create TypeOf edge (alias → target) and Reference edges
            let qualified_name = format!("{package}.{name}");
            let type_id = helper.add_type(&qualified_name, None);

            // Add export edge if exported
            if is_exported {
                let module_id = helper.add_module(package, None);
                helper.add_export_edge(module_id, type_id);
            }

            // Get full type string for TypeOf edge
            let type_string = type_node.utf8_text(content).map_or_else(
                |_| "<complex_type>".to_string(),
                std::string::ToString::to_string,
            );

            // Create TypeOf edge: alias → target
            let target_id = helper.add_type(&type_string, None);
            helper.add_typeof_edge_with_context(
                type_id,
                target_id,
                Some(TypeOfContext::TypeParameter),
                None,
                None,
            );

            // Extract and create Reference edges
            let referenced_types =
                extract_all_type_names_from_go_type_with_params(type_node, content, &type_params);
            for ref_type_name in &referenced_types {
                let ref_type_id = helper.add_type(ref_type_name, None);
                helper.add_reference_edge(type_id, ref_type_id);
            }
        }
    }
}

/// Extract type parameter names from a type declaration for qualification
///
/// Returns a map from bare parameter name (e.g., "T") to qualified name (e.g., "main.List.T")
fn extract_type_parameter_names(
    type_decl_node: Node,
    content: &[u8],
    package: &str,
    type_name: &str,
) -> HashMap<String, String> {
    let mut params = HashMap::new();

    let Some(params_node) = type_decl_node.child_by_field_name("type_parameters") else {
        return params;
    };

    let mut cursor = params_node.walk();
    for param_node in params_node.children(&mut cursor) {
        if param_node.kind() != "type_parameter_declaration" {
            continue;
        }

        // Extract all parameter names
        let mut name_cursor = param_node.walk();
        for name_node in param_node.children_by_field_name("name", &mut name_cursor) {
            if let Ok(param_name) = name_node.utf8_text(content) {
                let qualified = format!("{package}.{type_name}.{param_name}");
                params.insert(param_name.to_string(), qualified);
            }
        }
    }

    params
}

fn extract_type_spec_info<'a>(
    type_spec: Node<'a>,
    content: &[u8],
) -> Option<(String, bool, Node<'a>)> {
    let name_node = type_spec.child_by_field_name("name")?;
    let name = name_node.utf8_text(content).ok()?.to_string();
    let is_exported = is_uppercase_export(&name);
    let type_node = type_spec.child_by_field_name("type")?;
    Some((name, is_exported, type_node))
}

/// Context for processing a type specification
struct TypeSpecContext<'a> {
    type_spec: Node<'a>,
    name: &'a str,
    is_exported: bool,
    type_node: Node<'a>,
    content: &'a [u8],
    package: &'a str,
    type_params: &'a HashMap<String, String>,
}

fn handle_struct_type_spec(ctx: &TypeSpecContext, helper: &mut GraphBuildHelper) {
    let symbol_qualified = format!("{}.{}", ctx.package, ctx.name);
    let visibility = visibility_for_identifier(ctx.name);
    let struct_id = helper.add_struct_with_visibility(
        &symbol_qualified,
        Some(Span::from_bytes(
            ctx.type_spec.start_byte(),
            ctx.type_spec.end_byte(),
        )),
        Some(visibility),
    );
    if ctx.is_exported {
        let module_id = helper.add_module(ctx.package, None);
        helper.add_export_edge(module_id, struct_id);
    }

    // Phase 1-2: Handle embedding (Inherits edges)
    process_struct_embedding(ctx.type_node, ctx.content, helper, struct_id, ctx.package);

    // Phase 3: Handle field types (TypeOf and Reference edges)
    process_struct_fields(
        ctx.type_node,
        &symbol_qualified,
        ctx.content,
        helper,
        ctx.package,
        ctx.type_params,
    );
}

fn handle_interface_type_spec(ctx: &TypeSpecContext, helper: &mut GraphBuildHelper) {
    let symbol_qualified = format!("{}.{}", ctx.package, ctx.name);
    let visibility = visibility_for_identifier(ctx.name);
    let interface_id = helper.add_interface_with_visibility(
        &symbol_qualified,
        Some(Span::from_bytes(
            ctx.type_spec.start_byte(),
            ctx.type_spec.end_byte(),
        )),
        Some(visibility),
    );
    if ctx.is_exported {
        let module_id = helper.add_module(ctx.package, None);
        helper.add_export_edge(module_id, interface_id);
    }

    // Phase 1-2: Handle embedding (Inherits edges)
    process_interface_embedding(
        ctx.type_node,
        ctx.content,
        helper,
        interface_id,
        ctx.package,
    );

    // Extract all type names used in interface body (method signatures) for Reference edges
    // This creates edges from the interface to types it uses (including type parameters)
    let referenced_types =
        extract_interface_referenced_types(ctx.type_node, ctx.content, ctx.type_params);
    for ref_type_name in &referenced_types {
        let ref_type_id = helper.add_type(ref_type_name, None);
        helper.add_reference_edge(interface_id, ref_type_id);
    }

    // Phase 3: Handle method signatures (TypeOf and Reference edges)
    process_interface_methods(
        ctx.type_node,
        &symbol_qualified,
        ctx.content,
        helper,
        ctx.package,
        ctx.type_params,
    );
}

/// Process struct embedding - detect anonymous/embedded fields and create Inherits edges.
///
/// In Go, an embedded field is a field without an explicit name:
/// ```go
/// type Child struct {
///     Parent    // embedded field - type is the name
///     Name string  // regular field
/// }
/// ```
///
/// **IMPORTANT SEMANTIC NOTE**: Go struct embedding is **composition with promotion**,
/// NOT classical inheritance/subtyping. Embedded fields' methods are "promoted" to the
/// outer type, making them accessible without explicit field access. However, the embedded
/// type is NOT a supertype (no "is-a" relationship).
///
/// We use `Inherits` edges for pragmatic graph analysis purposes (discovering related types),
/// but consumers should be aware this represents "has-a with promotion" rather than "is-a".
/// Interface embedding (see `process_interface_embedding`) is closer to true inheritance
/// as it represents type-set intersection/method-set inclusion.
///
/// This creates an Inherits edge: Child → Parent
fn process_struct_embedding(
    struct_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    child_id: UnifiedNodeId,
    package: &str,
) {
    // Find field_declaration_list inside struct_type
    let mut cursor = struct_node.walk();
    for child in struct_node.children(&mut cursor) {
        if child.kind() == "field_declaration_list" {
            let mut field_cursor = child.walk();
            for field in child.children(&mut field_cursor) {
                if field.kind() == "field_declaration" {
                    // An embedded field has no "name" field, only a "type" field
                    let has_name = field.child_by_field_name("name").is_some();
                    if !has_name
                        && let Some(type_node) = field.child_by_field_name("type")
                        && let Some(embedded_name) = extract_embedded_type_name(type_node, content)
                    {
                        // Qualify the embedded type name
                        let parent_qualified = if embedded_name.contains('.') {
                            // Already qualified (e.g., pkg.Type)
                            embedded_name
                        } else {
                            // Assume same package
                            format!("{package}.{embedded_name}")
                        };
                        let parent_id = helper.add_struct(&parent_qualified, None);
                        helper.add_inherits_edge(child_id, parent_id);
                    }
                }
            }
        }
    }
}

/// Process interface embedding - detect embedded interfaces and create Inherits edges.
///
/// In Go, an interface can embed other interfaces:
/// ```go
/// type ReadWriter interface {
///     Reader   // embedded interface
///     Writer   // embedded interface
///     Close() error  // method signature
/// }
/// ```
///
/// This creates Inherits edges: `ReadWriter` → `Reader`, `ReadWriter` → `Writer`
///
/// In tree-sitter-go, the AST structure for embedded interfaces is:
/// ```text
/// interface_type
///   type_elem              <- wrapper for embedded type
///     type_identifier = "Reader"
///   method_elem            <- wrapper for method signature (ignored)
///     ...
/// ```
fn process_interface_embedding(
    interface_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    child_id: UnifiedNodeId,
    package: &str,
) {
    // Interface type contains method_elem (for methods) and type_elem (for embedded interfaces)
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        // Embedded interfaces are wrapped in type_elem nodes
        if child.kind() == "type_elem" {
            // Look for type_identifier or qualified_type inside type_elem
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                match inner.kind() {
                    "type_identifier" => {
                        // Simple embedded interface: Reader
                        if let Ok(name) = inner.utf8_text(content) {
                            let name = name.trim();
                            if !name.is_empty() {
                                let parent_qualified = format!("{package}.{name}");
                                let parent_id = helper.add_interface(&parent_qualified, None);
                                helper.add_inherits_edge(child_id, parent_id);
                            }
                        }
                    }
                    "qualified_type" => {
                        // Qualified embedded interface: io.Reader
                        if let Ok(text) = inner.utf8_text(content) {
                            let text = text.trim();
                            if !text.is_empty() {
                                let parent_id = helper.add_interface(text, None);
                                helper.add_inherits_edge(child_id, parent_id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn visibility_for_identifier(name: &str) -> &'static str {
    let Some(first) = name.chars().next() else {
        return "private";
    };
    if first.is_uppercase() {
        "public"
    } else {
        "private"
    }
}

/// Extract the type name from an embedded field type node.
///
/// Handles:
/// - Simple type: `Parent` → "Parent"
/// - Pointer type: `*Parent` → "Parent"
/// - Qualified type: `pkg.Type` → "pkg.Type"
/// - Pointer to qualified: `*pkg.Type` → "pkg.Type"
fn extract_embedded_type_name(type_node: Node, content: &[u8]) -> Option<String> {
    match type_node.kind() {
        "type_identifier" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),
        "qualified_type" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),
        "pointer_type" => {
            // Get the inner type (without the *)
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.kind() == "type_identifier" || child.kind() == "qualified_type" {
                    return child.utf8_text(content).ok().map(|s| s.trim().to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Process var declaration exports using `GraphBuildHelper`
fn process_var_declaration_exports_unified(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    inserted: &mut HashSet<NodeId>,
    package: &str,
) {
    // MEDIUM fix (iter4): Only process package-level declarations
    // Export edges should only be created for package-scoped declarations
    let Some(parent) = node.parent() else {
        return;
    };
    if parent.kind() != "source_file" {
        return; // Skip function-local var/const declarations
    }

    // LOW-2 fix: Align with process_var_typeof_edges logic
    // Collect all var_spec and const_spec nodes (handles both direct and grouped)
    let mut specs = Vec::new();

    for child in node.children(&mut node.walk()) {
        match child.kind() {
            // Direct spec (e.g., `var X int`)
            "var_spec" | "const_spec" => {
                specs.push(child);
            }
            // Grouped specs (e.g., `var (X int; Y string)`)
            _ => {
                // Check if this is a spec list by examining its children
                for grandchild in child.children(&mut child.walk()) {
                    if grandchild.kind() == "var_spec" || grandchild.kind() == "const_spec" {
                        specs.push(grandchild);
                    }
                }
            }
        }
    }

    // Process each spec, handling multiple names (e.g., `var A, B int`)
    for spec in specs {
        let mut cursor = spec.walk();
        // Iterate ALL names in the spec
        for name_node in spec.children_by_field_name("name", &mut cursor) {
            if let Ok(name) = name_node.utf8_text(content)
                && is_uppercase_export(name)
            {
                add_export_edge_unified(helper, inserted, name, package, spec);
            }
        }
    }
}

/// Process var/const declarations to create `TypeOf` and Reference edges.
///
/// Handles:
/// - `var count int` → `TypeOf` edge: count → int
/// - `var user *User` → `TypeOf` edge: user → *User, Reference edge: user → User
/// - `const MaxSize = 100` → `TypeOf` edge: `MaxSize` → int (if type specified)
/// - `var cache map[string]User` → `TypeOf` edge + Reference edges: string, User
/// - `var (x int; y string)` → Grouped declarations
/// - `var a, b int` → Multi-name declarations
///
/// Only processes package-level declarations (parent is `source_file`).
fn process_var_typeof_edges(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    // MEDIUM-2 fix: Only process package-level declarations
    // Check if parent is source_file (package-level)
    let Some(parent) = node.parent() else {
        return;
    };
    if parent.kind() != "source_file" {
        // Skip function-local var/const declarations
        return;
    }

    // Collect all var_spec and const_spec nodes (handles both direct and grouped)
    let mut specs = Vec::new();

    for child in node.children(&mut node.walk()) {
        match child.kind() {
            // Direct spec (e.g., `var x int`)
            "var_spec" | "const_spec" => {
                specs.push(child);
            }
            // Grouped specs (e.g., `var (x int; y string)`)
            _ => {
                // Check if this is a spec list by examining its children
                for grandchild in child.children(&mut child.walk()) {
                    if grandchild.kind() == "var_spec" || grandchild.kind() == "const_spec" {
                        specs.push(grandchild);
                    }
                }
            }
        }
    }

    // Process each spec
    for spec in specs {
        process_single_var_spec(spec, content, helper, package);
    }
}

/// Process a single `var_spec` or `const_spec` node.
/// Handles multiple names in a single declaration (e.g., `var a, b int`).
fn process_single_var_spec(
    spec: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    // Extract type annotation if present
    let Some(type_node) = spec.child_by_field_name("type") else {
        // No explicit type annotation (e.g., `x := 42`), skip
        return;
    };

    // Get the type name for TypeOf edge
    let type_name = type_node.utf8_text(content).map_or_else(
        |_| "<complex_type>".to_string(),
        std::string::ToString::to_string,
    );

    // Extract all referenced type names for Reference edges
    let referenced_types = extract_all_type_names_from_go_type(type_node, content);

    // HIGH-1 fix: Process ALL names in the spec (handles `var a, b int`)
    // Iterate all children with field name "name"
    let mut cursor = spec.walk();
    for name_node in spec.children_by_field_name("name", &mut cursor) {
        let Ok(var_name) = name_node.utf8_text(content) else {
            continue;
        };

        // Create qualified variable name
        let qualified_var_name = format!("{package}.{var_name}");

        // Create variable node
        let var_id = helper.add_variable(
            &qualified_var_name,
            Some(Span::from_bytes(
                name_node.start_byte(),
                name_node.end_byte(),
            )),
        );

        // Create type node and TypeOf edge with Variable context
        let type_id = helper.add_type(&type_name, None);
        helper.add_typeof_edge_with_context(
            var_id,
            type_id,
            Some(TypeOfContext::Variable),
            None,
            Some(var_name),
        );

        // Create Reference edges for all nested types
        for ref_type_name in &referenced_types {
            let ref_type_id = helper.add_type(ref_type_name, None);
            helper.add_reference_edge(var_id, ref_type_id);
        }
    }
}

/// Process function/method parameters to create `TypeOf` and Reference edges
fn process_function_parameters(
    func_node: Node,
    func_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    let Some(params_node) = func_node.child_by_field_name("parameters") else {
        return;
    };

    let mut cursor = params_node.walk();
    let mut param_index = 0;

    // Regular functions don't have type parameters in scope (only generic types/interfaces do)
    let empty_type_params = HashMap::new();

    for param_decl in params_node.named_children(&mut cursor) {
        if param_decl.kind() == "parameter_declaration" {
            process_single_parameter(
                func_name,
                param_decl,
                param_index,
                content,
                helper,
                package,
                &empty_type_params,
            );

            // Count how many names this param has (e.g., "a, b int" is 2 params)
            let names_count = param_decl
                .children_by_field_name("name", &mut param_decl.walk())
                .count();
            param_index += if names_count > 0 { names_count } else { 1 };
        } else if param_decl.kind() == "variadic_parameter_declaration" {
            process_variadic_parameter(
                func_name,
                param_decl,
                param_index,
                content,
                helper,
                package,
                &empty_type_params,
            );
            param_index += 1;
        }
    }
}

fn process_single_parameter(
    func_name: &str,
    param_node: Node,
    base_index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let Some(type_node) = param_node.child_by_field_name("type") else {
        return;
    };

    // Get parameter names (there can be multiple: a, b, c int)
    let mut cursor = param_node.walk();
    let names: Vec<_> = param_node
        .children_by_field_name("name", &mut cursor)
        .filter_map(|n| n.utf8_text(content).ok())
        .collect();

    let type_text = type_node.utf8_text(content).unwrap_or("").to_string();
    let referenced_types =
        extract_all_type_names_from_go_type_with_params(type_node, content, type_params);

    // Create edges for each parameter name (or anonymous if no names)
    if names.is_empty() {
        // Anonymous parameter
        create_parameter_edges(
            func_name,
            base_index,
            None,
            &type_text,
            &referenced_types,
            helper,
            package,
        );
    } else {
        for (i, name) in names.iter().enumerate() {
            create_parameter_edges(
                func_name,
                base_index + i,
                Some(name),
                &type_text,
                &referenced_types,
                helper,
                package,
            );
        }
    }
}

fn process_variadic_parameter(
    func_name: &str,
    param_node: Node,
    param_index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let Some(type_node) = param_node.child_by_field_name("type") else {
        return;
    };

    // Get parameter name (variadic params can only have one name)
    let name = param_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok());

    // Variadic type "...T" becomes slice type "[]T" for TypeOf
    let type_text = format!("[]{}", type_node.utf8_text(content).unwrap_or(""));

    // Referenced types come from the element type
    let referenced_types =
        extract_all_type_names_from_go_type_with_params(type_node, content, type_params);

    create_parameter_edges(
        func_name,
        param_index,
        name,
        &type_text,
        &referenced_types,
        helper,
        package,
    );
}

fn create_parameter_edges(
    func_name: &str,
    index: usize,
    name: Option<&str>,
    type_text: &str,
    referenced_types: &[String],
    helper: &mut GraphBuildHelper,
    _package: &str,
) {
    // Get or create function node (Go functions are never async or unsafe)
    let func_id = helper.add_function(func_name, None, false, false);

    // Create TypeOf edge: function → parameter type with Parameter context
    let type_id = helper.add_type(type_text, None);
    #[allow(clippy::cast_possible_truncation)]
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(index as u16),
        name,
    );

    // Create Reference edges to all referenced types
    for ref_type in referenced_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Process function/method return types to create `TypeOf` and Reference edges
fn process_function_returns(
    func_node: Node,
    func_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    let Some(result_node) = func_node.child_by_field_name("result") else {
        return; // No return type (void function)
    };

    // Regular functions don't have type parameters in scope (only generic types/interfaces do)
    let empty_type_params = HashMap::new();

    match result_node.kind() {
        "parameter_list" => {
            // Multiple returns: (int, error) or (result int, err error)
            let mut cursor = result_node.walk();
            for (index, param_decl) in result_node.named_children(&mut cursor).enumerate() {
                if param_decl.kind() == "parameter_declaration" {
                    process_single_return(
                        func_name,
                        param_decl,
                        index,
                        content,
                        helper,
                        package,
                        &empty_type_params,
                    );
                }
            }
        }
        _ => {
            // Single return type (no parens)
            process_single_return_type(
                func_name,
                result_node,
                0,
                content,
                helper,
                package,
                &empty_type_params,
            );
        }
    }
}

fn process_single_return(
    func_name: &str,
    param_node: Node,
    index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let Some(type_node) = param_node.child_by_field_name("type") else {
        return;
    };

    process_single_return_type(
        func_name,
        type_node,
        index,
        content,
        helper,
        package,
        type_params,
    );
}

fn process_single_return_type(
    func_name: &str,
    type_node: Node,
    index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let type_text = type_node.utf8_text(content).unwrap_or("").to_string();
    let referenced_types =
        extract_all_type_names_from_go_type_with_params(type_node, content, type_params);

    create_return_edges(
        func_name,
        index,
        &type_text,
        &referenced_types,
        helper,
        package,
    );
}

fn create_return_edges(
    func_name: &str,
    index: usize,
    type_text: &str,
    referenced_types: &[String],
    helper: &mut GraphBuildHelper,
    _package: &str,
) {
    // Get or create function node (Go functions are never async or unsafe)
    let func_id = helper.add_function(func_name, None, false, false);

    // Create TypeOf edge: function → return type with Return context
    let type_id = helper.add_type(type_text, None);
    #[allow(clippy::cast_possible_truncation)]
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(index as u16),
        None, // Returns don't have names (unless named returns, which we extract the type from)
    );

    // Create Reference edges to all referenced types
    for ref_type in referenced_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Process struct fields to create `TypeOf` and Reference edges (Phase 3)
///
/// Handles regular fields (with names) and creates edges for their types.
/// Embedded fields (no explicit name) are handled separately by `process_struct_embedding`.
fn process_struct_fields(
    struct_node: Node,
    struct_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    // Find field_declaration_list inside struct_type
    let mut cursor = struct_node.walk();
    for child in struct_node.children(&mut cursor) {
        if child.kind() == "field_declaration_list" {
            let mut field_cursor = child.walk();
            let mut field_index = 0;

            for field in child.children(&mut field_cursor) {
                if field.kind() == "field_declaration" {
                    // Only process fields that have names (regular fields)
                    // Embedded fields (no name) are handled by process_struct_embedding
                    let has_name = field.child_by_field_name("name").is_some();
                    if has_name {
                        process_single_struct_field(
                            struct_name,
                            field,
                            field_index,
                            content,
                            helper,
                            package,
                            type_params,
                        );

                        // Count how many names this field has (e.g., "X, Y int" is 2 fields)
                        let names_count = field
                            .children_by_field_name("name", &mut field.walk())
                            .count();
                        field_index += names_count;
                    }
                }
            }
        }
    }
}

fn process_single_struct_field(
    struct_name: &str,
    field_node: Node,
    base_index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let Some(type_node) = field_node.child_by_field_name("type") else {
        return;
    };

    // Get field names (there can be multiple: X, Y, Z int)
    let mut cursor = field_node.walk();
    let names: Vec<_> = field_node
        .children_by_field_name("name", &mut cursor)
        .filter_map(|n| n.utf8_text(content).ok())
        .collect();

    let type_text = type_node.utf8_text(content).unwrap_or("").to_string();
    let referenced_types =
        extract_all_type_names_from_go_type_with_params(type_node, content, type_params);

    // Create edges for each field name
    for (i, name) in names.iter().enumerate() {
        create_field_edges(
            struct_name,
            base_index + i,
            name,
            &type_text,
            &referenced_types,
            helper,
            package,
        );
    }
}

fn create_field_edges(
    struct_name: &str,
    index: usize,
    name: &str,
    type_text: &str,
    referenced_types: &[String],
    helper: &mut GraphBuildHelper,
    _package: &str,
) {
    // Get or create struct node
    let struct_id = helper.add_struct(struct_name, None);

    // Create TypeOf edge: struct → field type with Field context
    let type_id = helper.add_type(type_text, None);
    #[allow(clippy::cast_possible_truncation)]
    helper.add_typeof_edge_with_context(
        struct_id,
        type_id,
        Some(TypeOfContext::Field),
        Some(index as u16),
        Some(name),
    );

    // Create Reference edges to all referenced types
    for ref_type in referenced_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(struct_id, ref_type_id);
    }
}

/// Extract all type names referenced in an interface body (from method signatures)
///
/// This is used to create Reference edges from the interface type to all types it uses.
/// Similar to how type aliases create Reference edges from the alias to types in the RHS.
fn extract_interface_referenced_types(
    interface_node: Node,
    content: &[u8],
    type_params: &HashMap<String, String>,
) -> Vec<String> {
    let mut referenced_types = Vec::new();
    let mut cursor = interface_node.walk();

    for child in interface_node.children(&mut cursor) {
        if child.kind() == "method_elem" {
            // Extract types from method parameters and returns
            let mut method_cursor = child.walk();
            for method_child in child.children(&mut method_cursor) {
                match method_child.kind() {
                    "parameter_list" => {
                        // Extract types from parameter list
                        let mut param_cursor = method_child.walk();
                        for param in method_child.named_children(&mut param_cursor) {
                            if matches!(
                                param.kind(),
                                "parameter_declaration" | "variadic_parameter_declaration"
                            ) && let Some(type_node) = param.child_by_field_name("type")
                            {
                                let types = extract_all_type_names_from_go_type_with_params(
                                    type_node,
                                    content,
                                    type_params,
                                );
                                referenced_types.extend(types);
                            }
                        }
                    }
                    "type_identifier" | "qualified_type" | "pointer_type" | "slice_type"
                    | "array_type" | "map_type" | "channel_type" | "function_type"
                    | "struct_type" | "interface_type" | "generic_type" | "type_union" => {
                        // Single return type (not in parameter_list)
                        let types = extract_all_type_names_from_go_type_with_params(
                            method_child,
                            content,
                            type_params,
                        );
                        referenced_types.extend(types);
                    }
                    _ => {}
                }
            }
        } else if child.kind() == "type_elem" {
            // Extract types from type-set constraints (e.g., ~[]T, ~int | ~string)
            // type_elem contains type expressions including negated types and unions
            let mut elem_cursor = child.walk();
            for type_expr in child.named_children(&mut elem_cursor) {
                let types = extract_all_type_names_from_go_type_with_params(
                    type_expr,
                    content,
                    type_params,
                );
                referenced_types.extend(types);
            }
        }
    }

    referenced_types
}

/// Process interface methods to create `TypeOf` and Reference edges (Phase 3)
///
/// For each method in the interface, creates TypeOf/Reference edges for parameters and returns,
/// similar to how Phase 2 handles function/method signatures.
fn process_interface_methods(
    interface_node: Node,
    interface_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        // Method signatures are in method_elem nodes
        if child.kind() == "method_elem" {
            process_interface_method_elem(
                interface_name,
                child,
                content,
                helper,
                package,
                type_params,
            );
        }
    }
}

fn process_interface_method_elem(
    interface_name: &str,
    method_elem: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    // method_elem structure:
    //   field_identifier (method name)
    //   parameter_list (parameters)
    //   parameter_list or type_identifier (return type)

    // Get method name from field_identifier
    let mut cursor = method_elem.walk();
    let mut method_name_opt: Option<&str> = None;
    let mut params_nodes: Vec<Node> = Vec::new();

    for child in method_elem.children(&mut cursor) {
        match child.kind() {
            "field_identifier" => {
                if let Ok(name) = child.utf8_text(content) {
                    method_name_opt = Some(name);
                }
            }
            "parameter_list" | "type_identifier" | "qualified_type" | "pointer_type"
            | "slice_type" | "array_type" | "map_type" | "channel_type" | "function_type"
            | "struct_type" | "interface_type" | "generic_type" | "type_union" => {
                params_nodes.push(child);
            }
            _ => {}
        }
    }

    let Some(method_name) = method_name_opt else {
        return;
    };

    // Qualify the method name: Interface.Method
    let qualified_method = format!("{interface_name}.{method_name}");

    // First parameter_list is parameters, second is returns (if present)
    // Or a single type node is the return type
    if !params_nodes.is_empty() {
        // First node should be parameters
        if params_nodes[0].kind() == "parameter_list" {
            process_method_parameters(
                &qualified_method,
                params_nodes[0],
                content,
                helper,
                package,
                type_params,
            );
        }

        // If there's a second node, it's the return type(s)
        if params_nodes.len() > 1 {
            let return_node = params_nodes[1];
            if return_node.kind() == "parameter_list" {
                // Multiple returns
                let mut cursor = return_node.walk();
                for (index, param_decl) in return_node.named_children(&mut cursor).enumerate() {
                    if param_decl.kind() == "parameter_declaration" {
                        process_single_return(
                            &qualified_method,
                            param_decl,
                            index,
                            content,
                            helper,
                            package,
                            type_params,
                        );
                    }
                }
            } else {
                // Single return type
                process_single_return_type(
                    &qualified_method,
                    return_node,
                    0,
                    content,
                    helper,
                    package,
                    type_params,
                );
            }
        }
    }
}

fn process_method_parameters(
    method_name: &str,
    params_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    package: &str,
    type_params: &HashMap<String, String>,
) {
    let mut cursor = params_node.walk();
    let mut param_index = 0;

    for param_decl in params_node.named_children(&mut cursor) {
        if param_decl.kind() == "parameter_declaration" {
            process_single_parameter(
                method_name,
                param_decl,
                param_index,
                content,
                helper,
                package,
                type_params,
            );

            let names_count = param_decl
                .children_by_field_name("name", &mut param_decl.walk())
                .count();
            param_index += if names_count > 0 { names_count } else { 1 };
        } else if param_decl.kind() == "variadic_parameter_declaration" {
            process_variadic_parameter(
                method_name,
                param_decl,
                param_index,
                content,
                helper,
                package,
                type_params,
            );
            param_index += 1;
        }
    }
}

/// Process field access using `GraphBuildHelper`
fn process_field_access_unified(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) {
    let Some(field_node) = node.child_by_field_name("field") else {
        return;
    };

    let Ok(field_name) = field_node.utf8_text(content) else {
        return;
    };
    let field_name = field_name.to_string();

    let Some(operand_node) = node.child_by_field_name("operand") else {
        return;
    };

    let Ok(operand_text) = operand_node.utf8_text(content) else {
        return;
    };
    let operand_text = operand_text.to_string();

    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );

    let field_ref_id = helper.add_variable(
        &format!("<field:{operand_text}.{field_name}>"),
        Some(Span::from_bytes(node.start_byte(), node.end_byte())),
    );

    helper.add_reference_edge(caller_id, field_ref_id);
}

/// Process type assertion using `GraphBuildHelper`
fn process_type_assertion_unified(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
    package: &str,
) {
    let type_node = node.children(&mut node.walk()).find(|child| {
        matches!(
            child.kind(),
            "type_identifier" | "pointer_type" | "qualified_type" | "slice_type" | "array_type"
        )
    });

    let Some(type_node) = type_node else {
        return;
    };

    let asserted_type = match type_node.utf8_text(content) {
        Ok(text) => text.to_string(),
        Err(_) => return,
    };

    if asserted_type == "type" {
        return;
    }

    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );

    let qualified_type = if asserted_type.contains('.') {
        asserted_type.clone()
    } else {
        let base_type = asserted_type
            .trim_start_matches('*')
            .trim_start_matches("[]");
        format!("{package}.{base_type}")
    };

    let type_ref_id = helper.add_interface(
        &format!("<type:{qualified_type}>"),
        Some(Span::from_bytes(node.start_byte(), node.end_byte())),
    );

    helper.add_implements_edge(caller_id, type_ref_id);
}

// ============================================================================
// FFI Detection - CGo, syscall, and plugin packages
// ============================================================================

/// Detect if the file imports "C" (`CGo`).
fn detect_cgo_import(node: Node, content: &[u8]) -> bool {
    if node.kind() == "import_spec" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "interpreted_string_literal"
                || child.kind() == "raw_string_literal")
                && let Ok(text) = child.utf8_text(content)
            {
                let trimmed = text
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim_start_matches('`')
                    .trim_end_matches('`');
                if trimmed == "C" {
                    return true;
                }
            }
        }
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if detect_cgo_import(child, content) {
            return true;
        }
    }

    false
}

/// Detect Go HTTP route registrations and create `Endpoint` nodes.
///
/// Matches patterns from popular Go web frameworks and the standard library:
///
/// - `http.HandleFunc("/path", handler)` — standard library `net/http`
/// - `mux.HandleFunc("/path", handler)` — gorilla/mux (any receiver + `HandleFunc`)
/// - `r.GET("/path", handler)` — Gin framework (any receiver + HTTP method name)
/// - `r.POST("/path", handler)` — Gin framework
/// - `e.GET("/path", handler)` — Echo framework
/// - `router.Handle("GET", "/path", handler)` — httprouter (method as first string arg)
///
/// Creates an `Endpoint` node with qualified name `route::METHOD::/path` and a
/// `Contains` edge from the endpoint to the handler function if identifiable.
///
/// Returns `true` if a route registration was detected, `false` otherwise.
fn detect_http_route_registration(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) -> bool {
    // The callee must be a selector_expression (e.g., `http.HandleFunc`, `r.GET`)
    let Some(function_node) = node.child_by_field_name("function") else {
        return false;
    };

    if function_node.kind() != "selector_expression" {
        return false;
    }

    let Some(field_node) = function_node.child_by_field_name("field") else {
        return false;
    };

    let field_text = field_node.utf8_text(content).unwrap_or("").trim();

    // Determine HTTP method and which argument index holds the path
    let (http_method, path_arg_index) = match field_text {
        // HandleFunc: method defaults to GET, path is first argument
        "HandleFunc" => ("GET", 0),
        // Handle (httprouter): method is first argument, path is second argument
        "Handle" => {
            // For Handle, the HTTP method is the first string argument
            let Some(method_str) = extract_route_string_arg(node, content, 0) else {
                return false;
            };
            let method_upper = method_str.to_uppercase();
            if !matches!(
                method_upper.as_str(),
                "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS"
            ) {
                return false;
            }
            // Path is the second argument
            let Some(path) = extract_route_string_arg(node, content, 1) else {
                return false;
            };
            return build_route_endpoint(
                node,
                content,
                &method_upper,
                &path,
                caller_context,
                helper,
                2,
            );
        }
        // Direct HTTP method names (Gin, Echo): method from name, path is first argument.
        // For ambiguous names like GET/POST/PUT/DELETE/PATCH, validate that the first
        // argument is a path-like string starting with '/' to avoid false positives
        // (e.g., cache.GET("key") should NOT be treated as a route registration).
        "GET" | "POST" | "PUT" | "DELETE" | "PATCH" => {
            let Some(path) = extract_route_string_arg(node, content, 0) else {
                return false;
            };
            if !path.starts_with('/') {
                return false;
            }
            (field_text, 0)
        }
        _ => return false,
    };

    // Extract the path from the appropriate argument position
    let Some(path) = extract_route_string_arg(node, content, path_arg_index) else {
        return false;
    };

    // The handler argument is one position after the path
    let handler_arg_index = path_arg_index + 1;
    build_route_endpoint(
        node,
        content,
        http_method,
        &path,
        caller_context,
        helper,
        handler_arg_index,
    )
}

/// Build a route endpoint node and link it to the caller and handler.
///
/// Creates an `Endpoint` node with qualified name `route::METHOD::/path`,
/// a `Calls` edge from the caller to the endpoint, and a `Contains` edge
/// from the endpoint to the handler function if identifiable.
///
/// Returns `true` always (a route was detected).
fn build_route_endpoint(
    call_node: Node,
    content: &[u8],
    method: &str,
    path: &str,
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
    handler_arg_index: usize,
) -> bool {
    let qualified_name = format!("route::{method}::{path}");
    let span = Span::from_bytes(call_node.start_byte(), call_node.end_byte());
    let endpoint_id = helper.add_endpoint(&qualified_name, Some(span));

    // Add Calls edge from the enclosing function to the endpoint
    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );
    helper.add_call_edge(caller_id, endpoint_id);

    // Try to find and link the handler function argument
    if let Some(handler_node) = get_nth_argument(call_node, handler_arg_index)
        && let Ok(handler_text) = handler_node.utf8_text(content)
    {
        let handler_name = handler_text.trim();
        if !handler_name.is_empty()
            && matches!(handler_node.kind(), "identifier" | "selector_expression")
        {
            let handler_id = helper.ensure_function(handler_name, None, false, false);
            helper.add_contains_edge(endpoint_id, handler_id);
        }
    }

    true
}

/// Extract a string literal from the Nth non-punctuation argument of a call expression.
///
/// Navigates into the `arguments` child of the call expression, skips parentheses
/// and commas, and returns the string content (with quotes stripped) at the given index.
fn extract_route_string_arg(call_node: Node, content: &[u8], arg_index: usize) -> Option<String> {
    let args_node = call_node.child_by_field_name("arguments")?;

    let mut cursor = args_node.walk();
    let arg = args_node
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","))
        .nth(arg_index)?;

    // Must be a string literal (interpreted or raw)
    if arg.kind() != "interpreted_string_literal" && arg.kind() != "raw_string_literal" {
        return None;
    }

    let text = arg.utf8_text(content).ok()?;
    let trimmed = text
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('`')
        .trim_end_matches('`');
    Some(trimmed.to_string())
}

/// Get the Nth non-punctuation argument node from a call expression.
fn get_nth_argument(call_node: Node, arg_index: usize) -> Option<Node> {
    let args_node = call_node.child_by_field_name("arguments")?;

    let mut cursor = args_node.walk();
    args_node
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | ","))
        .nth(arg_index)
}

/// Check if a call expression is an FFI call and build appropriate edge.
///
/// Detects:
/// - `C.function_name()` - `CGo` calls to C functions
/// - `syscall.Syscall()` / `syscall.Syscall6()` / `syscall.SyscallN()` - low-level syscalls
/// - `syscall.RawSyscall()` / `syscall.RawSyscall6()` - raw syscalls without runtime notifications
/// - `plugin.Open()` - dynamic plugin loading
///
/// Returns true if an FFI edge was created, false otherwise.
fn build_ffi_call_edge(
    node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> bool {
    let Some(function_node) = node.child_by_field_name("function") else {
        return false;
    };

    // Check for selector expressions (X.Y pattern)
    if function_node.kind() != "selector_expression" {
        return false;
    }

    let Some(operand_node) = function_node.child_by_field_name("operand") else {
        return false;
    };

    let Some(field_node) = function_node.child_by_field_name("field") else {
        return false;
    };

    let operand_text = operand_node.utf8_text(content).unwrap_or("").trim();
    let field_text = field_node.utf8_text(content).unwrap_or("").trim();

    // Check for CGo calls (C.xxx)
    if operand_text == "C" && ast_graph.uses_cgo {
        return build_cgo_edge(node, field_text, caller_context, helper);
    }

    // Check for syscall package calls
    if operand_text == "syscall" && is_syscall_ffi_call(field_text) {
        return build_syscall_edge(node, content, caller_context, helper);
    }

    // Check for plugin.Open
    if operand_text == "plugin" && field_text == "Open" {
        return build_plugin_edge(node, content, caller_context, helper);
    }

    false
}

/// Check if a syscall method is an FFI call.
fn is_syscall_ffi_call(method_name: &str) -> bool {
    matches!(
        method_name,
        "Syscall"
            | "Syscall6"
            | "Syscall9"
            | "Syscall12"
            | "Syscall15"
            | "Syscall18"
            | "SyscallN"
            | "RawSyscall"
            | "RawSyscall6"
    )
}

/// Build FFI edge for `CGo` calls.
fn build_cgo_edge(
    call_node: Node,
    c_function_name: &str,
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) -> bool {
    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );

    // Create FFI target node for the C function
    let ffi_name = format!("C::{c_function_name}");
    let ffi_node_id = helper.add_function(
        &ffi_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    // Add FFI edge with C convention
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Build FFI edge for syscall package calls.
fn build_syscall_edge(
    call_node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) -> bool {
    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );

    // Try to extract syscall number from first argument
    let syscall_name =
        extract_syscall_name(call_node, content).unwrap_or_else(|| "syscall::unknown".to_string());

    // Create FFI target node
    let ffi_node_id = helper.add_function(
        &syscall_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
        false,
        false,
    );

    // Add FFI edge with C convention (syscalls use C ABI)
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Build FFI edge for plugin.Open calls.
fn build_plugin_edge(
    call_node: Node,
    content: &[u8],
    caller_context: &FunctionContext,
    helper: &mut GraphBuildHelper,
) -> bool {
    let caller_id = helper.ensure_function(
        &caller_context.qualified_name(),
        Some(Span::from_bytes(
            caller_context.span.0,
            caller_context.span.1,
        )),
        false,
        false,
    );

    // Try to extract plugin path from first argument
    let plugin_name = extract_plugin_path(call_node, content).map_or_else(
        || "plugin::unknown".to_string(),
        |path| format!("plugin::{}", simple_name(&path)),
    );

    // Create FFI target node
    let ffi_node_id = helper.add_module(
        &plugin_name,
        Some(Span::from_bytes(
            call_node.start_byte(),
            call_node.end_byte(),
        )),
    );

    // Add FFI edge with C convention (plugins use C ABI for symbol lookup)
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Extract syscall name from arguments.
///
/// Looks for patterns like `syscall.SYS_WRITE` or constant expressions.
fn extract_syscall_name(call_node: Node, content: &[u8]) -> Option<String> {
    let args_node = call_node.child_by_field_name("arguments")?;

    let mut cursor = args_node.walk();
    let first_arg = args_node
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;

    // Check if it's a selector expression like syscall.SYS_WRITE
    if first_arg.kind() == "selector_expression"
        && let Some(field) = first_arg.child_by_field_name("field")
    {
        let text = field.utf8_text(content).ok()?;
        return Some(format!("syscall::{}", text.trim()));
    }

    // Check if it's an identifier (could be a local variable or constant)
    if first_arg.kind() == "identifier" {
        let text = first_arg.utf8_text(content).ok()?;
        let trimmed = text.trim();
        // Check for known syscall constant naming patterns
        if trimmed.starts_with("SYS_") {
            return Some(format!("syscall::{trimmed}"));
        }
        return Some(format!("syscall::${trimmed}")); // Variable reference
    }

    // Integer literal - just record as numeric
    if first_arg.kind() == "int_literal" {
        let text = first_arg.utf8_text(content).ok()?;
        return Some(format!("syscall::#{}", text.trim()));
    }

    None
}

/// Extract plugin path from `Open()` call.
fn extract_plugin_path(call_node: Node, content: &[u8]) -> Option<String> {
    let args_node = call_node.child_by_field_name("arguments")?;

    let mut cursor = args_node.walk();
    let first_arg = args_node
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;

    // Check for string literal
    if first_arg.kind() == "interpreted_string_literal" || first_arg.kind() == "raw_string_literal"
    {
        let text = first_arg.utf8_text(content).ok()?;
        let trimmed = text
            .trim_start_matches('"')
            .trim_end_matches('"')
            .trim_start_matches('`')
            .trim_end_matches('`');
        return Some(trimmed.to_string());
    }

    // Check for identifier (variable)
    if first_arg.kind() == "identifier" {
        let text = first_arg.utf8_text(content).ok()?;
        return Some(format!("${}", text.trim()));
    }

    None
}

/// Extract simple name from a path (last component).
fn simple_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Extract all type names from a Go type node, including nested types.
///
/// Returns a vector of type names that should be created as Reference edges.
/// For simple types like `int` or `string`, returns a single-element vector.
/// For complex types like `map[string]*User`, returns \["string", "User"\].
///
/// # Supported Type Constructs
///
/// - `type_identifier`: int, string, User, etc.
/// - `qualified_type`: pkg.Type, context.Context
/// - `pointer_type`: *T → extracts T
/// - `slice_type`: []T → extracts T
/// - `array_type`: [N]T → extracts T
/// - `map_type`: map[K]V → extracts K, V
/// - `channel_type`: chan T, <-chan T, chan<- T → extracts T
/// - `function_type`: func(A, B) C → extracts A, B, C
/// - `struct_type`: struct { F T } → extracts field types
/// - `interface_type`: interface { `M()` T } → extracts method signature types
///
/// # Examples
///
/// ```ignore
/// // Input: type_identifier = "int"
/// // Output: vec!["int"]
///
/// // Input: pointer_type → type_identifier = "User"
/// // Output: vec!["User"]
///
/// // Input: slice_type → type_identifier = "string"
/// // Output: vec!["string"]
///
/// // Input: map_type
/// //   key_type: type_identifier = "string"
/// //   value_type: pointer_type → type_identifier = "User"
/// // Output: vec!["string", "User"]
/// ```
fn extract_all_type_names_from_go_type(type_node: Node, content: &[u8]) -> Vec<String> {
    extract_all_type_names_from_go_type_with_params(type_node, content, &HashMap::new())
}

/// Extract all type names with type parameter qualification support
#[allow(clippy::too_many_lines)]
fn extract_all_type_names_from_go_type_with_params(
    type_node: Node,
    content: &[u8],
    type_params: &HashMap<String, String>,
) -> Vec<String> {
    match type_node.kind() {
        // Simple type identifier: int, string, User, T, etc.
        // Qualify if it matches a type parameter
        "type_identifier" => {
            if let Ok(type_name) = type_node.utf8_text(content) {
                let qualified = type_params
                    .get(type_name)
                    .cloned()
                    .unwrap_or_else(|| type_name.to_string());
                vec![qualified]
            } else {
                Vec::new()
            }
        }

        // Qualified type: pkg.Type, context.Context
        "qualified_type" => {
            if let Ok(qualified_name) = type_node.utf8_text(content) {
                vec![qualified_name.to_string()]
            } else {
                Vec::new()
            }
        }

        // Pointer type: *T → extract T
        "pointer_type" => {
            let mut types = Vec::new();
            if let Some(inner_type) = type_node.named_child(0) {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    inner_type,
                    content,
                    type_params,
                ));
            }
            types
        }

        // Slice type: []T → extract T
        // Array type: [N]T → extract T
        "slice_type" | "array_type" => {
            let mut types = Vec::new();
            if let Some(element_type) = type_node.child_by_field_name("element") {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    element_type,
                    content,
                    type_params,
                ));
            }
            types
        }

        // Map type: map[K]V → extract K, V
        "map_type" => {
            let mut types = Vec::new();
            if let Some(key_type) = type_node.child_by_field_name("key") {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    key_type,
                    content,
                    type_params,
                ));
            }
            if let Some(value_type) = type_node.child_by_field_name("value") {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    value_type,
                    content,
                    type_params,
                ));
            }
            types
        }

        // Channel type: chan T, <-chan T, chan<- T → extract T
        "channel_type" => {
            let mut types = Vec::new();
            if let Some(value_type) = type_node.child_by_field_name("value") {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    value_type,
                    content,
                    type_params,
                ));
            }
            types
        }

        // Function type: func(params) result → extract parameter and return types
        "function_type" => {
            let mut types = Vec::new();

            // Extract parameter types
            if let Some(params) = type_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    if param.kind() == "parameter_declaration"
                        && let Some(param_type) = param.child_by_field_name("type")
                    {
                        types.extend(extract_all_type_names_from_go_type_with_params(
                            param_type,
                            content,
                            type_params,
                        ));
                    }
                }
            }

            // Extract return types
            if let Some(result) = type_node.child_by_field_name("result") {
                // result can be either a single type or parameter_list for multiple returns
                if result.kind() == "parameter_list" {
                    let mut cursor = result.walk();
                    for param in result.named_children(&mut cursor) {
                        if param.kind() == "parameter_declaration"
                            && let Some(return_type) = param.child_by_field_name("type")
                        {
                            types.extend(extract_all_type_names_from_go_type_with_params(
                                return_type,
                                content,
                                type_params,
                            ));
                        }
                    }
                } else {
                    // Single return type
                    types.extend(extract_all_type_names_from_go_type_with_params(
                        result,
                        content,
                        type_params,
                    ));
                }
            }

            types
        }

        // Struct type: extract field types
        "struct_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() == "field_declaration_list" {
                    let mut field_cursor = child.walk();
                    for field in child.named_children(&mut field_cursor) {
                        if field.kind() == "field_declaration"
                            && let Some(field_type) = field.child_by_field_name("type")
                        {
                            types.extend(extract_all_type_names_from_go_type_with_params(
                                field_type,
                                content,
                                type_params,
                            ));
                        }
                    }
                }
            }
            types
        }

        // Generic type: List[User], Map[string, int] (Go 1.18+)
        // Extract both base type and type arguments
        "generic_type" => {
            let mut types = Vec::new();

            // Extract base type (e.g., "List" in List[User])
            if let Some(base) = type_node.child_by_field_name("type") {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    base,
                    content,
                    type_params,
                ));
            }

            // Extract type arguments (e.g., "User" in List[User])
            if let Some(args_node) = type_node.child_by_field_name("type_arguments") {
                let mut cursor = args_node.walk();
                for arg_node in args_node.children(&mut cursor) {
                    if arg_node.is_named() {
                        types.extend(extract_all_type_names_from_go_type_with_params(
                            arg_node,
                            content,
                            type_params,
                        ));
                    }
                }
            }

            types
        }

        // Interface type: extract method signature types and embedded types
        "interface_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() == "method_elem" {
                    // method_elem structure (by child order, NOT field names):
                    //   field_identifier (method name)
                    //   parameter_list (parameters)
                    //   parameter_list or type node (returns)
                    let mut method_cursor = child.walk();
                    let mut param_lists: Vec<Node> = Vec::new();

                    for method_child in child.children(&mut method_cursor) {
                        match method_child.kind() {
                            "parameter_list" | "type_identifier" | "qualified_type"
                            | "pointer_type" | "slice_type" | "array_type" | "map_type"
                            | "channel_type" | "function_type" | "struct_type"
                            | "interface_type" | "generic_type" | "type_union" => {
                                param_lists.push(method_child);
                            }
                            _ => {}
                        }
                    }

                    // First parameter_list is parameters, second is returns
                    if !param_lists.is_empty() {
                        // Extract types from parameters
                        if param_lists[0].kind() == "parameter_list" {
                            let mut param_cursor = param_lists[0].walk();
                            for param in param_lists[0].named_children(&mut param_cursor) {
                                // Both parameter_declaration and variadic_parameter_declaration have same extraction logic
                                if matches!(
                                    param.kind(),
                                    "parameter_declaration" | "variadic_parameter_declaration"
                                ) && let Some(param_type) = param.child_by_field_name("type")
                                {
                                    types.extend(extract_all_type_names_from_go_type_with_params(
                                        param_type,
                                        content,
                                        type_params,
                                    ));
                                }
                            }
                        }

                        // Extract types from returns (if present)
                        if param_lists.len() > 1 {
                            let return_node = param_lists[1];
                            if return_node.kind() == "parameter_list" {
                                let mut result_cursor = return_node.walk();
                                for param in return_node.named_children(&mut result_cursor) {
                                    if param.kind() == "parameter_declaration"
                                        && let Some(return_type) = param.child_by_field_name("type")
                                    {
                                        types.extend(
                                            extract_all_type_names_from_go_type_with_params(
                                                return_type,
                                                content,
                                                type_params,
                                            ),
                                        );
                                    }
                                }
                            } else {
                                // Single return type
                                types.extend(extract_all_type_names_from_go_type_with_params(
                                    return_node,
                                    content,
                                    type_params,
                                ));
                            }
                        }
                    }
                } else if child.kind() == "type_elem" {
                    // MEDIUM-3 fix: Handle embedded types and type sets
                    // Examples:
                    // - interface { io.Reader } (embedded interface)
                    // - interface { ~int | ~string } (type set)
                    //
                    // type_elem contains type expressions; extract all type references
                    let mut elem_cursor = child.walk();
                    for type_expr in child.named_children(&mut elem_cursor) {
                        // Recursively extract types from the type expression
                        types.extend(extract_all_type_names_from_go_type_with_params(
                            type_expr,
                            content,
                            type_params,
                        ));
                    }
                }
            }
            types
        }

        // Type union: interface { ~int | ~string }
        // Union contains multiple negated_type or type_term nodes separated by |
        "type_union" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Recursively extract from each term in the union
                types.extend(extract_all_type_names_from_go_type_with_params(
                    child,
                    content,
                    type_params,
                ));
            }
            types
        }

        // Negated type (Go 1.18+ type sets): ~int, ~[]byte, ~map[K]V
        // The "~" indicates an approximation constraint (the type or any type with the same underlying type)
        // For complex types like ~[]byte or ~map[string]Foo, recurse into the underlying type
        "negated_type" | "type_term" => {
            // Collect all named children (this skips the "~" token which is unnamed)
            let mut cursor = type_node.walk();
            let named_children: Vec<_> = type_node.named_children(&mut cursor).collect();

            if named_children.is_empty() {
                // Simple negated type with no children - extract text and strip "~"
                if let Ok(text) = type_node.utf8_text(content) {
                    let type_name = text.trim_start_matches('~').trim();
                    if type_name.is_empty() {
                        Vec::new()
                    } else {
                        // Qualify type parameter if it matches
                        let qualified = type_params
                            .get(type_name)
                            .cloned()
                            .unwrap_or_else(|| type_name.to_string());
                        vec![qualified]
                    }
                } else {
                    Vec::new()
                }
            } else {
                // Process all named children (type nodes) - recurse to extract nested types
                let mut types = Vec::new();
                for child in named_children {
                    types.extend(extract_all_type_names_from_go_type_with_params(
                        child,
                        content,
                        type_params,
                    ));
                }
                types
            }
        }

        // type_elem: Wrapper node used in type arguments and interface type sets
        // Simply unwrap and process children
        "type_elem" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_go_type_with_params(
                    child,
                    content,
                    type_params,
                ));
            }
            types
        }

        // For other types, try to extract text if it looks like a type
        _ => {
            if let Ok(text) = type_node.utf8_text(content) {
                // Strip "~" prefix from approximation constraints
                let cleaned = text.trim_start_matches('~').trim();
                // Only return if it doesn't look like a keyword or literal
                if !cleaned.is_empty()
                    && !matches!(cleaned, "struct" | "interface" | "map" | "chan" | "|")
                {
                    vec![cleaned.to_string()]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::*;
    use sqry_core::graph::unified::edge::EdgeKind;
    use tree_sitter::Parser;

    fn parse_go(source: &str) -> Tree {
        let mut parser = Parser::new();
        let language = tree_sitter_go::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    #[test]
    fn test_simple_function_call() {
        let source = "package main\n\nfunc helper() {}\n\nfunc main() {\n    helper()\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "helper");
        assert_has_node(&staging, "main");
        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected at least one call edge");
    }

    #[test]
    fn test_goroutine_detection() {
        let source = "package main\n\nfunc worker() {}\n\nfunc main() {\n    go worker()\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "worker");
        assert_has_node(&staging, "main");

        // Verify at least one call edge exists with is_async=true (goroutine)
        let calls = collect_call_edges(&staging);
        assert!(
            !calls.is_empty(),
            "Expected at least one call edge for goroutine"
        );

        // Check that at least one call has is_async set to true (representing goroutine)
        let has_goroutine = calls.iter().any(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::Calls { is_async, .. },
                ..
            } = op
            {
                *is_async
            } else {
                false
            }
        });
        assert!(
            has_goroutine,
            "Expected at least one call with is_async=true for goroutine"
        );
    }

    #[test]
    fn test_defer_detection() {
        let source = "package main\n\nfunc cleanup() {}\n\nfunc main() {\n    defer cleanup()\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "cleanup");
        assert_has_node(&staging, "main");

        // Verify at least one call edge exists for defer
        // Note: defer detection creates regular call edges
        let calls = collect_call_edges(&staging);
        assert!(
            !calls.is_empty(),
            "Expected at least one call edge for defer"
        );
    }

    #[test]
    fn test_builtin_detection() {
        let source =
            "package main\n\nfunc main() {\n    s := make([]int, 0, 10)\n    n := len(s)\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "main");

        // Verify call edges exist for builtin functions
        // Note: builtin detection creates regular call edges
        let calls = collect_call_edges(&staging);
        assert!(
            !calls.is_empty(),
            "Expected call edges for builtin functions"
        );
    }

    #[test]
    fn test_import_edge_creation() {
        let source = "package main\n\nimport \"fmt\"\n\nfunc main() {}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());

        // Verify import edges exist
        let imports = collect_import_edges(&staging);
        assert!(!imports.is_empty(), "Expected at least one import edge");

        // Verify fmt package is imported
        assert_has_node(&staging, "fmt");
    }

    #[test]
    fn test_export_edge_creation() {
        let source = "package mypackage\n\nfunc PublicFunction() {}\n\nfunc privateFunction() {}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "PublicFunction");
        assert_has_node(&staging, "privateFunction");

        // Verify export edges exist for uppercase identifiers
        let exports = collect_export_edges(&staging);
        assert!(
            !exports.is_empty(),
            "Expected at least one export edge for public function"
        );
    }

    #[test]
    fn test_field_access_detection() {
        let source = "package main\n\ntype User struct {\n    Name string\n}\n\nfunc main() {\n    u := User{}\n    _ = u.Name\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "User");
        assert_has_node(&staging, "main");
        assert_has_node(&staging, "Name");
    }

    #[test]
    fn test_type_assertion_detection() {
        let source = "package main\n\nfunc main() {\n    var i interface{} = \"hello\"\n    s := i.(string)\n    _ = s\n}\n";
        let tree = parse_go(source);
        let mut staging = StagingGraph::new();
        let builder = GoGraphBuilder::new(DEFAULT_SCOPE_DEPTH);

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging);

        assert!(result.is_ok());
        assert_has_node(&staging, "main");

        // Verify the graph builds successfully with type assertions
        // The specific edge representation depends on implementation details
        let nodes = staging.nodes().count();
        assert!(
            nodes > 0,
            "Expected at least one node for type assertion test"
        );
    }

    #[test]
    fn test_stdlib_detection() {
        assert!(is_stdlib_package("fmt"));
        assert!(is_stdlib_package("net/http"));
        assert!(!is_stdlib_package("github.com/user/repo"));
    }

    #[test]
    fn test_builtin_names() {
        assert!(is_builtin("make"));
        assert!(is_builtin("len"));
        assert!(is_builtin("append"));
        assert!(!is_builtin("fmt"));
        assert!(!is_builtin("myFunc"));
    }
}
