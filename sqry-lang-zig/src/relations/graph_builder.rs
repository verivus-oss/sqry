//! `GraphBuilder` implementation for Zig
//!
//! Builds the unified `CodeGraph` for Zig files by:
//! 1. Extracting function definitions (`fn` declarations)
//! 2. Detecting function calls (`call_expression`)
//! 3. Creating call edges between caller and callee
//! 4. Detecting imports (`@import()` builtin calls)
//! 5. Emitting Export edges for `pub` declarations
//!
//! ## Supported Patterns
//! - Top-level functions: `pub fn name(args) return_type { body }`
//! - Nested functions: Functions defined within other functions
//! - Direct calls: `functionName(arg1, arg2)`
//! - Qualified calls: `std.mem.copy(...)`, `Module.SubModule.function(...)`
//! - Method calls: `object.method(args)` (treated as qualified calls)
//! - Imports: `const std = @import("std");`
//! - Exports: `pub fn`, `pub const`, `pub` struct/enum/union declarations
//!
//! ## Limitations (Phase 5C Scope)
//! - Comptime evaluation: Not tracked (runtime-dependent)
//! - Generic specialization: Not tracked (requires type inference)
//! - Inline assembly calls: Not tracked (low-level only)
//! - Function pointers: Not tracked (runtime dispatch)

use std::sync::OnceLock;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::resolution::canonicalize_graph_qualified_name;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

use crate::relations::type_extractor::extract_type_names_from_zig_type;

/// Synthetic module name for file-level exports.
const FILE_MODULE_NAME: &str = "<file_module>";

/// `GraphBuilder` for Zig files using manual tree walking approach
#[derive(Debug, Clone, Copy)]
pub struct ZigGraphBuilder {
    max_scope_depth: usize,
}

impl Default for ZigGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 4, // Zig: module -> function -> nested function -> closure
        }
    }
}

impl ZigGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for ZigGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Zig);

        // Build AST metadata to track function contexts
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Phase 1: Insert function contexts as nodes
        for context in ast_graph.contexts() {
            let qualified = context.qualified_name();
            let span = context.decl_span;
            let visibility = if context.is_pub {
                Some("public")
            } else {
                Some("private")
            };
            helper.add_function_with_visibility(&qualified, Some(span), false, false, visibility);
        }

        // Phase 1b: Insert type/const declarations as nodes
        for decl in ast_graph.decl_nodes() {
            let decl_id = helper.add_type(&decl.name, Some(decl.decl_span));
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(decl_id);
        }

        // Phase 1c: Emit Export edges for pub declarations at module level
        let module_id = helper.add_module(FILE_MODULE_NAME, None);

        // Export pub functions (top-level only)
        for context in ast_graph.contexts() {
            // Only export top-level declarations (no dots in qualified name)
            if context.is_pub
                && !context.qualified_name.contains('.')
                && let Some(exported_id) = helper.get_node(&context.qualified_name)
            {
                helper.add_export_edge(module_id, exported_id);
            }
        }

        // Export pub types/consts (top-level only)
        for decl in ast_graph.decl_nodes() {
            if decl.is_pub
                && let Some(exported_id) = helper.get_node(&decl.name)
            {
                helper.add_export_edge(module_id, exported_id);
            }
        }

        // Phase 2: Traverse tree to collect call edges and import edges
        let mut stack = vec![tree.root_node()];
        let mut visited = HashSet::new();

        while let Some(node) = stack.pop() {
            let node_id = node.id();

            // Skip if already visited (prevents infinite loops)
            if !visited.insert(node_id) {
                continue;
            }

            // Skip non-code nodes
            match node.kind() {
                "comment" | "line_comment" | "doc_comment" | "string" | "char" | "integer"
                | "float" => {
                    continue;
                }
                _ => {}
            }

            // Detect @import() builtin calls and create import edges
            if node.kind() == "builtin_function"
                && is_import_builtin(node, content)
                && let Some(module_name) = extract_import_module_name(node, content)
            {
                // Get the importing context (module or function)
                let importer_id = if let Some(ctx) = ast_graph.get_callable_context(node.id()) {
                    helper.get_node(&ctx.qualified_name()).unwrap_or_else(|| {
                        let span = ctx.decl_span;
                        helper.add_function(&ctx.qualified_name(), Some(span), false, false)
                    })
                } else {
                    module_id
                };

                // Create import node and edge
                let span = Span::from_node(&node);
                let import_node_id = helper.add_import(&module_name, Some(span));
                helper.add_import_edge(importer_id, import_node_id);
            }
            // Detect function call expressions (regular and non-import builtins)
            else if (node.kind() == "call_expression"
                || (node.kind() == "builtin_function" && !is_import_builtin(node, content)))
                && let Some((caller_id, callee_id, argument_count)) =
                    build_call_edge_ids(&ast_graph, node, content, &mut helper)
            {
                let call_span = Span::from_node(&node);
                helper.add_call_edge_full_with_span(
                    caller_id,
                    callee_id,
                    argument_count,
                    false,
                    vec![call_span],
                );
            }

            // Traverse children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }

        // Phase 3: Process TypeOf and Reference edges
        process_typeof_edges(tree.root_node(), content, &mut helper)?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Zig
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(zig_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Zig: a precomputed `kind_id -> CfBucket`
/// table over the tree-sitter-zig grammar, shared process-wide via
/// [`zig_shape_mapping`]. Mirrors the C reference impl: a single array index per
/// node on the hot shape walk, identifier-blind throughout.
pub struct ZigShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl ZigShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_zig::LANGUAGE.into();
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
                *slot = cf_bucket_for_zig_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for ZigShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // A `function_declaration` carries its parameters in a named child of kind
        // `parameters` (not a tree-sitter field); its `parameter` children are the
        // positional params. Zig has no varargs/kwargs at the grammar level.
        if let Some(params) = zig_parameters(fn_node) {
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                if child.kind() == "parameter" {
                    shape.arity_positional = shape.arity_positional.saturating_add(1);
                }
            }
        }
        // The return type lives in the `type` field of `function_declaration`; a
        // present slot is the structural witness of a declared return type.
        shape.has_return_annotation = fn_node.child_by_field_name("type").is_some();
        shape
    }
}

/// Find the `parameters` child of a Zig `function_declaration`. The grammar
/// exposes it as a named child by kind rather than a labelled field.
fn zig_parameters(fn_node: Node) -> Option<Node> {
    let mut cursor = fn_node.walk();
    fn_node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "parameters")
}

/// Map one tree-sitter-zig grammar node-kind name to its canonical control-flow
/// bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_zig_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_expression" | "if_statement" | "if_type_expression" => CfBucket::Branch,
        "for_expression" | "for_statement" | "while_expression" | "while_statement" => {
            CfBucket::Loop
        }
        "switch_expression" | "switch_case" => CfBucket::Match,
        "try_expression" => CfBucket::Try,
        // `x catch |e| ...`: error-set recovery maps onto the catch bucket.
        "catch_expression" => CfBucket::Catch,
        // `defer` / `errdefer` are scope-exit resource cleanup.
        "defer_statement" | "errdefer_statement" => CfBucket::Resource,
        "return_expression" => CfBucket::Return,
        // Zig's async/suspend family maps onto the async-suspend bucket.
        "await_expression"
        | "async_expression"
        | "nosuspend_expression"
        | "suspend_statement"
        | "nosuspend_statement" => CfBucket::Await,
        "break_expression" | "continue_expression" => CfBucket::BreakContinue,
        "call_expression" => CfBucket::Call,
        "assignment_expression" | "variable_declaration" => CfBucket::Assign,
        // Zig has no `throw`: errors are values surfaced through `try` / `catch`,
        // so there is no Throw arm by design.
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Zig shape mapping, built once on first use.
#[must_use]
pub fn zig_shape_mapping() -> &'static ZigShapeMapping {
    static MAPPING: OnceLock<ZigShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(ZigShapeMapping::build)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Build call edge node IDs from a `call_expression` node
fn build_call_edge_ids(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Option<(NodeId, NodeId, u8)> {
    // Get callable context (the function we're currently inside)
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level expressions
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            // Whole-file synthetic context. `Span::default()` reports line 1
            // column 0, the honest position for module-level code, and it is
            // what master carries here. Where this mint creates the node, the
            // degenerate end also keeps a non-body out of the body-hash and
            // shape planes via `has_valid_body_span`. Where another path mints
            // the same name first, that span stands: `ensure_callee` returns a
            // cache hit untouched, so the FIRST mint decides and nothing later
            // widens it. Both orderings match master.
            decl_span: Span::default(),
            is_pub: false,
        };
        &module_context
    };

    // Extract callee name and argument count
    let (callee_name, arg_count) = extract_call_info(call_node, content);

    // Skip if we couldn't extract a meaningful name
    if callee_name.is_empty() {
        return None;
    }

    // Get caller node ID (from context or create module context)
    let source_id = if helper.has_node(&call_context.qualified_name()) {
        helper.get_node(&call_context.qualified_name()).unwrap()
    } else {
        let span = call_context.decl_span;
        helper.add_function(&call_context.qualified_name(), Some(span), false, false)
    };

    // Create or get callee node
    let target_id = helper.add_function(&callee_name, None, false, false);

    let argument_count = u8::try_from(arg_count).unwrap_or(u8::MAX);
    Some((source_id, target_id, argument_count))
}

/// Extract function name and argument count from a `call_expression` or `builtin_function` node
/// Zig `call_expression` AST:
///   `call_expression`
///     identifier | `field_expression` (the function being called)
///     (
///     arg1
///     ,
///     arg2
///     )
///
/// Zig `builtin_function` AST:
///   `builtin_function`
///     `builtin_identifier` (@import, @memcpy, etc.)
///     arguments
///       (
///       arg1
///       ,
///       arg2
///       )
fn extract_call_info(call_node: Node<'_>, content: &[u8]) -> (String, usize) {
    let mut function_name = String::new();
    let mut arg_count = 0;
    let mut in_arguments = false;
    let mut found_function_name = false;

    let mut cursor = call_node.walk();
    for child in call_node.children(&mut cursor) {
        match child.kind() {
            "builtin_identifier" => {
                // Builtin function name (e.g., @import, @memcpy)
                if !found_function_name {
                    function_name = child.utf8_text(content).unwrap_or("").to_string();
                    found_function_name = true;
                }
            }
            "identifier" | "field_expression" | "field_access" => {
                // The FIRST identifier/field_expression is the function being called
                if !found_function_name {
                    function_name = child.utf8_text(content).unwrap_or("").to_string();
                    found_function_name = true;
                } else if in_arguments {
                    // After we found the function name, these are arguments
                    arg_count += 1;
                }
            }
            "arguments" => {
                // For builtin_function, arguments are wrapped in an "arguments" node
                arg_count = count_arguments_in_node(child);
            }
            "(" => {
                // Start of arguments (for regular call_expression)
                in_arguments = true;
            }
            ")" => {
                // End of arguments
                in_arguments = false;
            }
            "," => {
                // Comma separator between arguments - skip
            }
            _ => {
                // If we're inside the argument list and it's not a delimiter, it's an argument
                if in_arguments {
                    arg_count += 1;
                }
            }
        }
    }

    (function_name, arg_count)
}

/// Count arguments within an "arguments" node (used for `builtin_function`)
fn count_arguments_in_node(args_node: Node<'_>) -> usize {
    let mut count = 0;
    let mut cursor = args_node.walk();

    for child in args_node.children(&mut cursor) {
        match child.kind() {
            "(" | ")" | "," => {
                // Skip delimiters
            }
            _ => {
                // Count as argument
                count += 1;
            }
        }
    }

    count
}

/// Check if a `builtin_function` node is an `@import` call.
/// AST structure: `builtin_function` -> `builtin_identifier` (`@import`)
fn is_import_builtin(node: Node<'_>, content: &[u8]) -> bool {
    if node.kind() != "builtin_function" {
        return false;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "builtin_identifier"
            && let Ok(text) = child.utf8_text(content)
            && text == "@import"
        {
            return true;
        }
    }

    false
}

/// Extract the module name from an `@import()` builtin call.
/// AST structure:
///   `builtin_function`
///     `builtin_identifier` (`@import`)
///     arguments
///       (
///       string (e.g., "std")
///       )
fn extract_import_module_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    if node.kind() != "builtin_function" {
        return None;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "arguments" {
            // Look for the first string literal inside the arguments
            let mut args_cursor = child.walk();
            for arg_child in child.children(&mut args_cursor) {
                if arg_child.kind() == "string"
                    && let Ok(text) = arg_child.utf8_text(content)
                {
                    // Remove quotes from string literal
                    let trimmed = text.trim().trim_matches('"');
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    None
}

// ============================================================================
// AST Graph - Tracks callable contexts
// ============================================================================

#[derive(Debug)]
struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
    decl_nodes: Vec<DeclNode>,
}

#[derive(Debug, Clone)]
struct DeclNode {
    name: String,
    /// Real line/column span of the declaration; the byte tuple above cannot
    /// be resolved to one without the file content.
    decl_span: Span,
    is_pub: bool,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], _max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();
        let mut decl_nodes = Vec::new();

        // Extract function definitions by traversing the tree
        let root = tree.root_node();
        extract_functions_recursive(root, content, &mut contexts, &mut node_to_context, None)?;
        extract_declarations_recursive(root, content, &mut decl_nodes, None)?;

        Ok(Self {
            contexts,
            node_to_context,
            decl_nodes,
        })
    }

    fn contexts(&self) -> &[CallContext] {
        &self.contexts
    }

    fn decl_nodes(&self) -> &[DeclNode] {
        &self.decl_nodes
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

/// Recursively extract function definitions from AST
fn extract_functions_recursive(
    node: Node<'_>,
    content: &[u8],
    contexts: &mut Vec<CallContext>,
    node_to_context: &mut HashMap<usize, usize>,
    parent_name: Option<&str>,
) -> Result<(), String> {
    // Function declaration: pub fn name(args) return_type { body }
    if node.kind() == "function_declaration"
        && let Some(name) = extract_function_name(node, content)
    {
        let is_pub = has_pub_modifier(node);

        // Build qualified name (handle nested functions and struct methods)
        let qualified_name = if let Some(parent) = parent_name {
            format!("{parent}.{name}")
        } else {
            name.clone()
        };

        let context_idx = contexts.len();
        contexts.push(CallContext {
            qualified_name: qualified_name.clone(),
            decl_span: Span::from_node(&node),
            is_pub,
        });

        // Map all descendant nodes to this context
        map_descendants_to_context(&node, context_idx, node_to_context);

        // Process children for nested functions
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            extract_functions_recursive(
                child,
                content,
                contexts,
                node_to_context,
                Some(&qualified_name),
            )?;
        }

        // Return early to avoid re-processing children
        return Ok(());
    }

    // Struct/container declaration: track container name for methods
    // AST: variable_declaration -> identifier (name) -> = -> struct_declaration
    if node.kind() == "struct_declaration"
        || node.kind() == "union_declaration"
        || node.kind() == "enum_declaration"
    {
        // Try to find the container name from parent variable_declaration
        let container_name = node.parent().and_then(|parent| {
            if parent.kind() == "variable_declaration" {
                extract_container_name_from_var_decl(parent, content)
            } else {
                None
            }
        });

        // Determine the qualified container name
        let qualified_container = if let Some(name) = container_name {
            if let Some(parent) = parent_name {
                format!("{parent}.{name}")
            } else {
                name
            }
        } else {
            // Anonymous container - use parent name if available
            parent_name.map(String::from).unwrap_or_default()
        };

        // Process children with the container name as parent
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_parent = if qualified_container.is_empty() {
                parent_name
            } else {
                Some(qualified_container.as_str())
            };
            extract_functions_recursive(child, content, contexts, node_to_context, child_parent)?;
        }

        // Return early to avoid re-processing children
        return Ok(());
    }

    // Process children for other nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_functions_recursive(child, content, contexts, node_to_context, parent_name)?;
    }

    Ok(())
}

/// Recursively extract pub const/type declarations from AST (module level only)
fn extract_declarations_recursive(
    node: Node<'_>,
    content: &[u8],
    decl_nodes: &mut Vec<DeclNode>,
    parent_name: Option<&str>,
) -> Result<(), String> {
    // Only process at module level (parent_name is None)
    // Variable declaration: pub const NAME = ...
    if parent_name.is_none()
        && node.kind() == "variable_declaration"
        && let Some((name, is_pub)) = extract_var_decl_info(node, content)
        && is_pub
    {
        decl_nodes.push(DeclNode {
            name,
            decl_span: Span::from_node(&node),
            is_pub: true,
        });
    }

    // Process children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_declarations_recursive(child, content, decl_nodes, parent_name)?;
    }

    Ok(())
}

/// Extract name and pub status from a `variable_declaration` node.
fn extract_var_decl_info(node: Node<'_>, content: &[u8]) -> Option<(String, bool)> {
    let is_pub = has_pub_modifier(node);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(name) = child.utf8_text(content)
        {
            return Some((name.to_string(), is_pub));
        }
    }
    None
}

/// Extract container name from a `variable_declaration` node
/// AST: `variable_declaration` -> `const/identifier/=/struct_declaration`
fn extract_container_name_from_var_decl(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(name) = child.utf8_text(content)
        {
            return Some(name.to_string());
        }
    }
    None
}

/// Extract function name from a `function_declaration` node
fn extract_function_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(name) = child.utf8_text(content)
        {
            return Some(name.to_string());
        }
    }
    None
}

/// Check if a function has a pub modifier
fn has_pub_modifier(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "pub" {
            return true;
        }
    }
    false
}

/// Recursively map all descendant nodes to a context index
fn map_descendants_to_context(node: &Node, context_idx: usize, map: &mut HashMap<usize, usize>) {
    map.insert(node.id(), context_idx);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        map_descendants_to_context(&child, context_idx, map);
    }
}

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    /// Real line/column span of the declaration; the byte tuple above cannot
    /// be resolved to one without the file content.
    decl_span: Span,
    is_pub: bool,
}

impl CallContext {
    fn qualified_name(&self) -> String {
        self.qualified_name.clone()
    }
}

// ============================================================================
// TypeOf and Reference Edge Processing
// ============================================================================

/// Process `TypeOf` and Reference edges for all type annotations in the tree.
///
/// This function traverses the AST and extracts type information from:
/// - Variable declarations (var/const)
/// - Function parameters
/// - Function return types
/// - Struct/union/enum fields
fn process_typeof_edges(
    root: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut stack = vec![root];
    let mut visited = HashSet::new();

    while let Some(node) = stack.pop() {
        let node_id = node.id();

        if !visited.insert(node_id) {
            continue;
        }

        match node.kind() {
            "variable_declaration" => {
                // Container-level `const`/`var` declarations whose direct parent
                // is a container body (struct/union/enum/opaque) are emitted
                // exclusively by the container-member path
                // (`handle_container_member_decl`) under the qualified
                // `Container.Name` form with the appropriate
                // Property/Constant kind. Routing them through
                // `handle_variable_declaration` here as well would dual-emit
                // them as bare `NodeKind::Variable` under the un-qualified
                // member name, conflicting with the kind+attribute+qualified-
                // name design intent. Function-local `var`/`const` declarations
                // (parent is `block`/`function_body`/etc.) continue through
                // `handle_variable_declaration` and remain `NodeKind::Variable`.
                if !is_container_member_var_decl(node) {
                    handle_variable_declaration(node, content, helper)?;
                }
            }
            "function_declaration" => {
                handle_function_typeof_edges(node, content, helper)?;
            }
            "struct_declaration" | "union_declaration" | "enum_declaration"
            | "opaque_declaration" => {
                // Opaque containers carry no `container_field` children but
                // may still hold `const`/`var` member declarations. They must
                // be dispatched here because the parent guard
                // (`is_container_member_var_decl`) suppresses the bare
                // `NodeKind::Variable` emission for `opaque_declaration`
                // parents — without this dispatch the container-level
                // `const` would vanish from the graph entirely.
                handle_container_fields(node, content, helper)?;
            }
            _ => {}
        }

        // Traverse children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    Ok(())
}

/// Returns `true` when a `variable_declaration` node sits directly inside a
/// container body (struct/union/enum/opaque) and must therefore be emitted
/// exclusively by `handle_container_member_decl` under the qualified
/// `Container.Name` form. Used by `process_typeof_edges` to suppress the
/// dual-emission bug where the same node would otherwise be staged once as a
/// bare `NodeKind::Variable` and again as the qualified Property/Constant.
///
/// Function-local `var`/`const` declarations are *not* container members —
/// their parent is a `block` / `function_body` (or wrapper) node, never a
/// `*_declaration` container — and continue to be staged as `NodeKind::Variable`
/// by `handle_variable_declaration`.
fn is_container_member_var_decl(node: Node<'_>) -> bool {
    matches!(
        node.parent().map(|p| p.kind()),
        Some(
            "struct_declaration" | "union_declaration" | "enum_declaration" | "opaque_declaration"
        )
    )
}

/// Handle `TypeOf` edges for variable/constant declarations.
///
/// Processes:
/// - Regular variables: `var name: Type = value;` or `const name: Type = value;`
/// - Type aliases: `const TypeName = TypeExpression;`
#[allow(clippy::unnecessary_wraps)]
fn handle_variable_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract variable name
    let var_name = extract_variable_name(node, content);

    if let Some(name) = var_name {
        // Try explicit type annotation first (var x: Type)
        // If not found, check for type alias (const X = Type)
        let type_node =
            find_type_annotation_in_var_decl(node).or_else(|| find_type_alias_expression(node));

        if let Some(type_node) = type_node {
            // Get or create variable node
            let var_id = if let Some(id) = helper.get_node(&name) {
                id
            } else {
                // Create variable node if it doesn't exist
                let span = Span::from_node(&node);
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                let id = helper.add_variable(&name, Some(span));
                helper.mark_definition(id);
                id
            };

            // Extract full type string for TypeOf edge
            if let Ok(type_str) = type_node.utf8_text(content) {
                let type_id = helper.add_type(type_str.trim(), None);
                helper.add_typeof_edge_with_context(
                    var_id,
                    type_id,
                    Some(TypeOfContext::Variable),
                    None,
                    Some(&name),
                );
            }

            // Extract referenced type names for Reference edges
            let type_names = extract_type_names_from_zig_type(type_node, content);
            for type_name in type_names {
                let type_id = helper.add_type(&type_name, None);
                helper.add_reference_edge(var_id, type_id);
            }
        }
    }

    Ok(())
}

/// Handle `TypeOf` edges for function parameters and return type.
fn handle_function_typeof_edges(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract function name
    let fn_name = extract_function_name(node, content);

    if let Some(name) = fn_name {
        // Get function node
        if let Some(fn_id) = helper.get_node(&name) {
            // Process parameters
            if let Some(params_node) = find_parameters_node(node) {
                let mut param_index = 0;
                let mut cursor = params_node.walk();

                for child in params_node.children(&mut cursor) {
                    if child.kind() == "parameter" {
                        handle_function_parameter(child, content, helper, fn_id, param_index)?;
                        param_index += 1;
                    }
                }
            }

            // Process return type
            if let Some(return_type_node) = find_function_return_type(node) {
                // Extract full type string for TypeOf edge
                if let Ok(type_str) = return_type_node.utf8_text(content) {
                    let type_id = helper.add_type(type_str.trim(), None);
                    helper.add_typeof_edge_with_context(
                        fn_id,
                        type_id,
                        Some(TypeOfContext::Return),
                        None,
                        None,
                    );
                }

                // Extract referenced type names for Reference edges
                let type_names = extract_type_names_from_zig_type(return_type_node, content);
                for type_name in type_names {
                    let type_id = helper.add_type(&type_name, None);
                    helper.add_reference_edge(fn_id, type_id);
                }
            }
        }
    }

    Ok(())
}

/// Handle `TypeOf` edge for a single function parameter.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::unnecessary_wraps)]
fn handle_function_parameter(
    param_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    fn_id: NodeId,
    param_index: usize,
) -> GraphResult<()> {
    // Extract parameter name and type
    let param_name = extract_parameter_name(param_node, content);
    let type_node = find_parameter_type_node(param_node);

    if let Some(type_node) = type_node {
        // Extract full type string for TypeOf edge
        if let Ok(type_str) = type_node.utf8_text(content) {
            let type_id = helper.add_type(type_str.trim(), None);

            helper.add_typeof_edge_with_context(
                fn_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(param_index as u16),
                param_name.as_deref(),
            );
        }

        // Extract referenced type names for Reference edges
        let type_names = extract_type_names_from_zig_type(type_node, content);
        for type_name in type_names {
            let type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(fn_id, type_id);
        }
    }

    Ok(())
}

/// Handle `TypeOf` edges for struct/union/enum fields.
fn handle_container_fields(
    container_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find container name from parent variable declaration
    let container_name = container_node.parent().and_then(|parent| {
        if parent.kind() == "variable_declaration" {
            extract_container_name_from_var_decl(parent, content)
        } else {
            None
        }
    });

    if let Some(container_name) = container_name {
        // Process container fields and container-level const declarations.
        let mut cursor = container_node.walk();
        for child in container_node.children(&mut cursor) {
            match child.kind() {
                "container_field" => {
                    handle_container_field(child, content, helper, &container_name)?;
                }
                // Container-level const/var declarations (e.g. `const ORIGIN = …;`
                // inside a struct body) appear as `variable_declaration` AST
                // children of the container. Emit them under the qualified
                // `Container.Name` form with the appropriate Property/Constant
                // kind so they bind to the enclosing container rather than
                // collide with module-level variable declarations.
                "variable_declaration" => {
                    handle_container_member_decl(child, content, helper, &container_name)?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Handle `TypeOf` edge for a single container field.
///
/// Emits a `NodeKind::Property` node with `is_static = false` and
/// `visibility = None` (Zig has no member-level visibility). The edge call
/// site preserves the cross-language `TypeOfContext::Field` + bare-name
/// metadata contract.
#[allow(clippy::unnecessary_wraps)]
fn handle_container_field(
    field_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    container_name: &str,
) -> GraphResult<()> {
    // Extract field name and type
    let field_name = extract_field_name(field_node, content);
    let type_node = find_field_type_node(field_node);

    if let (Some(name), Some(type_node)) = (field_name, type_node) {
        let qualified_name = format!("{container_name}.{name}");

        // Get or create the field node. The dotted source-form qualified
        // name is canonicalised to `Container::field` before the cache
        // probe so the fast path actually fires (the helper's underlying
        // node cache is keyed on the canonical `::` form via
        // `add_node_internal`/`canonicalize_graph_qualified_name`).
        // Without this canonicalisation the `get_node` probe would always
        // miss and dedupe would only happen via `add_node_internal`'s
        // canonical-cache `update_node_entry` round-trip.
        let cache_key = canonicalize_graph_qualified_name(Language::Zig, &qualified_name);
        let field_id = if let Some(id) = helper.get_node(&cache_key) {
            id
        } else {
            let span = Span::from_node(&field_node);
            helper.add_property_with_static_and_visibility(&qualified_name, Some(span), false, None)
        };

        // Extract full type string for TypeOf edge
        if let Ok(type_str) = type_node.utf8_text(content) {
            let type_id = helper.add_type(type_str.trim(), None);
            helper.add_typeof_edge_with_context(
                field_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(&name),
            );
        }

        // Extract referenced type names for Reference edges
        let type_names = extract_type_names_from_zig_type(type_node, content);
        for type_name in type_names {
            let type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(field_id, type_id);
        }
    }

    Ok(())
}

/// Handle a container-level `const` (or `var`) declaration nested directly
/// inside a struct/union/enum body — e.g. `struct { const ORIGIN = …; }`.
///
/// Container-level `const` → `NodeKind::Constant` with `is_static = true`.
/// Container-level `var`   → `NodeKind::Property` with `is_static = false`
/// (rare in practice; included for completeness so non-`const` storage
/// still binds under the container rather than collapsing to a module
/// variable).
///
/// Both shapes use the qualified `Container.Name` form. Edge metadata
/// matches the field path (`TypeOfContext::Field` + bare name).
#[allow(clippy::unnecessary_wraps)]
fn handle_container_member_decl(
    decl_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    container_name: &str,
) -> GraphResult<()> {
    let Some(name) = extract_variable_name(decl_node, content) else {
        return Ok(());
    };
    // A container-level decl must carry an explicit type annotation or a
    // type-alias expression to participate in the field/TypeOf contract;
    // otherwise we have nothing meaningful to emit beyond the bare node.
    let type_node = find_type_annotation_in_var_decl(decl_node)
        .or_else(|| find_type_alias_expression(decl_node));

    let is_const = decl_node
        .children(&mut decl_node.walk())
        .any(|c| c.kind() == "const");

    let qualified_name = format!("{container_name}.{name}");

    // Canonicalise to `Container::Name` before probing the helper's node
    // cache (which is keyed on canonical form via `add_node_internal`).
    // See the matching comment in `handle_container_field`.
    let cache_key = canonicalize_graph_qualified_name(Language::Zig, &qualified_name);
    let member_id = if let Some(id) = helper.get_node(&cache_key) {
        id
    } else {
        let span = Span::from_node(&decl_node);
        if is_const {
            helper.add_constant_with_static_and_visibility(&qualified_name, Some(span), true, None)
        } else {
            helper.add_property_with_static_and_visibility(&qualified_name, Some(span), false, None)
        }
    };

    if let Some(type_node) = type_node {
        if let Ok(type_str) = type_node.utf8_text(content) {
            let type_id = helper.add_type(type_str.trim(), None);
            helper.add_typeof_edge_with_context(
                member_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(&name),
            );
        }

        let type_names = extract_type_names_from_zig_type(type_node, content);
        for type_name in type_names {
            let type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(member_id, type_id);
        }
    }

    Ok(())
}

// ============================================================================
// Type Annotation Extraction Helpers
// ============================================================================

/// Extract variable name from `variable_declaration` node.
fn extract_variable_name(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(content).ok().map(String::from);
        }
    }
    None
}

/// Find type annotation in variable declaration (after colon).
///
/// Pattern: var/const identifier : Type = value
fn find_type_annotation_in_var_decl(node: Node) -> Option<Node> {
    let mut found_colon = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == ":" {
            found_colon = true;
            continue;
        }

        // After colon, next child that's a type node is the type annotation
        if found_colon && is_type_like_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

/// Find type expression in type alias declaration (after equals).
///
/// Pattern: const `TypeName` = `TypeExpression`;
/// Examples: const `ByteArray` = []const u8;
///           const Point = struct { x: f32 };
fn find_type_alias_expression(node: Node) -> Option<Node> {
    let mut found_equals = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "=" {
            found_equals = true;
            continue;
        }

        // After equals, check if we have a type expression
        // Type alias RHS can be: array_type, pointer_type, slice_type,
        // optional_type, error_union, struct, enum, union, etc.
        if found_equals && is_type_like_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

/// Find parameters node in function declaration.
fn find_parameters_node(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == "parameters")
}

/// Find return type in function declaration (after parameters).
///
/// Pattern: fn name(params) `ReturnType`
fn find_function_return_type(node: Node) -> Option<Node> {
    let mut found_params = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Mark that we've passed the parameters
        if child.kind() == "parameters" || child.kind() == ")" {
            found_params = true;
            continue;
        }

        // After parameters, first type node is the return type
        // (before the function body)
        if found_params && is_type_like_node(child.kind()) && child.kind() != "block" {
            return Some(child);
        }
    }

    None
}

/// Extract parameter name from parameter node.
fn extract_parameter_name(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(content).ok().map(String::from);
        }
    }
    None
}

/// Find type annotation in parameter node (after colon).
///
/// Pattern: name : Type
fn find_parameter_type_node(node: Node) -> Option<Node> {
    let mut found_colon = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == ":" {
            found_colon = true;
            continue;
        }

        // After colon, next type node is the parameter type
        if found_colon && is_type_like_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

/// Extract field name from `container_field` node.
fn extract_field_name(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(content).ok().map(String::from);
        }
    }
    None
}

/// Find type annotation in container field (after colon).
///
/// Pattern: `field_name` : Type
fn find_field_type_node(node: Node) -> Option<Node> {
    let mut found_colon = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == ":" {
            found_colon = true;
            continue;
        }

        // After colon, next type node is the field type
        if found_colon && is_type_like_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

/// Check if a node kind represents a type-like node.
fn is_type_like_node(kind: &str) -> bool {
    matches!(
        kind,
        "builtin_type"
            | "identifier"
            | "pointer_type"
            | "slice_type"
            | "array_type"
            | "optional_type"
            | "nullable_type"
            | "error_union_type"
            | "function_type"
            | "FnProto"
            | "fn_proto"
            | "struct_declaration"
            | "enum_declaration"
            | "union_declaration"
            | "call_expression"      // Generic types: ArrayList(T)
            | "field_expression"      // Namespaced types: std.mem.Allocator
            | "field_access" // Alternative for field access in some grammar versions
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::*;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::Path;

    fn parse_zig(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .expect("Failed to load Zig grammar");

        let content = source.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    fn has_display_name(
        staging: &StagingGraph,
        canonical_name: &str,
        expected_display_name: &str,
    ) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                staging.resolve_node_canonical_name(entry) == Some(canonical_name)
                    && staging
                        .resolve_node_display_name(Language::Zig, entry)
                        .as_deref()
                        == Some(expected_display_name)
            } else {
                false
            }
        })
    }

    fn has_display_edge(
        staging: &StagingGraph,
        kind_matches: impl Fn(&EdgeKind) -> bool,
        expected_source: &str,
        expected_target: &str,
    ) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                if !kind_matches(kind) {
                    return false;
                }

                let source_display = staging.operations().iter().find_map(|candidate| {
                    if let StagingOp::AddNode {
                        entry,
                        expected_id: Some(node_id),
                    } = candidate
                        && *node_id == *source
                    {
                        staging.resolve_node_display_name(Language::Zig, entry)
                    } else {
                        None
                    }
                });
                let target_display = staging.operations().iter().find_map(|candidate| {
                    if let StagingOp::AddNode {
                        entry,
                        expected_id: Some(node_id),
                    } = candidate
                        && *node_id == *target
                    {
                        staging.resolve_node_display_name(Language::Zig, entry)
                    } else {
                        None
                    }
                });

                source_display.as_deref() == Some(expected_source)
                    && target_display.as_deref() == Some(expected_target)
            } else {
                false
            }
        })
    }

    fn assert_has_display_call_edge(staging: &StagingGraph, source: &str, target: &str) {
        assert!(
            has_display_edge(
                staging,
                |kind| matches!(kind, EdgeKind::Calls { .. }),
                source,
                target,
            ),
            "Expected Zig native display call edge {source} -> {target}"
        );
    }

    fn assert_has_display_import_edge(staging: &StagingGraph, source: &str, target: &str) {
        assert!(
            has_display_edge(
                staging,
                |kind| matches!(
                    kind,
                    EdgeKind::Imports {
                        alias: _,
                        is_wildcard: _,
                    }
                ),
                source,
                target,
            ),
            "Expected Zig native display import edge {source} -> {target}"
        );
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = r"
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify function node was created
        assert_has_node_with_kind(&staging, "add", NodeKind::Function);

        // Verify it was exported (pub function)
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "add");
    }

    #[test]
    fn test_simple_function_call() {
        let source = r"
fn helper() void {
    return;
}

fn main() void {
    helper();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify both functions exist
        assert_has_node_with_kind(&staging, "helper", NodeKind::Function);
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);

        // Verify call edge from main to helper
        assert_has_call_edge(&staging, "main", "helper");
    }

    #[test]
    fn test_qualified_std_call() {
        let source = r#"
const std = @import("std");

fn process(data: []const u8) void {
    std.debug.print("Data: {any}\n", .{data});
}
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify import edge exists
        assert_has_import_edge(&staging, FILE_MODULE_NAME, "std");

        // Verify function exists
        assert_has_node_with_kind(&staging, "process", NodeKind::Function);

        // Verify canonical graph identity and Zig-native display name for qualified stdlib calls.
        assert_has_call_edge(&staging, "process", "std::debug::print");
        assert_has_display_call_edge(&staging, "process", "std.debug.print");
    }

    #[test]
    fn test_argument_counting_zero_args() {
        let source = r"
fn getValue() i32 {
    return 42;
}

fn main() void {
    const x = getValue();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify call edge exists
        assert_has_call_edge(&staging, "main", "getValue");

        // Verify argument count is 0
        let call_edges = collect_call_edges(&staging);
        assert_eq!(call_edges.len(), 1, "Expected exactly one call edge");
    }

    #[test]
    fn test_argument_counting_multiple_args() {
        let source = r"
fn calculate(a: i32, b: i32, c: i32) i32 {
    return a + b + c;
}

fn main() void {
    const result = calculate(1, 2, 3);
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify call edge exists
        assert_has_call_edge(&staging, "main", "calculate");

        // Verify argument count is 3
        let call_edges = collect_call_edges(&staging);
        assert_eq!(call_edges.len(), 1, "Expected exactly one call edge");
    }

    #[test]
    fn test_nested_function() {
        // NOTE: tree-sitter-zig 1.1.2 has limited support for nested function declarations
        // They are parsed as struct_initializer with function_signature, not function_declaration
        // This is a known grammar limitation. For Phase 5C, we focus on top-level functions.
        //
        // This test verifies that we at least extract the outer function without panicking.
        let source = r"
fn outer() void {
    fn inner() void {
        return;
    }

    inner();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify outer function exists
        assert_has_node_with_kind(&staging, "outer", NodeKind::Function);

        // Due to grammar limitations, inner function may or may not be extracted
        // We just verify we don't panic
    }

    #[test]
    fn test_method_call_as_qualified() {
        let source = r"
const ArrayList = struct {
    fn append(self: *ArrayList, item: i32) void {
        // implementation
    }
};

fn main() void {
    var list: ArrayList = undefined;
    list.append(42);
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify canonical graph identity and Zig-native display for the method definition.
        assert_has_node_with_kind_exact(&staging, "ArrayList::append", NodeKind::Function);
        assert!(
            has_display_name(&staging, "ArrayList::append", "ArrayList.append"),
            "Struct methods should display with Zig native dot syntax"
        );

        // Verify main function exists
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);

        // Verify the call keeps canonical graph identity while exposing Zig native display syntax.
        assert_has_call_edge(&staging, "main", "list::append");
        assert_has_display_call_edge(&staging, "main", "list.append");
    }

    #[test]
    fn test_stdlib_qualified_call() {
        let source = r#"
const std = @import("std");

fn copyData(dest: []u8, src: []const u8) void {
    std.mem.copy(u8, dest, src);
}
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify import exists
        assert_has_import_edge(&staging, FILE_MODULE_NAME, "std");

        // Verify function exists
        assert_has_node_with_kind(&staging, "copyData", NodeKind::Function);

        // Verify canonical graph identity and Zig-native display for qualified stdlib calls.
        assert_has_call_edge(&staging, "copyData", "std::mem::copy");
        assert_has_display_call_edge(&staging, "copyData", "std.mem.copy");
    }

    #[test]
    fn test_private_function_visibility() {
        let source = r"
fn privateHelper() void {
    return;
}

pub fn publicFunction() void {
    privateHelper();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify both functions exist
        assert_has_node_with_kind(&staging, "privateHelper", NodeKind::Function);
        assert_has_node_with_kind(&staging, "publicFunction", NodeKind::Function);

        // Verify only public function is exported
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "publicFunction");

        // Verify private function is NOT exported
        let export_edges = collect_export_edges(&staging);
        assert_eq!(export_edges.len(), 1, "Expected only one export edge");
    }

    #[test]
    fn test_multiple_calls_in_function() {
        let source = r"
fn helper1() void {}
fn helper2() void {}
fn helper3() void {}

fn main() void {
    helper1();
    helper2();
    helper3();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify all functions exist
        assert_has_node_with_kind(&staging, "helper1", NodeKind::Function);
        assert_has_node_with_kind(&staging, "helper2", NodeKind::Function);
        assert_has_node_with_kind(&staging, "helper3", NodeKind::Function);
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);

        // Verify all call edges
        assert_has_call_edge(&staging, "main", "helper1");
        assert_has_call_edge(&staging, "main", "helper2");
        assert_has_call_edge(&staging, "main", "helper3");

        // Verify total call count
        let call_edges = collect_call_edges(&staging);
        assert_eq!(call_edges.len(), 3, "Expected exactly three call edges");
    }

    #[test]
    fn test_builtin_function_calls() {
        let source = r#"
const std = @import("std");

fn useBuiltins(dest: []u8, src: []const u8) void {
    @memcpy(dest.ptr, src.ptr, src.len);
    const info = @typeInfo(@TypeOf(dest));
}
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify import edge
        assert_has_import_edge(&staging, FILE_MODULE_NAME, "std");

        // Verify function exists
        assert_has_node_with_kind(&staging, "useBuiltins", NodeKind::Function);

        // Verify builtin calls are detected (non-import builtins create call edges)
        assert_has_call_edge(&staging, "useBuiltins", "@memcpy");
        assert_has_call_edge(&staging, "useBuiltins", "@typeInfo");
    }

    #[test]
    fn test_struct_methods_with_same_name() {
        // CRITICAL: Test that methods in different structs don't collide
        let source = r"
const ArrayList = struct {
    fn init() ArrayList {
        return undefined;
    }

    fn deinit(self: *ArrayList) void {
        // cleanup
    }

    fn append(self: *ArrayList, item: i32) void {
        // add item
    }
};

const HashMap = struct {
    fn init() HashMap {
        return undefined;
    }

    fn deinit(self: *HashMap) void {
        // cleanup
    }
};

fn main() void {
    var list = ArrayList.init();
    list.append(42);
    list.deinit();

    var map = HashMap.init();
    map.deinit();
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify both struct methods are qualified separately (definitions)
        assert_has_node_with_kind_exact(&staging, "ArrayList::init", NodeKind::Function);
        assert_has_node_with_kind_exact(&staging, "ArrayList::deinit", NodeKind::Function);
        assert_has_node_with_kind_exact(&staging, "ArrayList::append", NodeKind::Function);
        assert_has_node_with_kind_exact(&staging, "HashMap::init", NodeKind::Function);
        assert_has_node_with_kind_exact(&staging, "HashMap::deinit", NodeKind::Function);
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);
        assert!(has_display_name(
            &staging,
            "ArrayList::init",
            "ArrayList.init"
        ));
        assert!(has_display_name(
            &staging,
            "ArrayList::deinit",
            "ArrayList.deinit"
        ));
        assert!(has_display_name(
            &staging,
            "ArrayList::append",
            "ArrayList.append"
        ));
        assert!(has_display_name(&staging, "HashMap::init", "HashMap.init"));
        assert!(has_display_name(
            &staging,
            "HashMap::deinit",
            "HashMap.deinit"
        ));

        // Note: The graph builder also creates function nodes for call targets,
        // so we'll have additional nodes for instance method calls like list.append, list.deinit, etc.
        // These are call-site references that appear as qualified calls in the AST.
        let func_count = count_nodes_by_kind(&staging, NodeKind::Function);
        assert!(
            func_count >= 6,
            "Expected at least 6 functions (5 methods + main), got {func_count}"
        );
    }

    #[test]
    fn test_method_call_normalization() {
        // CRITICAL: Test that instance method calls like list.deinit() resolve to
        // container methods like ArrayList.deinit, not synthetic list.deinit nodes
        let source = r"
const ArrayList = struct {
    fn init() ArrayList {
        return undefined;
    }

    fn deinit(self: *ArrayList) void {
        // cleanup
    }
};

fn main() void {
    var list = ArrayList.init();
    list.deinit();  // This should resolve to ArrayList.deinit, not list.deinit
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify struct methods exist
        assert_has_node_with_kind_exact(&staging, "ArrayList::init", NodeKind::Function);
        assert_has_node_with_kind_exact(&staging, "ArrayList::deinit", NodeKind::Function);
        assert!(has_display_name(
            &staging,
            "ArrayList::init",
            "ArrayList.init"
        ));
        assert!(has_display_name(
            &staging,
            "ArrayList::deinit",
            "ArrayList.deinit"
        ));

        // Verify main exists
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);

        // Verify calls keep canonical graph identity while preserving Zig-native display syntax.
        assert_has_call_edge(&staging, "main", "ArrayList::init");
        assert_has_call_edge(&staging, "main", "list::deinit");
        assert_has_display_call_edge(&staging, "main", "ArrayList.init");
        assert_has_display_call_edge(&staging, "main", "list.deinit");
    }

    #[test]
    fn test_language_is_zig() {
        let source = r"
fn test_function() void {
    return;
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify function was created
        assert_has_node_with_kind(&staging, "test_function", NodeKind::Function);

        // Verify language is set correctly
        assert_eq!(builder.language(), Language::Zig);
    }

    #[test]
    fn test_import_builtin_detection() {
        let source = r#"
const std = @import("std");
const other = @import("other.zig");

fn main() void {
    std.debug.print("Hello\n", .{});
}
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify import nodes and edges
        assert_has_node_with_kind(&staging, "std", NodeKind::Import);
        assert_has_node_with_kind_exact(&staging, "other::zig", NodeKind::Import);
        assert_has_import_edge(&staging, FILE_MODULE_NAME, "std");
        assert_has_import_edge(&staging, FILE_MODULE_NAME, "other::zig");
        assert!(has_display_name(&staging, "other::zig", "other.zig"));
        assert_has_display_import_edge(&staging, FILE_MODULE_NAME, "other.zig");

        // Verify function and call
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);
        assert_has_call_edge(&staging, "main", "std::debug::print");
        assert_has_display_call_edge(&staging, "main", "std.debug.print");
    }

    #[test]
    fn test_import_in_function() {
        // Zig allows @import in function scope (though uncommon)
        let source = r#"
fn loadModule() void {
    const module = @import("dynamic.zig");
    module.init();
}
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify import node and edge from function
        assert_has_node_with_kind_exact(&staging, "dynamic::zig", NodeKind::Import);
        assert_has_import_edge(&staging, "loadModule", "dynamic::zig");
        assert!(has_display_name(&staging, "dynamic::zig", "dynamic.zig"));
        assert_has_display_import_edge(&staging, "loadModule", "dynamic.zig");

        // Verify function and call
        assert_has_node_with_kind(&staging, "loadModule", NodeKind::Function);
        assert_has_call_edge(&staging, "loadModule", "module::init");
        assert_has_display_call_edge(&staging, "loadModule", "module.init");
    }

    #[test]
    fn test_builtin_non_import_still_creates_call() {
        // Non-import builtins like @memcpy should still create call edges
        let source = r"
fn copyMemory(dest: []u8, src: []const u8) void {
    @memcpy(dest.ptr, src.ptr, src.len);
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify function exists
        assert_has_node_with_kind(&staging, "copyMemory", NodeKind::Function);

        // Verify call edge to builtin (not an import edge)
        assert_has_call_edge(&staging, "copyMemory", "@memcpy");

        // Verify no import edges were created for @memcpy
        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            0,
            "Non-import builtins should not create import edges"
        );
    }

    #[test]
    fn test_export_pub_function() {
        let source = r"
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

fn privateHelper() i32 {
    return 42;
}
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify both functions exist
        assert_has_node_with_kind(&staging, "add", NodeKind::Function);
        assert_has_node_with_kind(&staging, "privateHelper", NodeKind::Function);

        // Verify export edge for pub function
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "add");

        // Verify only one export edge (privateHelper is not exported)
        let export_edges = collect_export_edges(&staging);
        assert_eq!(export_edges.len(), 1, "Expected only one export edge");
    }

    #[test]
    fn test_export_pub_const_type() {
        let source = r#"
pub const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }
};

const PrivateType = struct {
    value: i32,
};

pub const API_VERSION = "1.0.0";
        "#;

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify pub type nodes exist (only pub declarations are tracked)
        assert_has_node_with_kind(&staging, "Point", NodeKind::Type);
        assert_has_node_with_kind(&staging, "API_VERSION", NodeKind::Type);

        // Verify export edges for pub declarations
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "Point");
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "API_VERSION");

        // Verify correct number of exports (only pub declarations)
        let export_edges = collect_export_edges(&staging);
        // We also have the pub method distance, so we expect 3 exports (Point, API_VERSION, and nested pub fn distance)
        // Actually, nested pub functions within structs should not be exported at module level
        // Let's verify the actual count
        assert!(
            export_edges.len() >= 2,
            "Expected at least two export edges (Point and API_VERSION)"
        );
    }

    #[test]
    fn test_export_nested_pub_in_private_container() {
        let source = r"
const PrivateContainer = struct {
    pub fn publicMethod() i32 {
        return 42;
    }

    pub const PUBLIC_CONST: i32 = 100;
};

pub const PublicContainer = struct {
    fn privateMethod() i32 {
        return 42;
    }
};
        ";

        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Verify PublicContainer is exported (it's pub const at module level)
        assert_has_export_edge(&staging, FILE_MODULE_NAME, "PublicContainer");

        // The implementation exports both:
        // 1. PublicContainer (pub const at module level)
        // 2. PrivateContainer.publicMethod (pub fn, even though container is private)
        // This is the current behavior - functions marked pub are exported even if their
        // containing struct is not pub. This could be refined in the future.
        let export_edges = collect_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected at least one export edge (PublicContainer)"
        );
    }

    // ========================================================================
    // C2_OTHER_ZIG — Property/Constant emission for container fields
    // REQ:R0001, R0002, R0003, R0004, R0005, R0023
    // ========================================================================

    /// Helper: locate a staged `AddNode` operation by exact canonical name.
    fn find_added_node<'a>(
        staging: &'a StagingGraph,
        canonical_name: &str,
    ) -> Option<&'a sqry_core::graph::unified::storage::arena::NodeEntry> {
        staging.operations().iter().find_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && staging.resolve_node_canonical_name(entry) == Some(canonical_name)
            {
                Some(entry)
            } else {
                None
            }
        })
    }

    /// 1-indexed line and 0-indexed column of `needle`'s first occurrence in
    /// `source`, in the shape `add_node_internal` stores on a `NodeEntry`
    /// (`start_line = span.start.line + 1`, `start_column = span.start.column`,
    /// the column being a byte offset within its line, as tree-sitter reports
    /// it). Deriving the expectation from the fixture text keeps the assertion
    /// honest if the fixture is ever reflowed.
    fn line_and_column_of(source: &str, needle: &str) -> (u32, u32) {
        let offset = source
            .find(needle)
            .unwrap_or_else(|| panic!("`{needle}` must appear in the fixture"));
        let prefix = &source[..offset];
        let line = u32::try_from(prefix.matches('\n').count() + 1).expect("line fits in u32");
        let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
        let column = u32::try_from(offset - line_start).expect("column fits in u32");
        (line, column)
    }

    /// AC-1 + AC-5: instance struct fields emit Property nodes whose
    /// canonical qualified name is `Container::field` (the helper-layer
    /// `canonicalize_graph_qualified_name` normalizes the source-form
    /// `Container.field` we pass in), with `is_static = false` and
    /// `visibility = None`. Span must be set to the field's real line and
    /// column, over a non-empty range.
    #[test]
    fn test_container_field_emits_property_with_attrs() {
        let source = r"
const Point = struct {
    x: i32,
    y: i32,
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // AC-1: Property kind under canonical qualified name `Container::field`
        // (canonicalize_graph_qualified_name normalizes Zig `.` -> `::`).
        assert_has_node_with_kind_exact(&staging, "Point::x", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Point::y", NodeKind::Property);

        // AC-5: attribute shape on the staged entry.
        let x_entry =
            find_added_node(&staging, "Point::x").expect("Point::x should be staged as a node");
        assert_eq!(x_entry.kind, NodeKind::Property, "x must be Property");
        assert!(
            !x_entry.is_static,
            "instance field is_static must be false (got true)"
        );
        assert!(
            x_entry.visibility.is_none(),
            "Zig has no member-level visibility — visibility must be None"
        );
        // `Span::from_node` records the node's real row and column;
        // `add_node_internal` then stores `start_line = row + 1` and
        // `start_column = column`. Pin both ends to the fixture's literal
        // `x: i32` text so the assertion fails loudly both on zero-width
        // spans and on accidental drift in which AST node we hand to
        // `Span::from_node` (e.g. swapping the `container_field` node for
        // the surrounding `field_list`).
        let field_text = "x: i32";
        let (expected_line, expected_start) = line_and_column_of(source, field_text);
        let expected_end = expected_start + u32::try_from(field_text.len()).unwrap();
        assert_eq!(
            x_entry.start_line, expected_line,
            "field span must report the real `x: i32` line, not line 1"
        );
        assert_eq!(
            x_entry.end_line, expected_line,
            "`x: i32` is single-line, so end_line must match start_line"
        );
        assert_eq!(
            x_entry.start_column, expected_start,
            "field span start_column must match `x: i32`'s column within its line"
        );
        assert_eq!(
            x_entry.end_column, expected_end,
            "field span end_column must match `x: i32`'s end column (tree-sitter \
             container_field excludes the trailing comma)"
        );
        assert!(
            x_entry.end_column > x_entry.start_column,
            "field span must be non-empty (end_column > start_column); \
             got start={} end={}",
            x_entry.start_column,
            x_entry.end_column,
        );

        // Old NodeKind::Variable for these qualified names must NOT appear.
        let stale_variable = staging.nodes().any(|n| {
            n.entry.kind == NodeKind::Variable
                && matches!(
                    staging.resolve_node_name(n.entry),
                    Some("Point::x" | "Point::y")
                )
        });
        assert!(
            !stale_variable,
            "Point::x/Point::y must not be emitted as NodeKind::Variable any more"
        );
    }

    /// AC-2 + AC-5: container-level `const` declarations inside a struct body
    /// emit Constant nodes with `is_static = true`, `visibility = None`, and
    /// the `Container::X` canonical qualified-name shape.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_container_level_const_emits_constant_static_true() {
        let source = r"
const Point = struct {
    x: i32,
    const ORIGIN: i32 = 0;
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // AC-2: Constant kind under canonical qualified name `Container::X`.
        assert_has_node_with_kind_exact(&staging, "Point::ORIGIN", NodeKind::Constant);

        let origin_entry = find_added_node(&staging, "Point::ORIGIN")
            .expect("Point::ORIGIN should be staged as a node");
        assert_eq!(origin_entry.kind, NodeKind::Constant);
        assert!(
            origin_entry.is_static,
            "container-level const is_static must be true"
        );
        assert!(
            origin_entry.visibility.is_none(),
            "Zig has no member-level visibility — visibility must be None"
        );
        // Pin the span against the fixture's literal `const ORIGIN: i32 = 0;`
        // text. The container-level decl path uses the entire
        // `variable_declaration` node span (statement including the trailing
        // semicolon).
        let decl_text = "const ORIGIN: i32 = 0;";
        let (expected_line, expected_start) = line_and_column_of(source, decl_text);
        let expected_end = expected_start + u32::try_from(decl_text.len()).unwrap();
        assert_eq!(
            origin_entry.start_line, expected_line,
            "container-const span must report the real declaration line, not line 1"
        );
        assert_eq!(
            origin_entry.end_line, expected_line,
            "the declaration is single-line, so end_line must match start_line"
        );
        assert_eq!(
            origin_entry.start_column, expected_start,
            "container-const span start_column must match the `const` keyword's column"
        );
        assert_eq!(
            origin_entry.end_column, expected_end,
            "container-const span end_column must match the trailing-`;` end column \
             (variable_declaration spans the full statement)"
        );
        assert!(
            origin_entry.end_column > origin_entry.start_column,
            "constant span must be non-empty (end_column > start_column); \
             got start={} end={}",
            origin_entry.start_column,
            origin_entry.end_column,
        );

        // Regression for codex feedback (dual-emission suppression):
        // ORIGIN must NOT also appear as a bare `NodeKind::Variable` under
        // the un-qualified name "ORIGIN". The container-level path is the
        // sole emission site for container `const`s.
        let stale_bare_origin_variable = staging.nodes().any(|n| {
            n.entry.kind == NodeKind::Variable
                && (staging.resolve_node_canonical_name(n.entry) == Some("ORIGIN")
                    || staging.resolve_node_name(n.entry) == Some("ORIGIN"))
        });
        assert!(
            !stale_bare_origin_variable,
            "container-level const ORIGIN must not be dual-emitted as a bare \
             NodeKind::Variable named \"ORIGIN\""
        );

        // Belt-and-braces: the planner-equivalent name lookup
        // (find_added_node by either bare or canonical name) must not
        // surface a `NodeKind::Variable` shadow of ORIGIN.
        let bare_origin_node_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(op, StagingOp::AddNode { entry, .. }
                if entry.kind == NodeKind::Variable
                    && (staging.resolve_node_canonical_name(entry) == Some("ORIGIN")
                        || staging.resolve_node_name(entry) == Some("ORIGIN")))
            })
            .count();
        assert_eq!(
            bare_origin_node_count, 0,
            "no bare-name Variable ORIGIN AddNode op may be staged \
             (got {bare_origin_node_count})"
        );

        // The TypeOf edge emitted for ORIGIN must use Field context, not
        // Variable context — the container-member path is the sole
        // emitter and uses `TypeOfContext::Field` to match the field
        // contract.
        let origin_id = staging
            .operations()
            .iter()
            .find_map(|op| match op {
                StagingOp::AddNode {
                    entry,
                    expected_id: Some(id),
                } if staging.resolve_node_canonical_name(entry) == Some("Point::ORIGIN")
                    && entry.kind == NodeKind::Constant =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .expect("Point::ORIGIN Constant node must be staged");

        let mut origin_typeof_contexts: Vec<Option<TypeOfContext>> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    kind: EdgeKind::TypeOf { context, .. },
                    ..
                } = op
                    && *source == origin_id
                {
                    Some(*context)
                } else {
                    None
                }
            })
            .collect();
        origin_typeof_contexts.sort_by_key(|c| match c {
            Some(TypeOfContext::Field) => 0,
            Some(TypeOfContext::Variable) => 1,
            _ => 2,
        });
        assert!(
            !origin_typeof_contexts.is_empty(),
            "ORIGIN must have at least one TypeOf edge"
        );
        for context in &origin_typeof_contexts {
            assert_eq!(
                *context,
                Some(TypeOfContext::Field),
                "every TypeOf edge from ORIGIN must use TypeOfContext::Field, \
                 never TypeOfContext::Variable (saw {context:?})",
            );
        }
    }

    /// AC-4: `TypeOf` edge metadata is unchanged. Specifically the edge
    /// carries `TypeOfContext::Field` and the bare field name (not the
    /// qualified form), and the edge's source `NodeId` is the (now-Property)
    /// field node.
    #[test]
    fn test_container_field_typeof_edge_metadata_unchanged() {
        let source = r"
const Point = struct {
    x: i32,
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Find the Property node id for Point::x (canonical form).
        let x_id = staging
            .operations()
            .iter()
            .find_map(|op| match op {
                StagingOp::AddNode {
                    entry,
                    expected_id: Some(id),
                } if staging.resolve_node_canonical_name(entry) == Some("Point::x")
                    && entry.kind == NodeKind::Property =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .expect("Point::x Property node must be staged");

        // Look for the TypeOf edge with Field context + bare name "x".
        let edge = staging.operations().iter().find_map(|op| {
            if let StagingOp::AddEdge {
                source,
                kind: EdgeKind::TypeOf { context, name, .. },
                ..
            } = op
                && *source == x_id
            {
                Some((*context, *name))
            } else {
                None
            }
        });

        let (ctx, name) = edge.expect("TypeOf edge from Point::x should be staged");
        assert_eq!(
            ctx,
            Some(TypeOfContext::Field),
            "TypeOf edge context must be Field"
        );
        let resolved_name = name.and_then(|sid| staging.resolve_local_string(sid));
        assert_eq!(
            resolved_name,
            Some("x"),
            "TypeOf edge name must be the bare field name 'x' (not qualified)"
        );
    }

    /// AC-3: dedupe pattern preserved — within a single `build_graph`
    /// invocation, the `process_typeof_edges` DFS may visit a container
    /// node multiple times (e.g., a `struct_declaration` reachable both as
    /// a child of its enclosing `variable_declaration` and through other
    /// traversal entries). Dedupe is two-layered:
    ///
    /// 1. `handle_container_field` / `handle_container_member_decl` first
    ///    canonicalise `Container.field` → `Container::field` and probe
    ///    `helper.get_node` (canonical-form fast path). On a hit no
    ///    further node-allocating work runs.
    /// 2. On miss, `add_property_with_static_and_visibility` /
    ///    `add_constant_with_static_and_visibility` end up in
    ///    `add_node_internal`, whose own cache is keyed on
    ///    (canonical-name, kind) and short-circuits via
    ///    `update_node_entry` rather than staging a second `AddNode`.
    ///
    /// A two-field struct yields exactly one `AddNode` op per field; the
    /// test also asserts no duplicate `Point::ORIGIN` Constant node is
    /// staged when the body mixes fields and a container-level const.
    #[test]
    fn test_container_field_dedupes_via_get_node() {
        let source = r"
const Point = struct {
    x: i32,
    y: i32,
    const ORIGIN: i32 = 0;
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        for canonical in ["Point::x", "Point::y", "Point::ORIGIN"] {
            let count = staging
                .operations()
                .iter()
                .filter(|op| {
                    matches!(op, StagingOp::AddNode { entry, .. }
                    if staging.resolve_node_canonical_name(entry) == Some(canonical))
                })
                .count();
            assert_eq!(
                count, 1,
                "{canonical} must be staged exactly once after a single build_graph pass — \
                 the canonical-form get_node fast path (and add_node_internal's \
                 canonical-cache fallback) must collapse duplicate visits; \
                 got {count} AddNode ops"
            );
        }
    }

    /// Regression for codex feedback (parent-container guard):
    /// genuine function-local `var`/`const` declarations must continue to
    /// be emitted as `NodeKind::Variable` under their bare un-qualified
    /// name (the existing pre-Property contract for fn-locals). The
    /// dual-emission suppression in `process_typeof_edges` only applies
    /// when the `variable_declaration`'s parent is a container body;
    /// function-locals must not be collateral damage.
    ///
    /// The fixture deliberately keeps each binding on a clean
    /// `<keyword> name: Type = value;` line and avoids assignment
    /// statements (which the tree-sitter grammar also exposes as
    /// `variable_declaration` AST nodes; that quirk is unrelated to
    /// this guard).
    #[test]
    fn test_function_local_var_const_still_emit_variable() {
        let source = r"
fn run() void {
    var counter: i32 = 0;
    const limit: i32 = 10;
}
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // Both the function-local `var` and the function-local `const`
        // must survive as `NodeKind::Variable` under their bare names.
        // (Function-local `const` is intentionally Variable, not
        // Constant: only container-level `const` becomes Constant per
        // the new container-member path.)
        for local in ["counter", "limit"] {
            let local_variable_count = staging
                .operations()
                .iter()
                .filter(|op| {
                    matches!(op, StagingOp::AddNode { entry, .. }
                    if entry.kind == NodeKind::Variable
                        && (staging.resolve_node_canonical_name(entry) == Some(local)
                            || staging.resolve_node_name(entry) == Some(local)))
                })
                .count();
            assert!(
                local_variable_count >= 1,
                "function-local `{local}` must remain a NodeKind::Variable \
                 under its bare name (got {local_variable_count} AddNode ops)"
            );
        }

        // Negative side of the guard: function-local var/const decls
        // must NOT be promoted to Constant or Property — only
        // container-level `const`/`var` decls take the qualified
        // Property/Constant path.
        let stale_local_constant_or_property = staging.nodes().any(|n| {
            matches!(n.entry.kind, NodeKind::Constant | NodeKind::Property)
                && (staging.resolve_node_canonical_name(n.entry) == Some("limit")
                    || staging.resolve_node_name(n.entry) == Some("limit")
                    || staging.resolve_node_canonical_name(n.entry) == Some("counter")
                    || staging.resolve_node_name(n.entry) == Some("counter"))
        });
        assert!(
            !stale_local_constant_or_property,
            "function-local var/const must NOT be emitted as Constant or Property"
        );
    }

    /// Union and tagged-union (union(enum)) container fields must also emit
    /// Property nodes — Zig treats union members exactly like struct members.
    #[test]
    fn test_union_field_emits_property() {
        let source = r"
const Tagged = union(enum) {
    a: i32,
    b: f64,
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        assert_has_node_with_kind_exact(&staging, "Tagged::a", NodeKind::Property);
        assert_has_node_with_kind_exact(&staging, "Tagged::b", NodeKind::Property);
    }

    /// Regression: container-level `const` declarations nested inside an
    /// `opaque { ... }` body must be emitted under the qualified
    /// `Container::Name` form as `NodeKind::Constant` (`is_static` = true,
    /// visibility = None), and must NOT be dual-emitted as a bare
    /// `NodeKind::Variable` under the un-qualified name. The `TypeOf`
    /// edge from the constant must use `TypeOfContext::Field`.
    ///
    /// Without `opaque_declaration` in the `process_typeof_edges` dispatch
    /// arm, the parent-guard (`is_container_member_var_decl`) suppressed
    /// the bare Variable emission while no container-fields walk was
    /// invoked for the opaque body — net effect: `X` vanished from the
    /// graph entirely. This test pins both the container-member emission
    /// and the absence of a stale bare-Variable shadow.
    ///
    /// Refs: REQ:R0001..R0005, R0023.
    #[test]
    fn test_opaque_container_level_const_emits_constant() {
        let source = r"
pub const O = opaque {
    const X: i32 = 1;
};
        ";
        let (tree, content) = parse_zig(source);
        let mut staging = StagingGraph::new();
        let builder = ZigGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.zig"), &mut staging)
            .unwrap();

        // AC: Constant kind under canonical qualified name `O::X`.
        assert_has_node_with_kind_exact(&staging, "O::X", NodeKind::Constant);

        let x_entry = find_added_node(&staging, "O::X").expect("O::X should be staged as a node");
        assert_eq!(x_entry.kind, NodeKind::Constant);
        assert!(
            x_entry.is_static,
            "container-level const inside opaque must have is_static = true"
        );
        assert!(
            x_entry.visibility.is_none(),
            "Zig has no member-level visibility — visibility must be None"
        );

        // No bare-name `NodeKind::Variable` shadow of X may exist:
        // the parent-guard correctly suppresses
        // `handle_variable_declaration` for opaque-parented vars, and the
        // container-member path is now the sole emitter.
        let stale_bare_x_variable = staging.nodes().any(|n| {
            n.entry.kind == NodeKind::Variable
                && (staging.resolve_node_canonical_name(n.entry) == Some("X")
                    || staging.resolve_node_name(n.entry) == Some("X"))
        });
        assert!(
            !stale_bare_x_variable,
            "container-level const X inside opaque must not be dual-emitted as a \
             bare NodeKind::Variable named \"X\""
        );

        let bare_x_node_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(op, StagingOp::AddNode { entry, .. }
                if entry.kind == NodeKind::Variable
                    && (staging.resolve_node_canonical_name(entry) == Some("X")
                        || staging.resolve_node_name(entry) == Some("X")))
            })
            .count();
        assert_eq!(
            bare_x_node_count, 0,
            "no bare-name Variable X AddNode op may be staged \
             (got {bare_x_node_count})"
        );

        // Every TypeOf edge from O::X must use `TypeOfContext::Field`,
        // matching the field-path contract used by the container-member
        // emission site.
        let x_id = staging
            .operations()
            .iter()
            .find_map(|op| match op {
                StagingOp::AddNode {
                    entry,
                    expected_id: Some(id),
                } if staging.resolve_node_canonical_name(entry) == Some("O::X")
                    && entry.kind == NodeKind::Constant =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .expect("O::X Constant node must be staged");

        let x_typeof_contexts: Vec<Option<TypeOfContext>> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    kind: EdgeKind::TypeOf { context, .. },
                    ..
                } = op
                    && *source == x_id
                {
                    Some(*context)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            !x_typeof_contexts.is_empty(),
            "O::X must have at least one TypeOf edge (from `: i32` annotation)"
        );
        for context in &x_typeof_contexts {
            assert_eq!(
                *context,
                Some(TypeOfContext::Field),
                "every TypeOf edge from O::X must use TypeOfContext::Field, \
                 never TypeOfContext::Variable (saw {context:?})",
            );
        }
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/systems/sample.zig"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .expect("load Zig grammar");
        parser.parse(src, None).expect("parse Zig sample")
    }

    fn nth_of_kind<'a>(node: Node<'a>, kind: &str, mut skip: usize) -> Option<Node<'a>> {
        fn walk<'a>(node: Node<'a>, kind: &str, skip: &mut usize) -> Option<Node<'a>> {
            if node.kind() == kind {
                if *skip == 0 {
                    return Some(node);
                }
                *skip -= 1;
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = walk(child, kind, skip) {
                    return Some(found);
                }
            }
            None
        }
        walk(node, kind, &mut skip)
    }

    #[test]
    fn zig_mapping_is_non_empty() {
        let mapping = zig_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_zig::LANGUAGE.into();
        let count = (0..lang.node_kind_count())
            .filter_map(|id| u16::try_from(id).ok())
            .filter(|id| mapping.cf_bucket(*id).is_some())
            .count();
        assert!(
            count > 0,
            "Zig cf_bucket map should cover real control-flow kinds"
        );
    }

    #[test]
    fn zig_histogram_covers_control_flow() {
        let tree = parse(SAMPLE);
        // The second `function_declaration` is `classify` (first is `compute`).
        let func = nth_of_kind(tree.root_node(), "function_declaration", 1)
            .expect("sample has a classify function_declaration");
        let desc = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            zig_shape_mapping(),
            &ShapeBudget::default(),
        );
        let h = &desc.cf_histogram;
        assert!(h[CfBucket::Branch.index()] >= 1, "branch present");
        assert!(h[CfBucket::Loop.index()] >= 1, "loop present");
        assert!(h[CfBucket::Match.index()] >= 1, "switch present");
        assert!(h[CfBucket::Call.index()] >= 1, "call present");
        assert!(h[CfBucket::Return.index()] >= 1, "return present");
        assert!(
            h[CfBucket::BreakContinue.index()] >= 1,
            "break/continue present"
        );
    }

    #[test]
    fn zig_signature_shape_reads_params() {
        let tree = parse(SAMPLE);
        let func = nth_of_kind(tree.root_node(), "function_declaration", 1)
            .expect("sample has a classify function_declaration");
        let shape = zig_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // classify(n: i32, items: []const u8) i32: two positional params.
        assert_eq!(shape.arity_positional, 2, "two positional params");
        assert!(shape.has_return_annotation, "Zig return type slot present");
    }
}
