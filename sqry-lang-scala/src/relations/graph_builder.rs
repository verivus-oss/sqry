//! Scala `GraphBuilder` implementation for code graph construction.
//!
//! Extracts Scala-specific relationships:
//! - Class definitions (regular, case, sealed, objects, companion objects)
//! - Function definitions (regular, implicit, inline, extension functions)
//! - Call expressions (regular calls, method calls, extension calls)
//! - Import statements (simple, wildcard, selective, renamed, exclude patterns)
//! - Export edges (public classes, objects, traits, methods, and fields)
//!
//! # Multi-Pass Strategy
//!
//! 1. **Pass 1**: Extract class/object definitions → Create Class nodes + Export edges
//! 2. **Pass 2**: Extract function/property definitions → Create Function nodes
//! 3. **Pass 3**: Extract call expressions → Create Call edges
//! 4. **Pass 4**: Extract import declarations → Create Import edges

use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Span,
    unified::{
        ExportKind, GraphBuildHelper, StagingGraph, edge::kind::TypeOfContext, node::NodeId,
    },
};
use std::{collections::HashMap, path::Path};
use tree_sitter::{Node, Tree};

use super::type_extractor::{
    extract_all_type_names_from_scala_type, extract_type_string, is_type_node,
};

/// Scala-specific `GraphBuilder` implementation.
///
/// Performs multi-pass analysis:
/// 1. Extract class and object definitions + export public symbols
/// 2. Extract function and property definitions
/// 3. Extract call expressions
/// 4. Extract import statements
///
/// # Export Visibility Rules
///
/// In Scala, symbols are public by default unless marked `private`:
/// - Public classes, objects, and traits are exported
/// - Public methods (def) within classes/objects/traits are exported
/// - Public fields (val/var) within classes/objects/traits are exported
/// - Private symbols (marked with `private` modifier) are NOT exported
#[derive(Debug, Default, Clone, Copy)]
pub struct ScalaGraphBuilder;

impl ScalaGraphBuilder {
    /// Create a new Scala `GraphBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for ScalaGraphBuilder {
    fn language(&self) -> Language {
        Language::Scala
    }

    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Scala);

        // Build AST graph for call context tracking
        let ast_graph =
            ASTGraph::from_tree(tree, content, 4).map_err(|e| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            })?;

        // Walk tree to find classes, functions, and calls
        walk_tree_for_graph(tree.root_node(), content, &ast_graph, &mut helper)?;

        Ok(())
    }
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    #[allow(dead_code)] // Reserved for scope analysis
    span: (usize, usize),
    is_async: bool,
    is_method: bool,
    #[allow(dead_code)] // Reserved for class context tracking
    class_name: Option<String>,
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
        let mut class_stack: Vec<String> = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        let mut walk_context = WalkContext {
            content,
            contexts: &mut contexts,
            node_to_context: &mut node_to_context,
            scope_stack: &mut scope_stack,
            class_stack: &mut class_stack,
            max_depth,
            guard: &mut guard,
        };

        walk_ast(tree.root_node(), &mut walk_context)?;

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
struct WalkContext<'a> {
    content: &'a [u8],
    contexts: &'a mut Vec<CallContext>,
    node_to_context: &'a mut HashMap<usize, usize>,
    scope_stack: &'a mut Vec<String>,
    class_stack: &'a mut Vec<String>,
    max_depth: usize,
    guard: &'a mut sqry_core::query::security::RecursionGuard,
}

fn walk_ast(node: Node, context: &mut WalkContext<'_>) -> Result<(), String> {
    context
        .guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    if context.scope_stack.len() > context.max_depth {
        context.guard.exit();
        return Ok(());
    }

    match node.kind() {
        "class_definition" | "object_definition" | "trait_definition" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| format!("{} missing name", node.kind()))?;

            let class_name = name_node
                .utf8_text(context.content)
                .map_err(|_| "failed to read class name".to_string())?;

            // Build qualified class name
            let qualified_class = if context.scope_stack.is_empty() {
                class_name.to_string()
            } else {
                format!("{}.{}", context.scope_stack.join("."), class_name)
            };

            context.class_stack.push(qualified_class.clone());
            context.scope_stack.push(class_name.to_string());

            // Recurse into children (Scala doesn't have explicit body field for all types)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, context)?;
            }

            context.class_stack.pop();
            context.scope_stack.pop();
        }
        "function_definition" | "function_declaration" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| format!("{} missing name", node.kind()))?;

            let func_name = name_node
                .utf8_text(context.content)
                .map_err(|_| "failed to read function name".to_string())?;

            // Scala doesn't have async keyword; async is handled by Future/Task types
            let is_async = false;

            // Build qualified function name
            let qualified_func = if context.scope_stack.is_empty() {
                func_name.to_string()
            } else {
                format!("{}.{}", context.scope_stack.join("."), func_name)
            };

            // Determine if this is a method (inside a class)
            let is_method = !context.class_stack.is_empty();
            let class_name = context.class_stack.last().cloned();

            let context_idx = context.contexts.len();
            context.contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: (node.start_byte(), node.end_byte()),
                is_async,
                is_method,
                class_name,
            });

            // Associate all descendants with this context
            associate_descendants(node, context_idx, context.node_to_context);

            context.scope_stack.push(func_name.to_string());

            // Recurse into function body to find nested functions
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, context)?;
            }

            context.scope_stack.pop();
        }
        _ => {
            // Recurse into children for other node types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(child, context)?;
            }
        }
    }

    context.guard.exit();
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

/// Walk the tree and populate the staging graph.
#[allow(clippy::too_many_lines)]
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    match node.kind() {
        "class_definition" | "object_definition" | "trait_definition" => {
            // Extract class/object/trait name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);
                let qualified_name = class_name.to_string();
                let visibility = get_visibility(node, content);

                let node_id = if node.kind() == "trait_definition" {
                    helper.add_interface(&qualified_name, Some(span))
                } else {
                    helper.add_class_with_visibility(&qualified_name, Some(span), visibility)
                };

                // Export if not private (Scala members are public by default)
                let is_public_type = !is_private(node, content);
                if is_public_type {
                    export_from_file_module(helper, node_id);
                    // Only process member exports for public classes/objects/traits
                    process_member_exports(node, content, &qualified_name, helper);
                }

                // Process constructor parameters for case classes
                // Case classes have class_parameters that function as constructor parameters
                if node.kind() == "class_definition" {
                    let _ = process_function_parameters_typeof(node, node_id, content, helper);
                    let _ = process_function_return_typeof(node, node_id, content, helper);
                }

                // Extract inheritance/implementation from extends_clause
                // Scala syntax: class Foo extends Bar with Baz with Qux
                // tree-sitter-scala structure:
                //   class_definition
                //     identifier (class name)
                //     extends_clause
                //       extends (keyword)
                //       type_identifier (superclass)
                //       with (keyword)
                //       type_identifier (trait)
                //       ...
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "extends_clause" {
                        let mut first_type = true;
                        let mut extends_cursor = child.walk();
                        for extends_child in child.children(&mut extends_cursor) {
                            if extends_child.kind() == "type_identifier"
                                && let Ok(parent_name) = extends_child.utf8_text(content)
                            {
                                let parent_name = parent_name.trim();
                                if !parent_name.is_empty() {
                                    if first_type {
                                        // First type after extends is the superclass
                                        if node.kind() == "trait_definition" {
                                            // Trait extending another trait
                                            let parent_id = helper.add_interface(parent_name, None);
                                            helper.add_inherits_edge(node_id, parent_id);
                                        } else {
                                            // Class extending a class
                                            let parent_id = helper.add_class(parent_name, None);
                                            helper.add_inherits_edge(node_id, parent_id);
                                        }
                                        first_type = false;
                                    } else {
                                        // Subsequent types after "with" are traits (interfaces)
                                        let trait_id = helper.add_interface(parent_name, None);
                                        helper.add_implements_edge(node_id, trait_id);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "function_definition" | "function_declaration" => {
            // Extract function context from AST graph
            if let Some(call_context) = ast_graph.get_callable_context(node.id()) {
                let span = span_from_node(node);

                // Add function or method node with visibility
                let visibility = get_visibility(node, content);
                let func_id = if call_context.is_method {
                    helper.add_method_with_visibility(
                        &call_context.qualified_name,
                        Some(span),
                        call_context.is_async,
                        false, // Scala doesn't distinguish static methods at AST level
                        visibility,
                    )
                } else {
                    helper.add_function_with_visibility(
                        &call_context.qualified_name,
                        Some(span),
                        call_context.is_async,
                        false, // Scala doesn't have unsafe
                        visibility,
                    )
                };

                // Process TypeOf and Reference edges for parameters and return type
                let _ = process_function_parameters_typeof(node, func_id, content, helper);
                let _ = process_function_return_typeof(node, func_id, content, helper);

                // JNI/Scala Native: Create FFI edge for @native methods
                if has_native_annotation(node, content) {
                    build_native_method_ffi_edge(call_context, helper, node, content);
                }
            }
        }
        "call_expression" | "infix_expression" => {
            // Build call edge
            if let Some((caller_qname, callee_qname)) =
                build_call_for_staging(ast_graph, node, content)
            {
                // Ensure both nodes exist
                let call_context = ast_graph.get_callable_context(node.id());
                let is_async = call_context.is_some_and(|c| c.is_async);

                let source_id = helper.ensure_function(&caller_qname, None, is_async, false);
                let target_id = helper.ensure_function(&callee_qname, None, false, false);

                // Add call edge
                let argument_count = count_call_arguments(node);
                let call_span = span_from_node(node);
                helper.add_call_edge_full_with_span(
                    source_id,
                    target_id,
                    argument_count,
                    false,
                    vec![call_span],
                );
            }
        }
        "import_declaration" => {
            // Process import statements and create Import edges
            // Scala import syntax:
            //   - Simple: import scala.collection.mutable.Map
            //   - Wildcard: import scala.collection.mutable._
            //   - Multiple: import scala.collection.mutable.{Map, Set}
            //   - Renamed: import scala.collection.mutable.{Map => MutableMap}
            //   - Exclude: import scala.collection.mutable.{Map => _, _}
            process_import_declaration(node, content, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(child, content, ast_graph, helper)?;
    }

    Ok(())
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> Option<(String, String)> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: (0, content.len()),
            is_async: false,
            is_method: false,
            class_name: None,
        };
        &module_context
    };

    // Extract callee name based on node kind
    let callee_text = if call_node.kind() == "call_expression" {
        // Regular call: function field contains the identifier
        call_node
            .child_by_field_name("function")
            .and_then(|n| n.utf8_text(content).ok())
    } else if call_node.kind() == "infix_expression" {
        // Infix call: operator field contains the identifier
        call_node
            .child_by_field_name("operator")
            .and_then(|n| n.utf8_text(content).ok())
    } else {
        None
    };

    let callee_text = callee_text?;

    let callee_text = callee_text.trim().to_string();
    if callee_text.is_empty() {
        return None;
    }

    // Derive qualified callee name
    let caller_qname = call_context.qualified_name();
    let target_qname = simple_name(&callee_text).to_string();

    Some((caller_qname, target_qname))
}

fn count_call_arguments(call_node: Node<'_>) -> u8 {
    if call_node.kind() == "call_expression" {
        let count = call_node
            .child_by_field_name("arguments")
            .map_or(0, |args| args.named_child_count());
        return u8::try_from(count).unwrap_or(u8::MAX);
    }

    if call_node.kind() == "infix_expression" {
        let has_rhs = call_node
            .child_by_field_name("right")
            .or_else(|| call_node.child_by_field_name("rhs"))
            .is_some();
        return if has_rhs { 1 } else { 255 };
    }

    255
}

/// Module-level name for file scope (used as import source).
const FILE_MODULE_NAME: &str = "<module>";

/// Module name for export edges (distinct to avoid node cache collision).
const EXPORT_MODULE_NAME: &str = "<file_module>";

/// Process an import declaration and create Import edges.
///
/// Handles all Scala import patterns:
/// - Simple: `import scala.collection.mutable.Map`
/// - Wildcard: `import scala.collection.mutable._`
/// - Multiple: `import scala.collection.mutable.{Map, Set}`
/// - Renamed: `import scala.collection.mutable.{Map => MutableMap}`
/// - Exclude pattern: `import scala.collection.mutable.{Map => _, _}` (treated as wildcard)
fn process_import_declaration(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Get the full import text
    let Ok(import_text) = node.utf8_text(content) else {
        return;
    };

    // Strip "import " prefix
    let import_text = import_text.trim();
    let Some(path_text) = import_text.strip_prefix("import").map(str::trim) else {
        return;
    };

    if path_text.is_empty() {
        return;
    }

    // Get span for the import declaration
    let span = span_from_node(node);

    // Create the file module node as the importer
    let module_id = helper.add_module(FILE_MODULE_NAME, None);

    // Parse the import and create edges
    parse_and_create_import_edges(path_text, span, module_id, helper);
}

/// Parse import path and create the appropriate Import edges.
///
/// # Import Patterns
///
/// 1. **Simple import**: `scala.collection.mutable.Map`
///    - Creates edge from module to `scala.collection.mutable.Map`
///    - `alias: None`, `is_wildcard: false`
///
/// 2. **Wildcard import**: `scala.collection.mutable._`
///    - Creates edge from module to `scala.collection.mutable`
///    - `alias: None`, `is_wildcard: true`
///
/// 3. **Selective import**: `scala.collection.mutable.{Map, Set}`
///    - Creates separate edges for each import
///    - Each edge: `alias: None`, `is_wildcard: false`
///
/// 4. **Renamed import**: `scala.collection.mutable.{Map => MutableMap}`
///    - Creates edge with alias
///    - `alias: Some("MutableMap")`, `is_wildcard: false`
///
/// 5. **Exclude pattern**: `scala.collection.mutable.{Map => _, _}`
///    - `Map => _` is ignored (exclusion)
///    - `_` creates wildcard import
fn parse_and_create_import_edges(
    path_text: &str,
    span: Span,
    module_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    // Check for wildcard at the end: scala.collection.mutable._
    if let Some(prefix) = path_text.strip_suffix("._") {
        let prefix = prefix.trim();
        if !prefix.is_empty() {
            let import_id = helper.add_import(prefix, Some(span));
            helper.add_import_edge_full(module_id, import_id, None, true);
        }
        return;
    }

    // Check for selective imports with braces: scala.collection.{Map, Set}
    if let Some(brace_start) = path_text.find('{')
        && let Some(brace_end) = path_text.rfind('}')
    {
        let base_path = path_text[..brace_start].trim().trim_end_matches('.');
        let selectors_text = &path_text[brace_start + 1..brace_end];

        // Parse each selector
        for selector in split_selectors(selectors_text) {
            let selector = selector.trim();
            if selector.is_empty() {
                continue;
            }

            // Check for wildcard in selectors
            if selector == "_" || selector == "*" {
                // Wildcard import of base path
                if !base_path.is_empty() {
                    let import_id = helper.add_import(base_path, Some(span));
                    helper.add_import_edge_full(module_id, import_id, None, true);
                }
                continue;
            }

            // Check for rename: Name => Alias or Name => _
            if let Some((name_part, alias_part)) = selector.split_once("=>") {
                let name = name_part.trim();
                let alias = alias_part.trim();

                // Exclusion pattern: Map => _ means "exclude Map"
                if alias == "_" {
                    // Skip exclusions - they don't create imports
                    continue;
                }

                // Renamed import: Map => MutableMap
                let full_path = if base_path.is_empty() {
                    name.to_string()
                } else {
                    format!("{base_path}.{name}")
                };

                if !full_path.is_empty() {
                    let import_id = helper.add_import(&full_path, Some(span));
                    helper.add_import_edge_full(module_id, import_id, Some(alias), false);
                }
            } else {
                // Simple selector without rename
                let full_path = if base_path.is_empty() {
                    selector.to_string()
                } else {
                    format!("{base_path}.{selector}")
                };

                if !full_path.is_empty() {
                    let import_id = helper.add_import(&full_path, Some(span));
                    helper.add_import_edge_full(module_id, import_id, None, false);
                }
            }
        }
        return;
    }

    // Simple import: scala.collection.mutable.Map
    if !path_text.is_empty() {
        let import_id = helper.add_import(path_text, Some(span));
        helper.add_import_edge_full(module_id, import_id, None, false);
    }
}

/// Split selectors handling nested braces (for complex patterns).
///
/// Splits on commas while respecting brace nesting depth.
fn split_selectors(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut brace_depth = 0u32;
    let mut start = 0usize;

    for (idx, ch) in s.char_indices() {
        match ch {
            '{' => brace_depth = brace_depth.saturating_add(1),
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                parts.push(&s[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }

    // Add the final part
    if start < s.len() {
        parts.push(&s[start..]);
    }

    parts
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

fn simple_name(qualified: &str) -> &str {
    qualified.split('.').next_back().unwrap_or(qualified)
}

// ================================
// Export Detection (visibility)
// ================================

/// Extract visibility modifier from a Scala node.
/// Returns "private", "protected", or "public" (default in Scala).
#[allow(clippy::unnecessary_wraps)]
fn get_visibility(node: Node, content: &[u8]) -> Option<&'static str> {
    if has_modifier(node, "private", content) {
        Some("private")
    } else if has_modifier(node, "protected", content) {
        Some("protected")
    } else {
        // Scala members are public by default
        Some("public")
    }
}

/// Check if a node has the `private` modifier.
/// In Scala, members are public by default unless marked private.
fn is_private(node: Node, content: &[u8]) -> bool {
    has_modifier(node, "private", content)
}

/// Check if a node has a specific modifier.
fn has_modifier(node: Node, modifier: &str, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            // In Scala, the modifiers node directly contains the text
            if let Ok(text) = child.utf8_text(content) {
                // Check if the modifier text contains the target modifier
                return text.split_whitespace().any(|word| word == modifier);
            }
        }
    }
    false
}

// ============================================================================
// FFI Detection (@native annotation for JNI, @extern for Scala Native)
// ============================================================================

/// Check if a function has the @native annotation (JNI).
///
/// In Scala, @native indicates a JNI method implemented in native code.
/// This is equivalent to Java's `native` keyword.
fn has_native_annotation(node: Node, content: &[u8]) -> bool {
    has_annotation(node, "native", content)
}

/// Check if a node has a specific annotation.
///
/// Scala annotations can appear as:
/// - `annotation` node kind
/// - Simple form: `@native`
/// - Qualified form: `@scala.native`
/// - With arguments: `@native def foo(): Unit`
///
/// Handles both qualified and unqualified annotation names.
fn has_annotation(node: Node, annotation_name: &str, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "annotation" {
            // The annotation node contains the @ symbol and the name
            if let Ok(text) = child.utf8_text(content) {
                // Check for @native or @extern
                // Remove @ and any parentheses for comparison
                let cleaned = text
                    .trim()
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("");

                // Match both unqualified (native) and qualified (scala.native) forms
                if cleaned == annotation_name || cleaned.ends_with(&format!(".{annotation_name}")) {
                    return true;
                }
            }
        }
        // Also check modifiers node for nested annotations
        else if child.kind() == "modifiers" && has_annotation(child, annotation_name, content) {
            return true;
        }
    }
    false
}

/// Normalize a Scala type name by stripping scala. prefixes.
///
/// Scala allows both qualified and unqualified primitive type names:
/// - scala.Int → Int
/// - scala.Array[T] → Array[T]
/// - scala.collection.immutable.List → scala.collection.immutable.List (preserve)
fn normalize_scala_type(scala_type: &str) -> String {
    let trimmed = scala_type.trim();

    // Strip scala. prefix for primitives and Array
    if let Some(stripped) = trimmed.strip_prefix("scala.") {
        // Only strip scala. for primitives and Array
        match stripped {
            s if s.starts_with("Int") => stripped.to_string(),
            s if s.starts_with("Long") => stripped.to_string(),
            s if s.starts_with("Double") => stripped.to_string(),
            s if s.starts_with("Float") => stripped.to_string(),
            s if s.starts_with("Short") => stripped.to_string(),
            s if s.starts_with("Byte") => stripped.to_string(),
            s if s.starts_with("Char") => stripped.to_string(),
            s if s.starts_with("Boolean") => stripped.to_string(),
            s if s.starts_with("Unit") => stripped.to_string(),
            s if s.starts_with("Any") => stripped.to_string(),
            s if s.starts_with("Array[") => stripped.to_string(),
            // Keep other scala.* types qualified
            _ => trimmed.to_string(),
        }
    } else {
        trimmed.to_string()
    }
}

/// Convert a Scala type name to a JVM type descriptor.
///
/// JVM descriptors use a compact format:
/// - Primitives: I (int), D (double), Z (boolean), F (float), J (long), B (byte), S (short), C (char)
/// - Boxed types: Ljava/lang/Integer;, Ljava/lang/Double;, etc.
/// - Reference types: Ljava/lang/String;, Lscala/collection/immutable/List;
/// - Arrays: [I (int[]), [Ljava/lang/String; (String[])
///
/// Handles qualified Scala primitives (scala.Int → I) and Array aliases (scala.Array[T] → [T).
fn scala_type_to_jvm_descriptor(scala_type: &str) -> String {
    // Normalize first to handle scala. prefixes
    let normalized = normalize_scala_type(scala_type);
    let trimmed = normalized.trim();

    // Handle Array[T] (both Array[T] and scala.Array[T] after normalization)
    if let Some(array_content) = trimmed.strip_prefix("Array[")
        && let Some(element_type) = array_content.strip_suffix(']')
    {
        let element_descriptor = scala_type_to_jvm_descriptor(element_type.trim());
        return format!("[{element_descriptor}");
    }

    // Handle primitives
    match trimmed {
        "Int" => "I".to_string(),
        "Double" => "D".to_string(),
        "Float" => "F".to_string(),
        "Long" => "J".to_string(),
        "Short" => "S".to_string(),
        "Byte" => "B".to_string(),
        "Char" => "C".to_string(),
        "Boolean" => "Z".to_string(),
        "Unit" => "Lscala/runtime/BoxedUnit;".to_string(),
        "String" => "Ljava/lang/String;".to_string(),
        "Any" => "Ljava/lang/Object;".to_string(),
        "AnyRef" => "Ljava/lang/Object;".to_string(),
        "AnyVal" => "Ljava/lang/Object;".to_string(),
        // For other types, assume they're reference types
        _ => format!("L{};", trimmed.replace('.', "/")),
    }
}

/// Extract parameter types from a Scala function node.
///
/// Handles:
/// - Single parameter list: def f(x: Int)
/// - Multiple parameter lists (curried): def f(x: Int)(y: String)
/// - Context parameters: def f(x: Int)(using ctx: Context)
/// - Implicit parameters: def f(x: Int)(implicit ctx: Context)
fn extract_parameter_types_from_function(func_node: Node, content: &[u8]) -> Vec<String> {
    let mut param_types = Vec::new();

    // Scala can have multiple parameter lists (curried functions)
    // Walk all children to find all `parameters` nodes
    let mut cursor = func_node.walk();
    for child in func_node.children(&mut cursor) {
        if child.kind() == "parameters" {
            // Extract types from this parameter list
            extract_types_from_parameter_list(child, content, &mut param_types);
        }
    }

    param_types
}

/// Extract parameter types from a single parameter list node.
fn extract_types_from_parameter_list(
    params_node: Node,
    content: &[u8],
    param_types: &mut Vec<String>,
) {
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        match child.kind() {
            "parameter" | "class_parameter" => {
                // Direct parameter with type annotation
                if let Some(type_node) = child.child_by_field_name("type")
                    && let Ok(type_text) = type_node.utf8_text(content)
                {
                    param_types.push(type_text.to_string());
                }
            }
            "parameters" => {
                // Nested parameter list (shouldn't happen, but handle recursively)
                extract_types_from_parameter_list(child, content, param_types);
            }
            _ => {
                // Other nodes (commas, keywords like 'using', 'implicit')
                // Recurse to find nested parameters
                let mut param_cursor = child.walk();
                for param in child.children(&mut param_cursor) {
                    if (param.kind() == "parameter" || param.kind() == "class_parameter")
                        && let Some(type_node) = param.child_by_field_name("type")
                        && let Ok(type_text) = type_node.utf8_text(content)
                    {
                        param_types.push(type_text.to_string());
                    }
                }
            }
        }
    }
}

/// Generate JVM signature for method parameters.
///
/// Converts Scala parameter types to JVM descriptors and joins them with underscores.
/// Example: `(Int, String) -> I_Ljava_lang_String`
fn generate_jvm_signature(func_node: Node, content: &[u8]) -> String {
    let param_types = extract_parameter_types_from_function(func_node, content);

    // Convert each parameter type to JVM descriptor
    let descriptors: Vec<String> = param_types
        .iter()
        .map(|t| scala_type_to_jvm_descriptor(t))
        .collect();

    // Join descriptors with underscores and strip semicolons for readability
    let signature = descriptors.join("_");
    signature.replace(';', "")
}

/// Build FFI edge for @native method declaration.
///
/// In Scala, @native methods are JNI bridges (like Java's `native` keyword).
/// The annotation marks a method as implemented natively.
///
/// # Examples
///
/// ```scala
/// // JNI native method
/// class NativeLib {
///   @native def nativeMethod(): Unit
///   @native def process(x: Int, y: String): Long
/// }
///
/// // Scala Native C interop (using @extern)
/// @extern
/// object CLib {
///   def printf(format: CString): CInt = extern
/// }
/// ```
fn build_native_method_ffi_edge(
    context: &CallContext,
    helper: &mut GraphBuildHelper,
    func_node: Node,
    content: &[u8],
) {
    use sqry_core::graph::unified::edge::FfiConvention;

    // Get function span for FFI edge
    let span = span_from_node(func_node);

    // Generate JVM signature for parameters to disambiguate overloads
    let signature = generate_jvm_signature(func_node, content);

    // Create FFI target with signature suffix
    // Format: <ffi:qualified.name__SIGNATURE>
    let ffi_name = if signature.is_empty() {
        format!("<ffi:{}>", context.qualified_name())
    } else {
        format!("<ffi:{}__{}>", context.qualified_name(), signature)
    };

    let target_id = helper.add_function(&ffi_name, None, false, false);

    // Get the caller node ID (the @native method itself)
    let caller_id = if context.is_method {
        helper.ensure_method(
            &context.qualified_name(),
            Some(span),
            context.is_async,
            false,
        )
    } else {
        helper.ensure_function(
            &context.qualified_name(),
            Some(span),
            context.is_async,
            false,
        )
    };

    // Add FFI edge from method to native implementation
    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

/// Create an export edge from the file module to the exported node.
fn export_from_file_module(
    helper: &mut GraphBuildHelper,
    exported: sqry_core::graph::unified::node::NodeId,
) {
    let module_id = helper.add_module(EXPORT_MODULE_NAME, None);
    helper.add_export_edge_full(module_id, exported, ExportKind::Direct, None);
}

/// Process public methods and fields within a class/object/trait body for export edges.
///
/// In Scala:
/// - Methods and fields are public by default unless marked `private`
/// - Trait methods are also public by default
fn process_member_exports(
    type_node: Node,
    content: &[u8],
    type_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    // Find the body node (template_body for Scala)
    let body_node = if let Some(body) = type_node.child_by_field_name("body") {
        body
    } else {
        // If no explicit body field, look for template_body child by kind
        let mut cursor = type_node.walk();
        let body_opt = type_node
            .children(&mut cursor)
            .find(|child| child.kind() == "template_body");

        if let Some(body) = body_opt {
            body
        } else {
            return;
        }
    };

    // Iterate through children looking for function definitions
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "function_definition" | "function_declaration" => {
                // Export if not private (public by default in Scala)
                if !is_private(child, content)
                    && let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(method_name) = name_node.utf8_text(content)
                {
                    let qualified_name = format!("{type_qualified_name}.{method_name}");
                    let span = span_from_node(child);
                    let visibility = get_visibility(child, content);

                    // In Scala, methods are never async at AST level (uses Future types)
                    // Check if inside class/object to determine if it's a method
                    let method_id = helper.add_method_with_visibility(
                        &qualified_name,
                        Some(span),
                        false,
                        false,
                        visibility,
                    );
                    export_from_file_module(helper, method_id);
                }
            }
            "val_definition" | "var_definition" => {
                // Extract field name from pattern (may have multiple bindings)
                let is_public = !is_private(child, content);
                extract_and_export_field_names(
                    child,
                    content,
                    type_qualified_name,
                    helper,
                    is_public,
                );
            }
            _ => {}
        }
    }
}

/// Process `TypeOf` and Reference edges for function/method parameters.
///
/// Finds all parameters in the function signature and creates edges for each:
/// 1. `TypeOf` edge: function → parameter type (with Parameter context and index)
/// 2. Reference edges: function → each nested type name in parameter
///
/// # Examples
/// - `def foo(x: Int)` → TypeOf(foo, "Int", ctx=Parameter(0)), Ref(foo, Int)
/// - `def bar(a: String, b: List[User])` → TypeOf(bar, "String", ctx=Parameter(0)),
///   TypeOf(bar, "List[User]", ctx=Parameter(1)),
///   Ref(bar, String), Ref(bar, List), Ref(bar, User)
fn process_function_parameters_typeof(
    func_node: Node,
    func_id: NodeId,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find parameters node
    // Structure:
    //   - function_definition → parameters → parameter*
    //   - class_definition (case class) → class_parameters → class_parameter*
    let params_node = func_node.child_by_field_name("parameters").or_else(|| {
        // Fallback: find by kind (handles both "parameters" and "class_parameters")
        let mut cursor = func_node.walk();
        func_node.children(&mut cursor).find(|c| {
            matches!(
                c.kind(),
                "parameters" | "parameter_list" | "class_parameters"
            )
        })
    });

    let Some(params_node) = params_node else {
        // No parameters - not an error
        return Ok(());
    };

    // Process each parameter
    let mut cursor = params_node.walk();
    let mut param_index: u16 = 0;
    for param in params_node.children(&mut cursor) {
        if matches!(param.kind(), "parameter" | "class_parameter") {
            process_parameter_typeof(func_id, param, param_index, content, helper)?;
            param_index = param_index.saturating_add(1);
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a single parameter.
///
/// # Structure
/// - `parameter → identifier → : → type_node`
/// - Example: `x: Int` → identifier="x", type="Int"
#[allow(clippy::unnecessary_wraps)]
fn process_parameter_typeof(
    func_id: NodeId,
    param_node: Node,
    param_index: u16,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find parameter name
    let param_name = param_node
        .child_by_field_name("name")
        .or_else(|| {
            // Fallback: find first identifier
            let mut cursor = param_node.walk();
            param_node
                .children(&mut cursor)
                .find(|c| c.kind() == "identifier")
        })
        .and_then(|n| n.utf8_text(content).ok());

    // Find type annotation after colon
    let mut cursor = param_node.walk();
    let mut found_colon = false;
    let type_node = param_node.children(&mut cursor).find(|child| {
        if child.kind() == ":" {
            found_colon = true;
            false
        } else {
            found_colon && is_type_node(child.kind())
        }
    });

    let Some(type_node) = type_node else {
        // No type annotation - type inference, not an error
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    if type_text.is_empty() {
        return Ok(());
    }

    // Create TypeOf edge with Parameter context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(param_index),
        param_name,
    );

    // Extract all nested type names for Reference edges
    let referenced_types = extract_all_type_names_from_scala_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for function/method return type.
///
/// Finds the return type annotation and creates:
/// 1. `TypeOf` edge: function → return type (with Return context, index 0)
/// 2. Reference edges: function → each nested type name in return type
///
/// # Examples
/// - `def foo(): Int` → TypeOf(foo, "Int", ctx=Return(0)), Ref(foo, Int)
/// - `def bar(): List[User]` → TypeOf(bar, "List[User]", ctx=Return(0)),
///   Ref(bar, List), Ref(bar, User)
#[allow(clippy::unnecessary_wraps)]
fn process_function_return_typeof(
    func_node: Node,
    func_id: NodeId,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find return type annotation
    // Structure: function_definition → : → type_node
    // The return type comes after the closing paren of parameters
    let return_type_node = func_node.child_by_field_name("return_type").or_else(|| {
        // Fallback: find type after colon (following parameters)
        let mut cursor = func_node.walk();
        let mut found_params = false;
        let mut found_colon = false;
        func_node.children(&mut cursor).find(|child| {
            if child.kind() == "parameters" {
                found_params = true;
                false
            } else if found_params && child.kind() == ":" {
                found_colon = true;
                false
            } else {
                found_colon && is_type_node(child.kind())
            }
        })
    });

    let Some(return_type_node) = return_type_node else {
        // No return type annotation - type inference or Unit, not an error
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(return_type_node, content);
    if type_text.is_empty() {
        return Ok(());
    }

    // Create TypeOf edge with Return context (index 0)
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0),
        None,
    );

    // Extract all nested type names for Reference edges
    let referenced_types = extract_all_type_names_from_scala_type(return_type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a field (val/var).
///
/// Finds the type annotation (after `:`) and creates:
/// 1. `TypeOf` edge: field → full type string
/// 2. Reference edges: field → each nested type name
///
/// # Examples
/// - `val users: List[User]` → TypeOf(users, "List[User]"), Ref(users, List), Ref(users, User)
/// - `var count: Int` → TypeOf(count, "Int"), Ref(count, Int)
#[allow(clippy::unnecessary_wraps)]
fn process_field_typeof_edges(
    field_node: Node,
    field_id: NodeId,
    field_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Find type annotation after colon
    // Structure: val_definition → identifier → : → type_node → = → expression
    let mut cursor = field_node.walk();
    let mut found_colon = false;
    let type_node = field_node.children(&mut cursor).find(|child| {
        if child.kind() == ":" {
            found_colon = true;
            false
        } else {
            found_colon && is_type_node(child.kind())
        }
    });

    let Some(type_node) = type_node else {
        // No type annotation - type inference, not an error
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    if type_text.is_empty() {
        // Invalid type annotation - not an error, just skip
        return Ok(());
    }

    // Create TypeOf edge with Field context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        field_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(field_name),
    );

    // Extract all nested type names for Reference edges
    let referenced_types = extract_all_type_names_from_scala_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(field_id, ref_type_id);
    }

    Ok(())
}

/// Extract and export field names from val/var definitions.
/// Handles patterns like: `val x = 1` or `var (a, b) = (1, 2)`
fn extract_and_export_field_names(
    field_node: Node,
    content: &[u8],
    type_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    is_public: bool,
) {
    // Look for identifier in the pattern
    let mut cursor = field_node.walk();
    for child in field_node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(field_name) = child.utf8_text(content)
        {
            let qualified_name = format!("{type_qualified_name}.{field_name}");
            let span = span_from_node(child);

            // Use constant for val (immutable), variable for var (mutable)
            let field_id = if field_node.kind() == "val_definition" {
                helper.add_constant(&qualified_name, Some(span))
            } else {
                helper.add_variable(&qualified_name, Some(span))
            };

            // Export only public fields
            if is_public {
                export_from_file_module(helper, field_id);
            }

            // Process TypeOf and Reference edges for the field (both public and private)
            let _ = process_field_typeof_edges(field_node, field_id, field_name, content, helper);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_scala(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
            .expect("Failed to set Scala language");
        parser
            .parse(source.as_bytes(), None)
            .expect("Failed to parse Scala source")
    }

    #[test]
    fn test_scala_graph_builder_new() {
        let builder = ScalaGraphBuilder::new();
        assert_eq!(builder.language(), Language::Scala);
    }

    #[test]
    fn test_class_inheritance_creates_inherits_edge() {
        // class Child extends Parent should create an Inherits edge
        let source = r#"
            class Parent {
                def greet(): String = "Hello"
            }

            class Child extends Parent {
                def wave(): String = "Hi"
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());

        // Check for Inherits edge
        let ops = staging.operations();
        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge for Child extends Parent, found {inherits_count}"
        );
    }

    #[test]
    fn test_class_with_traits_creates_implements_edges() {
        // class Foo extends Bar with Baz with Qux should create 1 Inherits + 2 Implements
        let source = r"
            class Parent
            trait Comparable
            trait Serializable

            class Widget extends Parent with Comparable with Serializable {
                def compare(): Int = 0
            }
        ";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());

        let ops = staging.operations();

        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        let implements_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Implements,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge for Widget extends Parent, found {inherits_count}"
        );
        assert_eq!(
            implements_count, 2,
            "Expected 2 Implements edges for Widget with Comparable with Serializable, found {implements_count}"
        );
    }

    #[test]
    fn test_trait_extends_trait_creates_inherits_edge() {
        // trait Child extends Parent should create an Inherits edge
        let source = r"
            trait Parent {
                def greet(): String
            }

            trait Child extends Parent {
                def wave(): String
            }
        ";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());

        // Check for Inherits edge (trait extending trait)
        let ops = staging.operations();
        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge for trait Child extends Parent, found {inherits_count}"
        );
    }

    // ========================================================================
    // Import Edge Tests
    // ========================================================================

    /// Helper to count Import edges in staging operations
    fn count_import_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Imports { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper to check if a wildcard import edge exists
    fn has_wildcard_import(staging: &StagingGraph) -> bool {
        staging.operations().iter().any(|op| {
            matches!(
                op,
                sqry_core::graph::unified::StagingOp::AddEdge {
                    kind: sqry_core::graph::unified::EdgeKind::Imports {
                        is_wildcard: true,
                        ..
                    },
                    ..
                }
            )
        })
    }

    /// Helper to check if an aliased import edge exists
    fn has_aliased_import(staging: &StagingGraph) -> bool {
        staging.operations().iter().any(|op| {
            matches!(
                op,
                sqry_core::graph::unified::StagingOp::AddEdge {
                    kind: sqry_core::graph::unified::EdgeKind::Imports { alias: Some(_), .. },
                    ..
                }
            )
        })
    }

    #[test]
    fn test_simple_import_creates_import_edge() {
        // import scala.collection.mutable.Map
        let source = r"import scala.collection.mutable.Map";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Simple import should create 1 Import edge"
        );
        assert!(
            !has_wildcard_import(&staging),
            "Simple import should not be wildcard"
        );
        assert!(
            !has_aliased_import(&staging),
            "Simple import should not have alias"
        );
    }

    #[test]
    fn test_wildcard_import_creates_wildcard_edge() {
        // import scala.collection.mutable._
        let source = r"import scala.collection.mutable._";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Wildcard import should create 1 Import edge"
        );
        assert!(
            has_wildcard_import(&staging),
            "Wildcard import should have is_wildcard: true"
        );
    }

    #[test]
    fn test_multiple_imports_in_braces() {
        // import scala.collection.mutable.{Map, Set}
        let source = r"import scala.collection.mutable.{Map, Set}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            2,
            "Multiple imports {{Map, Set}} should create 2 Import edges"
        );
        assert!(
            !has_wildcard_import(&staging),
            "Named imports should not be wildcards"
        );
    }

    #[test]
    fn test_renamed_import_creates_aliased_edge() {
        // import scala.collection.mutable.{Map => MutableMap}
        let source = r"import scala.collection.mutable.{Map => MutableMap}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Renamed import should create 1 Import edge"
        );
        assert!(
            has_aliased_import(&staging),
            "Renamed import should have alias"
        );
        assert!(
            !has_wildcard_import(&staging),
            "Renamed import should not be wildcard"
        );
    }

    #[test]
    fn test_exclude_pattern_with_wildcard() {
        // import scala.collection.mutable.{Map => _, _}
        // Map => _ is an exclusion (ignored), _ is wildcard
        let source = r"import scala.collection.mutable.{Map => _, _}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        // Only the wildcard should create an edge; Map => _ is excluded
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Exclude pattern {{Map => _, _}} should create 1 wildcard Import edge"
        );
        assert!(
            has_wildcard_import(&staging),
            "Exclude pattern with _ should have wildcard"
        );
    }

    #[test]
    fn test_mixed_imports_renamed_and_simple() {
        // import scala.collection.mutable.{Map => MutableMap, Set, List}
        let source = r"import scala.collection.mutable.{Map => MutableMap, Set, List}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            3,
            "Mixed imports should create 3 Import edges (1 renamed + 2 simple)"
        );
        assert!(
            has_aliased_import(&staging),
            "Mixed imports should have at least one alias"
        );
    }

    #[test]
    fn test_multiple_import_statements() {
        // Multiple separate import statements
        let source = r"
            import scala.collection.mutable.Map
            import scala.io.Source
            import java.util.ArrayList
        ";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            3,
            "Three import statements should create 3 Import edges"
        );
    }

    #[test]
    fn test_wildcard_in_braces() {
        // import scala.collection.mutable.{_}
        let source = r"import scala.collection.mutable.{_}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Wildcard in braces {{_}} should create 1 Import edge"
        );
        assert!(
            has_wildcard_import(&staging),
            "Wildcard in braces should have is_wildcard: true"
        );
    }

    #[test]
    fn test_import_with_asterisk_wildcard() {
        // Some Scala dialects use * instead of _ for wildcard
        // import scala.collection.mutable.{*}
        let source = r"import scala.collection.mutable.{*}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Asterisk wildcard {{*}} should create 1 Import edge"
        );
        assert!(
            has_wildcard_import(&staging),
            "Asterisk wildcard should have is_wildcard: true"
        );
    }

    #[test]
    fn test_import_mixed_with_classes() {
        // Import statements alongside class definitions
        let source = r"
            import scala.collection.mutable.Map

            class MyClass {
                def useMap(): Unit = {}
            }

            import scala.io.Source
        ";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            2,
            "Two import statements mixed with class should create 2 Import edges"
        );
    }

    #[test]
    fn test_single_item_in_braces() {
        // import scala.collection.mutable.{Map}
        let source = r"import scala.collection.mutable.{Map}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Single item in braces {{Map}} should create 1 Import edge"
        );
        assert!(
            !has_wildcard_import(&staging),
            "Single named item should not be wildcard"
        );
    }

    #[test]
    fn test_java_import() {
        // import java.util.{HashMap, ArrayList}
        let source = r"import java.util.{HashMap, ArrayList}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        assert_eq!(
            count_import_edges(&staging),
            2,
            "Java imports should create 2 Import edges"
        );
    }

    #[test]
    fn test_exclude_only_pattern() {
        // import scala.collection.mutable.{Map => _}
        // This excludes Map but imports nothing else
        let source = r"import scala.collection.mutable.{Map => _}";
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        // Exclusion-only pattern should create no import edges
        assert_eq!(
            count_import_edges(&staging),
            0,
            "Exclusion-only pattern {{Map => _}} should create 0 Import edges"
        );
    }

    // ========================================================================
    // Export Edge Tests
    // ========================================================================

    /// Helper to count Export edges in staging operations
    fn count_export_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count()
    }

    #[test]
    fn test_public_class_creates_export_edge() {
        // class User should be exported (public by default)
        let source = r#"
            class User(val name: String, val age: Int) {
                def getName: String = name
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert!(
            export_count >= 1,
            "Public class User should create at least 1 Export edge (for class), found {export_count}"
        );
    }

    #[test]
    fn test_public_object_creates_export_edge() {
        // object UserService should be exported (public by default)
        let source = r#"
            object UserService {
                def createUser(name: String): String = name
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert!(
            export_count >= 1,
            "Public object UserService should create at least 1 Export edge (for object), found {export_count}"
        );
    }

    #[test]
    fn test_public_trait_creates_export_edge() {
        // trait Repository should be exported (public by default)
        let source = r#"
            trait Repository {
                def save(item: String): Unit
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert!(
            export_count >= 1,
            "Public trait Repository should create at least 1 Export edge (for trait), found {export_count}"
        );
    }

    #[test]
    fn test_private_class_does_not_export() {
        // private class Internal should NOT be exported
        let source = r#"
            private class Internal {
                def process(): Unit = {}
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Private class Internal should create 0 Export edges, found {export_count}"
        );
    }

    #[test]
    fn test_private_object_does_not_export() {
        // private object Hidden should NOT be exported
        let source = r#"
            private object Hidden {
                def secret(): String = "hidden"
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Private object Hidden should create 0 Export edges, found {export_count}"
        );
    }

    #[test]
    fn test_public_class_with_public_methods_exports() {
        // Public class with public methods - both should export
        let source = r#"
            class Service {
                def publicMethod(): String = "public"
                private def privateMethod(): String = "private"
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        // Should export: class Service + publicMethod (2 exports)
        // Should NOT export: privateMethod
        assert_eq!(
            export_count, 2,
            "Public class with 1 public method should create 2 Export edges (class + method), found {export_count}"
        );
    }

    #[test]
    fn test_object_with_multiple_public_methods() {
        // object Utils with multiple public methods
        let source = r#"
            object Utils {
                def greet(name: String): String = s"Hello, $name"
                def farewell(name: String): String = s"Goodbye, $name"
                private def helper(): Int = 42
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        // Should export: object Utils + greet + farewell (3 exports)
        // Should NOT export: helper (private)
        assert_eq!(
            export_count, 3,
            "Object Utils with 2 public methods should create 3 Export edges (object + 2 methods), found {export_count}"
        );
    }

    #[test]
    fn test_mixed_visibility_classes() {
        // Mix of public and private classes
        let source = r#"
            class PublicClass {
                def publicMethod(): Unit = {}
            }

            private class PrivateClass {
                def someMethod(): Unit = {}
            }

            object PublicObject {
                def run(): Unit = {}
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        // Should export: PublicClass + publicMethod + PublicObject + run (4 exports)
        // Should NOT export: PrivateClass, someMethod
        assert_eq!(
            export_count, 4,
            "1 public class + 1 public object with methods should create 4 Export edges, found {export_count}"
        );
    }

    #[test]
    fn test_case_class_exports() {
        // case class should be exported (public by default)
        let source = r#"
            case class User(name: String, age: Int)
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        assert!(
            export_count >= 1,
            "Case class User should create at least 1 Export edge, found {export_count}"
        );
    }

    #[test]
    fn test_trait_with_methods() {
        // Trait with public method declarations
        let source = r#"
            trait Service {
                def execute(): Unit
                def validate(): Boolean
                private def internal(): Int
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());
        let export_count = count_export_edges(&staging);
        // Should export: trait Service + execute + validate (3 exports)
        // Should NOT export: internal (private)
        assert_eq!(
            export_count, 3,
            "Trait with 2 public methods should create 3 Export edges (trait + 2 methods), found {export_count}"
        );
    }

    #[test]
    fn test_export_edges_use_direct_kind() {
        // Verify that export edges use ExportKind::Direct
        let source = r#"
            class User {
                def getName(): String = "Alice"
            }
        "#;
        let tree = parse_scala(source);
        let mut staging = StagingGraph::new();
        let builder = ScalaGraphBuilder::new();

        let result = builder.build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.scala"),
            &mut staging,
        );

        assert!(result.is_ok());

        // Check that all export edges have ExportKind::Direct
        let ops = staging.operations();
        let export_edges: Vec<_> = ops
            .iter()
            .filter_map(|op| {
                if let sqry_core::graph::unified::StagingOp::AddEdge {
                    kind: sqry_core::graph::unified::EdgeKind::Exports { kind, .. },
                    ..
                } = op
                {
                    Some(kind)
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !export_edges.is_empty(),
            "Should have at least one export edge"
        );

        for kind in export_edges {
            assert_eq!(
                *kind,
                ExportKind::Direct,
                "All export edges should use ExportKind::Direct"
            );
        }
    }
}
