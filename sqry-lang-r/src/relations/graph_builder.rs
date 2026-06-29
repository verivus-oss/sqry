//! `GraphBuilder` implementation for R
//!
//! Builds the unified `CodeGraph` for R files by:
//! 1. Extracting function definitions (assignments with `function_definition` RHS)
//! 2. Detecting S3 methods (name.class pattern)
//! 3. Detecting function call expressions
//! 4. Creating call edges between caller and callee
//!
//! ## Cross-Language Support
//! - C/C++ FFI: Detects `.Call()`, `.External()`, `Rcpp::*` as `FfiCall` edges (`FfiConvention::C`)
//! - Rcpp: Detects `Rcpp::` function calls as `FfiCall` edges
//!
//! ## Limitations
//! - Non-standard evaluation (NSE): Not tracked (runtime-only, e.g., dplyr verbs)
//! - Formula objects (`~`): Not tracked (domain-specific, not function calls)
//! - S4 class hierarchy: Not tracked (OOP tracking beyond scope)

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::GraphBuildHelper;
use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span};
use tree_sitter::{Node, Point, StreamingIterator, Tree};

/// Maximum nesting depth to prevent pathological cases
const DEFAULT_MAX_SCOPE_DEPTH: usize = 5;

/// R-specific graph builder
#[derive(Debug)]
pub struct RGraphBuilder {
    pub max_scope_depth: usize,
}

impl Default for RGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_MAX_SCOPE_DEPTH,
        }
    }
}

impl GraphBuilder for RGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::R);

        // Build AST graph to track function contexts
        let ast_graph = RASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Create function nodes for all contexts
        let mut context_to_node: HashMap<String, NodeId> = HashMap::new();
        for context in ast_graph.contexts() {
            let span = Some(span_from_points(
                context.start_position,
                context.end_position,
            ));
            let visibility = extract_visibility(&context.qualified_name);
            let node_id = helper.add_function_with_visibility(
                &context.qualified_name,
                span,
                false,
                false,
                Some(visibility),
            );
            context_to_node.insert(context.qualified_name.clone(), node_id);
        }

        // Create package module and export public functions
        // In R, derive package name from file path or use default package name
        let package_name = extract_package_name_from_path(file);
        let module_id = helper.add_module(&package_name, None);

        for context in ast_graph.contexts() {
            let visibility = extract_visibility(&context.qualified_name);
            if visibility == "public" {
                // Export all public (non-dot-prefixed) functions
                if let Some(&node_id) = context_to_node.get(&context.qualified_name) {
                    helper.add_export_edge(module_id, node_id);
                }
            }
        }

        // Walk the tree to find call expressions and build call edges
        visit_node_for_calls(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &context_to_node,
        );

        // Extract class definitions (S3, S4, R6)
        extract_class_definitions(tree.root_node(), content, &mut helper);

        // Extract variable assignments
        extract_variable_assignments(tree.root_node(), content, &mut helper);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::R
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(r_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for R (identifier-blind body-shape feature).
///
/// Precomputed `kind_id -> CfBucket` table built once from the tree-sitter-r
/// grammar. R expresses assignment through the `<-` / `=` binary operators rather
/// than a dedicated assignment node, and error handling flows through the
/// `tryCatch`/`stop` calls (which stay in the `Call` bucket), so the honest bucket
/// set is branch/loop/break-continue/call/closure.
pub struct RShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl RShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_r::LANGUAGE.into();
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
                *slot = cf_bucket_for_r_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for RShapeMapping {
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
                if child.kind() != "parameter" {
                    continue;
                }
                // The `...` dots parameter is variadic, not a positional slot.
                if child
                    .named_child(0)
                    .is_some_and(|inner| inner.kind() == "dots")
                {
                    shape.has_varargs = true;
                    continue;
                }
                shape.arity_positional = shape.arity_positional.saturating_add(1);
                if child.child_by_field_name("default").is_some() {
                    shape.has_defaults = true;
                }
            }
        }
        shape
    }
}

/// Map one tree-sitter-r node-kind name to its canonical control-flow bucket.
/// Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_r_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" => CfBucket::Branch,
        "for_statement" | "while_statement" | "repeat_statement" => CfBucket::Loop,
        "break" | "next" => CfBucket::BreakContinue,
        "call" => CfBucket::Call,
        // Nested anonymous function literals (e.g. the closures passed to
        // `sapply`/`vapply`/`tryCatch`).
        "function_definition" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide R shape mapping, built once on first use.
#[must_use]
pub fn r_shape_mapping() -> &'static RShapeMapping {
    static MAPPING: OnceLock<RShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(RShapeMapping::build)
}

// Helper functions for graph building

/// Visit nodes recursively to find call expressions and create call edges
fn visit_node_for_calls(
    node: Node<'_>,
    content: &[u8],
    ast_graph: &RASTGraph,
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
) {
    // Check if this node is a call expression
    if node.kind() == "call" {
        // Check if this is a library() or source() call
        if is_import_call(&node, content) {
            // Build import edge for library() or source()
            build_import_edge_for_r(node, content, helper);
        } else if let Some(context) = ast_graph.context_for_node(&node)
            && let Some(&caller_id) = context_to_node.get(&context.qualified_name)
            && let Some(function_node) = node.child_by_field_name("function")
            && let Ok(callee_text) = node_text(function_node, content)
            && !callee_text.is_empty()
        {
            // Check if this is an FFI call
            let is_ffi = is_ffi_call(&callee_text);

            // For FFI calls, we might need to extract the actual target
            let actual_callee = if is_ffi {
                extract_ffi_target_name(node, content).unwrap_or(callee_text.clone())
            } else {
                callee_text.clone()
            };

            // Create or get callee node
            let call_span = span_from_node(node);
            let callee_id =
                helper.ensure_callee(&actual_callee, call_span, CalleeKindHint::Function);

            // Create appropriate edge type
            if is_ffi {
                // FFI calls get FfiCall edges with C convention
                helper.add_ffi_edge(caller_id, callee_id, FfiConvention::C);
            } else {
                // Regular calls get Calls edges with span and argument count
                let argument_count = count_arguments(node, content);
                let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                helper.add_call_edge_full_with_span(
                    caller_id,
                    callee_id,
                    argument_count,
                    false,
                    vec![call_span],
                );
            }
        }
    }

    // Recursively visit children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node_for_calls(child, content, ast_graph, helper, context_to_node);
    }
}

/// Check if a call node is a `library()` or `source()` call
fn is_import_call(call_node: &Node, content: &[u8]) -> bool {
    if let Some(function_node) = call_node.child_by_field_name("function")
        && let Ok(text) = function_node.utf8_text(content)
    {
        return matches!(
            text.trim(),
            "library" | "require" | "source" | "loadNamespace"
        );
    }
    false
}

/// Build import edge from a `library()` or `source()` call
fn build_import_edge_for_r(call_node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Get the function name to determine import type
    let Some(function_node) = call_node.child_by_field_name("function") else {
        return;
    };
    let function_name = function_node.utf8_text(content).unwrap_or("").trim();

    // Get the arguments
    let Some(args_node) = call_node.child_by_field_name("arguments") else {
        return;
    };

    // Extract the first argument (package/file name)
    let mut cursor = args_node.walk();
    let mut import_target: Option<String> = None;

    for child in args_node.named_children(&mut cursor) {
        match child.kind() {
            // Bare identifier: library(dplyr)
            "identifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    import_target = Some(text.trim().to_string());
                    break;
                }
            }
            // String argument: library("dplyr") or source("file.R")
            "string" => {
                if let Ok(text) = child.utf8_text(content) {
                    let trimmed = text.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !trimmed.is_empty() {
                        import_target = Some(trimmed);
                        break;
                    }
                }
            }
            // Named argument: library(package = dplyr)
            "argument" => {
                if let Some(value) = child.child_by_field_name("value")
                    && let Ok(text) = value.utf8_text(content)
                {
                    let trimmed = text.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !trimmed.is_empty() {
                        import_target = Some(trimmed);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    // Create import edge if we found a target
    if let Some(imported) = import_target {
        let span = span_from_node(call_node);

        // Create module node (importer) and import node (imported)
        let module_id = helper.add_module("<module>", None);

        // Prefix with function type to distinguish semantic meaning
        let import_name = match function_name {
            "source" => format!("source:{imported}"),
            "loadNamespace" => format!("namespace:{imported}"),
            _ => imported.clone(), // library/require are standard imports
        };

        let import_id = helper.add_import(&import_name, Some(span));

        // R library()/require() imports all exports from the package (wildcard)
        // source() loads a specific file (not wildcard)
        // loadNamespace() imports the namespace (not wildcard, specific reference)
        let is_wildcard = matches!(function_name, "library" | "require");
        helper.add_import_edge_full(module_id, import_id, None, is_wildcard);
    }
}

/// Check if a function name is an FFI call
fn is_ffi_call(name: &str) -> bool {
    matches!(name, ".Call" | ".External") || name.starts_with("Rcpp::")
}

/// Extract the target function name from FFI calls like .`Call("fast_mean_c`", ...)
fn extract_ffi_target_name(call_node: Node<'_>, content: &[u8]) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();

    // Find the first string argument (the C function name)
    for child in args.named_children(&mut cursor) {
        if child.kind() == "argument"
            && let Some(value) = child.child_by_field_name("value")
            && value.kind() == "string"
            && let Ok(text) = value.utf8_text(content)
        {
            // Remove quotes from string literal
            let trimmed = text.trim_matches('"').trim_matches('\'');
            return Some(trimmed.to_string());
        } else if child.kind() == "string"
            && let Ok(text) = child.utf8_text(content)
        {
            // Direct string child (no argument wrapper)
            let trimmed = text.trim_matches('"').trim_matches('\'');
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Extract text from a tree-sitter node
fn node_text(node: Node<'_>, content: &[u8]) -> GraphResult<String> {
    node.utf8_text(content)
        .map(std::string::ToString::to_string)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "failed to extract node text".to_string(),
        })
}

/// Count function call arguments
#[allow(dead_code)] // Reserved for argument count analysis
fn count_arguments(call_node: Node<'_>, _content: &[u8]) -> usize {
    call_node
        .child_by_field_name("arguments")
        .map_or(0, |args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor)
                .filter(|child| child.kind() == "argument")
                .count()
        })
}

/// Check if a node kind is trivia (comments, whitespace, etc.)
#[allow(dead_code)] // Reserved for trivia filtering
fn is_trivia(kind: &str) -> bool {
    matches!(kind, "comment" | "string" | "ERROR")
}

/// Convert a tree-sitter node to a Span
fn span_from_node(node: Node<'_>) -> Span {
    Span::from_bytes(node.start_byte(), node.end_byte())
}

/// Convert tree-sitter Points to a Span
fn span_from_points(start: Point, end: Point) -> Span {
    Span::new(
        Position::new(start.row + 1, start.column),
        Position::new(end.row + 1, end.column),
    )
}

/// Extract class definitions from R code (S3, S4, R6)
///
/// R has multiple object systems:
/// - S3: `structure(list(...), class = "MyClass")`
/// - S4: `setClass("MyClass", ...)`
/// - R6: `R6Class("MyClass", ...)`
///
/// This is a best-effort approach - R's dynamic nature makes comprehensive detection difficult.
fn extract_class_definitions(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    if node.kind() == "call" {
        // Check for setClass (S4) or R6Class calls
        if let Some(function_node) = node.child_by_field_name("function")
            && let Ok(func_text) = function_node.utf8_text(content)
            && matches!(func_text, "setClass" | "R6Class")
        {
            // Extract the class name from the first argument
            // First, find the arguments node
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "arguments" {
                    // Look for the first string or first argument node
                    let mut arg_cursor = child.walk();
                    for arg_child in child.children(&mut arg_cursor) {
                        // Check for direct string child
                        if arg_child.kind() == "string"
                            && let Ok(text) = arg_child.utf8_text(content)
                        {
                            let class_name = text.trim().trim_matches('"').trim_matches('\'');
                            if !class_name.is_empty() {
                                let span = span_from_node(node);
                                let class_id = helper.add_class(class_name, Some(span));
                                // issue #394: real declaration; opt dual-use bare helper into is_definition
                                helper.mark_definition(class_id);
                            }
                            return; // Found the class name, done
                        }
                        // Check for argument wrapper
                        if arg_child.kind() == "argument" {
                            // Look for string inside argument node
                            let mut inner_cursor = arg_child.walk();
                            for inner_child in arg_child.children(&mut inner_cursor) {
                                if inner_child.kind() == "string"
                                    && let Ok(text) = inner_child.utf8_text(content)
                                {
                                    let class_name =
                                        text.trim().trim_matches('"').trim_matches('\'');
                                    if !class_name.is_empty() {
                                        let span = span_from_node(node);
                                        let class_id = helper.add_class(class_name, Some(span));
                                        // issue #394: real declaration; opt dual-use bare helper into is_definition
                                        helper.mark_definition(class_id);
                                    }
                                    return; // Found the class name, done
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_class_definitions(child, content, helper);
    }
}

/// Extract variable assignments from R code
///
/// R uses `<-` and `=` for assignment. This function extracts variable names
/// from assignment statements and creates variable nodes.
fn extract_variable_assignments(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    if node.kind() == "binary_operator" {
        // Check if this is an assignment operator
        if let Some(operator_node) = node.child_by_field_name("operator")
            && let Ok(op_text) = operator_node.utf8_text(content)
            && matches!(op_text, "<-" | "=" | "<<-" | "->>" | "->")
        {
            // Get the left-hand side (variable name)
            let lhs_node = if matches!(op_text, "->" | "->>") {
                // Right assignment: rhs -> lhs
                node.child_by_field_name("rhs")
            } else {
                // Left assignment: lhs <- rhs
                node.child_by_field_name("lhs")
            };

            if let Some(var_node) = lhs_node {
                // Extract variable name if it's a simple identifier
                if var_node.kind() == "identifier"
                    && let Ok(var_name) = var_node.utf8_text(content)
                {
                    let var_name = var_name.trim();
                    // Skip function assignments (handled separately)
                    // Check if RHS is a function_definition
                    let rhs = if matches!(op_text, "->" | "->>") {
                        node.child_by_field_name("lhs")
                    } else {
                        node.child_by_field_name("rhs")
                    };

                    let is_function_assignment =
                        rhs.is_some_and(|r| r.kind() == "function_definition");

                    if !is_function_assignment && !var_name.is_empty() {
                        let span = span_from_node(var_node);
                        let var_id = helper.add_variable(var_name, Some(span));
                        // issue #394: real declaration; opt dual-use bare helper into is_definition
                        helper.mark_definition(var_id);
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_variable_assignments(child, content, helper);
    }
}

// ============================================================================
// AST Graph - Tracks callable contexts
// ============================================================================

#[derive(Debug)]
struct RASTGraph {
    contexts: Vec<FunctionContext>,
    node_to_context: HashMap<usize, usize>,
}

impl RASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], _max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();

        // Extract function definitions using tree-sitter query
        // Pattern: name <- function(...) { ... }
        // R uses binary_operator for assignments
        let query = tree_sitter::Query::new(
            &tree_sitter_r::LANGUAGE.into(),
            r"
            (binary_operator
              lhs: (identifier) @function_name
              operator: _ @op
              rhs: (function_definition) @function_def
            ) @assignment
            ",
        )
        .map_err(|e| format!("Failed to create query: {e}"))?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let root = tree.root_node();
        let capture_names = query.capture_names();
        let mut matches = cursor.matches(&query, root, content);

        while let Some(m) = matches.next() {
            let mut function_name = None;
            let mut assignment_node = None;

            for capture in m.captures {
                let capture_name = capture_names[capture.index as usize];
                match capture_name {
                    "function_name" => function_name = Some(capture.node),
                    "assignment" => assignment_node = Some(capture.node),
                    _ => {}
                }
            }

            if let (Some(name_node), Some(assign_node)) = (function_name, assignment_node) {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|e| format!("Failed to extract function name: {e}"))?
                    .to_string();

                let context_idx = contexts.len();
                contexts.push(FunctionContext {
                    qualified_name: name,
                    start_position: assign_node.start_position(),
                    end_position: assign_node.end_position(),
                });

                // Map all descendant nodes to this context
                map_descendants_to_context(&assign_node, context_idx, &mut node_to_context);
            }
        }

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    fn contexts(&self) -> &[FunctionContext] {
        &self.contexts
    }

    fn context_for_node(&self, node: &Node) -> Option<&FunctionContext> {
        self.node_to_context
            .get(&node.id())
            .and_then(|idx| self.contexts.get(*idx))
    }
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
struct FunctionContext {
    qualified_name: String,
    start_position: Point,
    end_position: Point,
}

/// Extract visibility for an R function based on naming convention.
///
/// In R, the convention is:
/// - Functions starting with `.` are considered private
/// - All other functions are considered public
fn extract_visibility(name: &str) -> &'static str {
    let simple_name = name.split("::").last().unwrap_or(name);
    if simple_name.starts_with('.') {
        "private"
    } else {
        "public"
    }
}

/// Extract package name from file path
/// For R packages, tries to extract from R/ directory structure
/// Returns file stem as fallback or "main" if no better option
fn extract_package_name_from_path(file: &Path) -> String {
    // Try to get file stem (e.g., "utils.R" -> "utils")
    if let Some(stem) = file.file_stem()
        && let Some(name) = stem.to_str()
    {
        return name.to_string();
    }

    // Fallback to "main" package
    "main".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_call_edge, assert_has_ffi_call_edge, assert_has_node, assert_has_node_with_kind,
        count_nodes_by_kind,
    };
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::resolution::display_graph_qualified_name;

    /// Helper to extract Import edges from staging operations
    fn extract_import_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Imports { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    fn parse_r(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_r::LANGUAGE.into())
            .expect("Failed to load R grammar");

        let content = source.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    #[test]
    fn test_extract_function_definition() {
        let source = r"
            calculate_mean <- function(x) {
              sum(x) / length(x)
            }
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        assert_has_node_with_kind(&staging, "calculate_mean", NodeKind::Function);
    }

    #[test]
    fn test_extract_s3_method() {
        let source = r#"
            print.custom_class <- function(x, ...) {
              cat("Custom:", x$value, "\n")
            }
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        assert_has_node_with_kind(&staging, "print::custom_class", NodeKind::Function);
        assert_eq!(
            display_graph_qualified_name(
                Language::R,
                "print::custom_class",
                NodeKind::Function,
                false,
            ),
            "print.custom_class"
        );
    }

    #[test]
    fn test_extract_multiple_functions() {
        let source = r"
            func_a <- function(x) { x }
            func_b <- function(y) { y }
            func_c <- function(z) { z }
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        assert_has_node(&staging, "func_a");
        assert_has_node(&staging, "func_b");
        assert_has_node(&staging, "func_c");
        assert_eq!(count_nodes_by_kind(&staging, NodeKind::Function), 3);
    }

    #[test]
    fn test_extract_simple_call() {
        let source = r"
            calculate_stats <- function(x) {
              mean_val <- mean(x)
              sd_val <- sd(x)
              list(mean = mean_val, sd = sd_val)
            }
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "calculate_stats", "mean");
        assert_has_call_edge(&staging, "calculate_stats", "sd");
        assert_has_call_edge(&staging, "calculate_stats", "list");
    }

    #[test]
    fn test_extract_c_ffi_call() {
        let source = r#"
            fast_mean <- function(x) {
              .Call("fast_mean_c", x, PACKAGE = "mypackage")
            }
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        // The R graph builder extracts the FFI target name from the .Call() string argument
        // and creates an FfiCall edge to the extracted C function name
        assert_has_ffi_call_edge(&staging, "fast_mean", "fast_mean_c");
        assert_has_node(&staging, "fast_mean_c");
    }

    #[test]
    fn test_namespace_operator() {
        let source = r"
            filter_data <- function(df) {
              dplyr::filter(df, x > 10)
            }
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "filter_data", "dplyr::filter");
    }

    #[test]
    fn test_rcpp_ffi_call() {
        let source = r#"
            process_data <- function(x) {
              Rcpp::sourceCpp("functions.cpp")
            }
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        // Rcpp::sourceCpp("functions.cpp") is an FFI call — the graph builder
        // stores the canonical graph identity as `functions::cpp`.
        assert_has_ffi_call_edge(&staging, "process_data", "functions::cpp");
    }

    #[test]
    fn test_external_ffi_reason() {
        let source = r#"
            do_thing <- function(x) {
              .External("c_func", x)
            }
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        // .External("c_func", x) is an FFI call — the graph builder extracts
        // "c_func" as the FFI target from the string argument
        assert_has_ffi_call_edge(&staging, "do_thing", "c_func");
        assert_has_node(&staging, "c_func");
    }

    // ============================================================================
    // Import Edge Tests (Wave 7)
    // ============================================================================

    #[test]
    fn test_library_import_bare_identifier() {
        let source = r"
            library(dplyr)
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected at least one import edge"
        );

        // library() imports all exports from package (wildcard)
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(*is_wildcard, "library() should be wildcard import");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_library_import_string() {
        let source = r#"
            library("ggplot2")
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected library import edge with string"
        );

        // Verify it's an Import edge with wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(*is_wildcard, "library() should be wildcard import");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_source_import() {
        let source = r#"
            source("utils.R")
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected source import edge");

        // source() loads a specific file, NOT a wildcard import
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(!*is_wildcard, "source() should NOT be wildcard import");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_require_import() {
        let source = r"
            require(tidyr)
        ";

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected require import edge");

        // require() imports all exports from package (wildcard)
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(*is_wildcard, "require() should be wildcard import");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_load_namespace_import() {
        let source = r#"
            loadNamespace("methods")
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected loadNamespace import edge"
        );

        // loadNamespace() loads a namespace reference, NOT a wildcard import
        // (it doesn't put all symbols into scope like library() does)
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "loadNamespace() should NOT be wildcard (namespace reference)"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_multiple_imports() {
        let source = r#"
            library(dplyr)
            library(ggplot2)
            library(tidyr)
            source("helpers.R")
            require(stringr)
        "#;

        let (tree, content) = parse_r(source);
        let mut staging = StagingGraph::new();
        let builder = RGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.R"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert_eq!(import_edges.len(), 5, "Expected 5 import edges");

        // Verify all are EdgeKind::Imports
        for edge in &import_edges {
            assert!(
                matches!(edge, UnifiedEdgeKind::Imports { .. }),
                "All edges should be Imports"
            );
        }
    }
}

#[cfg(test)]
mod shape_tests {
    //! Coverage for the R [`ShapeMapping`]. Consumes the hand-written
    //! control-flow fixture so the test is load-bearing.

    use super::{cf_bucket_for_r_kind, r_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };
    use tree_sitter::{Node, Parser, Tree};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/dynamic/sample.R"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_r::LANGUAGE.into())
            .expect("load r grammar");
        parser.parse(src, None).expect("parse r")
    }

    /// First `function_definition` in document order (the top-level `classify`,
    /// bound through the `<-` assignment).
    fn first_function<'t>(tree: &'t Tree) -> Node<'t> {
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_definition" {
                return node;
            }
            let mut cursor = node.walk();
            let mut children: Vec<Node> = node.named_children(&mut cursor).collect();
            children.reverse();
            stack.extend(children);
        }
        panic!("no function_definition in r fixture");
    }

    #[test]
    fn mapping_is_non_empty_and_covers_real_kinds() {
        assert_eq!(cf_bucket_for_r_kind("if_statement"), Some(CfBucket::Branch));
        assert_eq!(cf_bucket_for_r_kind("for_statement"), Some(CfBucket::Loop));
        assert_eq!(
            cf_bucket_for_r_kind("while_statement"),
            Some(CfBucket::Loop)
        );
        assert_eq!(cf_bucket_for_r_kind("break"), Some(CfBucket::BreakContinue));
        assert_eq!(cf_bucket_for_r_kind("next"), Some(CfBucket::BreakContinue));
        assert_eq!(cf_bucket_for_r_kind("call"), Some(CfBucket::Call));
        assert_eq!(
            cf_bucket_for_r_kind("function_definition"),
            Some(CfBucket::Closure)
        );
        assert_eq!(cf_bucket_for_r_kind("nope"), None);

        let lang: tree_sitter::Language = tree_sitter_r::LANGUAGE.into();
        let id = (0..lang.node_kind_count())
            .map(|i| i as u16)
            .find(|&i| {
                lang.node_kind_is_named(i) && lang.node_kind_for_id(i) == Some("if_statement")
            })
            .expect("grammar exposes named if_statement");
        assert_eq!(r_shape_mapping().cf_bucket(id), Some(CfBucket::Branch));
    }

    #[test]
    fn descriptor_covers_fixture_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_function(&tree);
        let descriptor = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            r_shape_mapping(),
            &ShapeBudget::default(),
        );
        let hist = descriptor.cf_histogram;
        assert!(hist[CfBucket::Branch.index()] >= 1, "branch (if)");
        assert!(hist[CfBucket::Loop.index()] >= 1, "loop (for/while)");
        assert!(hist[CfBucket::BreakContinue.index()] >= 1, "break/next");
        assert!(hist[CfBucket::Call.index()] >= 1, "call");
        assert!(
            hist[CfBucket::Closure.index()] >= 1,
            "nested function literal"
        );
    }

    #[test]
    fn signature_shape_reads_arity_defaults_and_dots() {
        let tree = parse(SAMPLE);
        let func = first_function(&tree);
        let shape = r_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // `function(value, label = "n/a", ...)`.
        assert_eq!(shape.arity_positional, 2, "value + label");
        assert!(shape.has_defaults, "label has a default");
        assert!(shape.has_varargs, "...");
    }
}
