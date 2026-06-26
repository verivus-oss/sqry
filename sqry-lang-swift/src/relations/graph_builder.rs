//! Swift `GraphBuilder` implementation for Tier-2 graph coverage.
//!
//! Implements the `GraphBuilder` trait to extract call graphs from Swift codebases,
//! including async/await patterns, throws/rethrows, protocol extensions, and bridging headers.
//!
//! # Supported Features
//!
//! - Function definitions (top-level and nested)
//! - Method definitions (instance and static)
//! - Function call expressions
//! - Method calls (`object.method()`)
//! - Chained method calls (`a.b().c()`)
//! - Async/await detection
//! - Trailing closure calls
//! - Class/Struct/Enum/Protocol declarations
//! - Inheritance and protocol conformance edges
//!
//! # Architecture Notes
//!
//! - Pre-compute callable contexts for O(1) containment lookups during traversal.
//! - Implicit receiver resolution: `foo()` inside `Type.method()` resolves to `Type.foo()`
//!   when that method exists, avoiding false global function references.
//! - Swift visibility semantics: default `internal` symbols are exported; `private` and
//!   `fileprivate` are not.
//! - Bridging headers enable C FFI call detection and cross-language edges.

use crate::relations::{BridgingHeaderLocator, SwiftBridgingIndex};
use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeId as UnifiedNodeId;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span,
    unified::{GraphBuildHelper, StagingGraph},
};
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::{Node, Tree};

/// Maximum type nesting depth allowed for graph extraction.
const DEFAULT_MAX_SCOPE_DEPTH: usize = 64;

/// Maximum AST depth to prevent pathological recursion.
const DEFAULT_MAX_AST_DEPTH: usize = 256;

/// Swift graph builder for call graph extraction.
pub struct SwiftGraphBuilder {
    max_scope_depth: usize,
}

impl Default for SwiftGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_MAX_SCOPE_DEPTH,
        }
    }
}

impl SwiftGraphBuilder {
    /// Create a new Swift graph builder with custom max scope depth.
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for SwiftGraphBuilder {
    fn language(&self) -> Language {
        Language::Swift
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(swift_shape_mapping())
    }

    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Swift);

        // Build AST context to track function/method contexts
        let ast_context = ASTContext::from_tree(tree, content, self.max_scope_depth);

        if let Some(header_path) = BridgingHeaderLocator::find_header(file) {
            SwiftBridgingIndex::index_header(&header_path).map_err(|reason| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason,
                }
            })?;
        }

        // Phase 0: Create extension nodes
        extract_extensions(tree.root_node(), content, &mut helper);

        // Phase 1: Create function/method/class nodes
        for context in &ast_context.callable_contexts {
            let node_id = if context.is_method {
                helper.add_method_with_visibility(
                    &context.qualified_name,
                    Some(context.span),
                    context.is_async,
                    false,
                    Some(context.visibility),
                )
            } else {
                helper.add_function_with_visibility(
                    &context.qualified_name,
                    Some(context.span),
                    context.is_async,
                    false,
                    Some(context.visibility),
                )
            };

            // Export public/internal top-level functions (not methods)
            // Swift default: symbols without modifier are internal (module-scoped)
            if !context.is_method && context.is_exported {
                export_from_file_module(&mut helper, node_id);
            }
        }

        // Phase 2: Walk the tree to extract edges (calls, inheritance, etc.)
        let mut callable_stack: Vec<(UnifiedNodeId, String)> = Vec::new();
        let mut type_stack: Vec<String> = Vec::new();
        walk_tree(
            &mut helper,
            &ast_context,
            tree.root_node(),
            content,
            &mut callable_stack,
            &mut type_stack,
            false,
            0,
            self.max_scope_depth,
        );

        // Phase 3: Process TypeOf/Reference edges for top-level variables
        process_toplevel_variables(tree.root_node(), content, &mut helper);

        Ok(())
    }
}

// ============================================================================
// AST Context Types
// ============================================================================

/// Tracks callable (function/method) contexts discovered in the AST.
#[derive(Debug)]
struct CallableContext {
    /// Fully qualified name: "ClassName.methodName" or just "functionName"
    qualified_name: String,
    /// Byte span of the callable body (used for containment lookups)
    byte_span: (usize, usize),
    /// Proper span with row/column info (used for node creation)
    span: Span,
    /// Whether this is a method (vs top-level function)
    is_method: bool,
    /// Whether this callable is async
    is_async: bool,
    /// Whether this callable is exported (public or internal, not private/fileprivate)
    is_exported: bool,
    /// Normalized visibility ("public" or "private").
    visibility: &'static str,
}

/// Pre-computed AST context for O(1) lookups during call edge detection.
struct ASTContext {
    callable_contexts: Vec<CallableContext>,
}

impl ASTContext {
    fn from_tree(tree: &Tree, content: &[u8], max_scope_depth: usize) -> Self {
        let mut contexts = Vec::new();
        let mut type_stack: Vec<String> = Vec::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .expect("Failed to load recursion limits");
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .expect("Invalid file_ops_depth configuration");
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .expect("Failed to create recursion guard");

        if let Err(e) = extract_callable_contexts(
            tree.root_node(),
            content,
            &mut contexts,
            &mut type_stack,
            0,
            max_scope_depth,
            &mut guard,
        ) {
            eprintln!("Warning: Swift AST traversal hit recursion limit: {e}");
        }
        Self {
            callable_contexts: contexts,
        }
    }

    /// Find the enclosing callable context for a given byte position.
    fn find_enclosing(&self, byte_pos: usize) -> Option<&CallableContext> {
        self.callable_contexts
            .iter()
            .filter(|ctx| byte_pos >= ctx.byte_span.0 && byte_pos < ctx.byte_span.1)
            .min_by_key(|ctx| ctx.byte_span.1 - ctx.byte_span.0) // Prefer innermost scope
    }
}

// ============================================================================
// Extension Extraction
// ============================================================================

/// Extract extension declarations and create Module nodes for them.
///
/// Extensions in Swift are created as Module nodes with names like "extension String".
/// This allows the graph to represent extension membership and makes it possible to
/// query for all extensions of a particular type.
fn extract_extensions(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Note: tree-sitter-swift uses "class_declaration" for extensions in the grammar
    if node.kind() == "class_declaration" {
        // Check if this is actually an extension by looking at the modifiers or keyword
        let node_text = node.utf8_text(content).unwrap_or("");
        if node_text.trim_start().starts_with("extension") {
            // Extract the extended type name - it's in the same place as class names
            if let Some(type_name) = extract_type_name(&node, content) {
                let extension_name = format!("extension {type_name}");
                let span = node_to_span(&node);
                helper.add_module(&extension_name, Some(span));
            }
        }
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_extensions(child, content, helper);
    }
}

// ============================================================================
// Context Extraction (Phase 1)
// ============================================================================

/// Recursively extract callable contexts from the Swift AST.
#[allow(
    clippy::too_many_lines,
    reason = "Context extraction handles diverse Swift constructs in one pass."
)]
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_callable_contexts(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallableContext>,
    type_stack: &mut Vec<String>,
    depth: usize,
    max_scope_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    guard.enter()?;

    if depth > DEFAULT_MAX_AST_DEPTH {
        guard.exit();
        return Ok(());
    }

    if type_stack.len() > max_scope_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        // Type declarations that can contain methods
        "class_declaration" | "protocol_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    guard.exit();
                    return Ok(());
                }
                type_stack.push(name);
                // Recurse into body
                if let Some(body) = node.child_by_field_name("body") {
                    extract_callable_contexts(
                        body,
                        content,
                        contexts,
                        type_stack,
                        depth + 1,
                        max_scope_depth,
                        guard,
                    )?;
                }
                // Also check children without field name (tree-sitter-swift quirk)
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "class_body" || child.kind().contains("body") {
                        extract_callable_contexts(
                            child,
                            content,
                            contexts,
                            type_stack,
                            depth + 1,
                            max_scope_depth,
                            guard,
                        )?;
                    }
                }
                type_stack.pop();
                guard.exit();
                return Ok(()); // Don't process children twice
            }
        }
        // Struct is handled via class_declaration in tree-sitter-swift
        // but it might have different kind depending on grammar version
        "struct_declaration"
        | "enum_declaration"
        | "actor_declaration"
        | "extension_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    guard.exit();
                    return Ok(());
                }
                type_stack.push(name);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_callable_contexts(
                        child,
                        content,
                        contexts,
                        type_stack,
                        depth + 1,
                        max_scope_depth,
                        guard,
                    )?;
                }
                type_stack.pop();
                guard.exit();
                return Ok(());
            }
        }
        "function_declaration" => {
            if let Some(name) = extract_function_name(&node, content) {
                let is_async =
                    has_modifier(&node, content, "async") || has_child_kind(&node, "async");
                let qualified_name = build_qualified_name(type_stack, &name);
                let is_method = !type_stack.is_empty();
                let is_exported = is_exported_symbol(&node, content);
                let visibility = visibility_for_node(&node, content);

                contexts.push(CallableContext {
                    qualified_name,
                    byte_span: (node.start_byte(), node.end_byte()),
                    span: node_to_span(&node),
                    is_method,
                    is_async,
                    is_exported,
                    visibility,
                });
            }
        }
        "init_declaration" | "deinit_declaration" => {
            let name = if node.kind() == "init_declaration" {
                "init"
            } else {
                "deinit"
            };
            let qualified_name = build_qualified_name(type_stack, name);
            let is_exported = is_exported_symbol(&node, content);
            contexts.push(CallableContext {
                qualified_name,
                byte_span: (node.start_byte(), node.end_byte()),
                span: node_to_span(&node),
                is_method: true,
                is_async: has_modifier(&node, content, "async"),
                is_exported,
                visibility: visibility_for_node(&node, content),
            });
        }
        "subscript_declaration" => {
            let qualified_name = build_qualified_name(type_stack, "subscript");
            let is_exported = is_exported_symbol(&node, content);
            contexts.push(CallableContext {
                qualified_name,
                byte_span: (node.start_byte(), node.end_byte()),
                span: node_to_span(&node),
                is_method: true,
                is_async: false,
                is_exported,
                visibility: visibility_for_node(&node, content),
            });
        }
        _ => {}
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_callable_contexts(
            child,
            content,
            contexts,
            type_stack,
            depth + 1,
            max_scope_depth,
            guard,
        )?;
    }

    guard.exit();
    Ok(())
}

// ============================================================================
// Tree Walking (Phase 2) - Edge Extraction
// ============================================================================

/// Walk the Swift AST to extract nodes and edges.
#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::too_many_lines,
    reason = "Traversal spans call edges, scopes, and relations without splitting state."
)]
fn walk_tree(
    helper: &mut GraphBuildHelper,
    ast_context: &ASTContext,
    node: Node,
    content: &[u8],
    callable_stack: &mut Vec<(UnifiedNodeId, String)>,
    type_stack: &mut Vec<String>,
    in_await: bool,
    depth: usize,
    max_scope_depth: usize,
) {
    if depth > DEFAULT_MAX_AST_DEPTH {
        return;
    }

    if type_stack.len() > max_scope_depth {
        return;
    }

    let mut pushed_callable = false;
    let mut pushed_type = false;

    match node.kind() {
        // Type declarations - add class/struct/protocol/enum nodes
        "class_declaration" | "protocol_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    return;
                }
                let qualified_name = build_qualified_name(type_stack, &name);
                let span = Some(node_to_span(&node));
                let visibility = visibility_for_node(&node, content);

                let type_id = if node.kind() == "protocol_declaration" {
                    helper.add_interface_with_visibility(&qualified_name, span, Some(visibility))
                } else {
                    // Check if it's actually a struct (class_declaration can be struct in tree-sitter-swift)
                    if has_child_kind(&node, "struct") {
                        helper.add_struct_with_visibility(&qualified_name, span, Some(visibility))
                    } else {
                        helper.add_class_with_visibility(&qualified_name, span, Some(visibility))
                    }
                };

                // Export public/internal top-level types (not nested)
                if type_stack.is_empty() && is_exported_symbol(&node, content) {
                    export_from_file_module(helper, type_id);
                }

                // Handle inheritance/conformance
                process_inheritance(helper, &node, content, type_stack, &qualified_name);

                type_stack.push(name);
                pushed_type = true;
            }
        }
        "struct_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    return;
                }
                let qualified_name = build_qualified_name(type_stack, &name);
                let span = Some(node_to_span(&node));
                let visibility = visibility_for_node(&node, content);
                let struct_id =
                    helper.add_struct_with_visibility(&qualified_name, span, Some(visibility));

                // Export public/internal top-level structs (not nested)
                if type_stack.is_empty() && is_exported_symbol(&node, content) {
                    export_from_file_module(helper, struct_id);
                }

                process_inheritance(helper, &node, content, type_stack, &qualified_name);
                type_stack.push(name);
                pushed_type = true;
            }
        }
        "enum_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    return;
                }
                let qualified_name = build_qualified_name(type_stack, &name);
                let visibility = visibility_for_node(&node, content);
                let enum_id = helper.add_enum_with_visibility(
                    &qualified_name,
                    Some(node_to_span(&node)),
                    Some(visibility),
                );

                // Export public/internal top-level enums (not nested)
                if type_stack.is_empty() && is_exported_symbol(&node, content) {
                    export_from_file_module(helper, enum_id);
                }

                process_inheritance(helper, &node, content, type_stack, &qualified_name);
                type_stack.push(name);
                pushed_type = true;
            }
        }
        "extension_declaration" => {
            if let Some(name) = extract_type_name(&node, content) {
                let next_depth = type_stack.len() + 1;
                if next_depth > max_scope_depth {
                    return;
                }
                // For extensions, we don't create a new type but track the extended type
                type_stack.push(name);
                pushed_type = true;
            }
        }
        "function_declaration" => {
            if let Some(name) = extract_function_name(&node, content) {
                let qualified_name = build_qualified_name(type_stack, &name);
                let span = Some(node_to_span(&node));
                let is_async =
                    has_modifier(&node, content, "async") || has_child_kind(&node, "async");
                let is_method = !type_stack.is_empty();
                let visibility = visibility_for_node(&node, content);

                let fn_id = if is_method {
                    helper.add_method_with_visibility(
                        &qualified_name,
                        span,
                        is_async,
                        false,
                        Some(visibility),
                    )
                } else {
                    helper.add_function_with_visibility(
                        &qualified_name,
                        span,
                        is_async,
                        false,
                        Some(visibility),
                    )
                };

                // Process TypeOf and Reference edges for parameters and return type
                process_function_parameters_typeof(
                    node,
                    &qualified_name,
                    is_method,
                    content,
                    helper,
                );
                process_function_return_typeof(node, &qualified_name, is_method, content, helper);

                callable_stack.push((fn_id, qualified_name));
                pushed_callable = true;
            }
        }
        "init_declaration" | "deinit_declaration" => {
            let name = if node.kind() == "init_declaration" {
                "init"
            } else {
                "deinit"
            };
            let qualified_name = build_qualified_name(type_stack, name);
            let span = Some(node_to_span(&node));
            let is_async = has_modifier(&node, content, "async");
            let visibility = visibility_for_node(&node, content);

            let fn_id = helper.add_method_with_visibility(
                &qualified_name,
                span,
                is_async,
                false,
                Some(visibility),
            );
            callable_stack.push((fn_id, qualified_name));
            pushed_callable = true;
        }
        "call_expression" => {
            process_call_expression(
                helper,
                ast_context,
                &node,
                content,
                callable_stack,
                in_await,
            );
        }
        "import_declaration" => {
            process_import_declaration(helper, &node, content);
        }
        "property_declaration" => {
            // Process TypeOf and Reference edges for properties within types
            if !type_stack.is_empty() {
                let qualified_owner = build_qualified_name(type_stack, "");
                let owner_without_trailing_dot = qualified_owner.trim_end_matches('.');
                process_property_typeof_edges(node, content, helper, owner_without_trailing_dot);
            }
        }
        _ => {}
    }

    // Determine if we're inside an await context
    // Note: try_expression is for error handling, not async - only await_expression indicates async calls
    let next_in_await = in_await || node.kind() == "await_expression";

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree(
            helper,
            ast_context,
            child,
            content,
            callable_stack,
            type_stack,
            next_in_await,
            depth + 1,
            max_scope_depth,
        );
    }

    // Pop stacks
    if pushed_callable {
        callable_stack.pop();
    }
    if pushed_type {
        type_stack.pop();
    }
}

// ============================================================================
// Call Expression Processing
// ============================================================================

/// Process a `call_expression` node and create appropriate call edges.
fn process_call_expression(
    helper: &mut GraphBuildHelper,
    ast_context: &ASTContext,
    node: &Node,
    content: &[u8],
    callable_stack: &[(UnifiedNodeId, String)],
    in_await: bool,
) {
    // Get the caller from the stack or from AST context
    let (source_id, caller_name) = if let Some((id, name)) = callable_stack.last() {
        (*id, name.clone())
    } else if let Some(ctx) = ast_context.find_enclosing(node.start_byte()) {
        // Use ensure_method or ensure_function based on the context's is_method flag
        let call_site_span = node_to_span(node);
        let id = if ctx.is_method {
            helper.ensure_method(&ctx.qualified_name, None, ctx.is_async, false)
        } else {
            helper.ensure_callee(
                &ctx.qualified_name,
                call_site_span,
                CalleeKindHint::Function,
            )
        };
        (id, ctx.qualified_name.clone())
    } else {
        // Call at module level - create synthetic module-level context
        let id = helper.add_module("<module>", None);
        (id, "<module>".to_string())
    };

    // Extract callee information
    if let Some(callee_info) = extract_callee_info(node, content) {
        // Count arguments
        let arg_count = count_call_arguments(node);

        // Determine if the call is async (either in await context or calling known async function)
        let is_async = in_await;

        if callee_info.receiver.is_none()
            && SwiftBridgingIndex::is_c_function(&callee_info.name).is_some()
        {
            let ffi_name = format!("C::{}", callee_info.name);
            let ffi_id = helper.add_function(&ffi_name, Some(node_to_span(node)), false, false);
            helper.add_ffi_edge(source_id, ffi_id, FfiConvention::C);
            return;
        }

        // Build qualified callee name and determine if it's a method call
        let (target_qualified, is_method_call) = if let Some(receiver) = &callee_info.receiver {
            // Method call: receiver.method

            // Check if receiver is 'self' - use caller's type context
            if receiver == "self" {
                // Extract type from caller_name (e.g., "ClassName.methodName" -> "ClassName")
                if let Some(dot_pos) = caller_name.rfind('.') {
                    let class_name = &caller_name[..dot_pos];
                    (format!("{}.{}", class_name, callee_info.name), true)
                } else {
                    (callee_info.name.clone(), true)
                }
            } else {
                // External receiver - use as-is or try to resolve
                (format!("{}.{}", receiver, callee_info.name), true)
            }
        } else {
            // Simple function call without explicit receiver
            // Check if this could be an implicit method call (sibling method in same type)
            // Use ASTContext.callable_contexts as a "known definitions" index
            if let Some(dot_pos) = caller_name.rfind('.') {
                // Caller is a method - compute caller's type
                let caller_type = &caller_name[..dot_pos];
                // Build candidate qualified method name
                let candidate = format!("{}.{}", caller_type, callee_info.name);
                // Check if candidate exists in callable_contexts as a method
                let is_known_method = ast_context
                    .callable_contexts
                    .iter()
                    .any(|ctx| ctx.qualified_name == candidate && ctx.is_method);
                if is_known_method {
                    // Implicit receiver call to sibling method
                    (candidate, true)
                } else {
                    // Fall back to unqualified function call
                    (callee_info.name.clone(), false)
                }
            } else {
                // Caller is a top-level function, so callee is also a function
                (callee_info.name.clone(), false)
            }
        };

        // Create callee node and add edge
        // Use ensure_method for method calls, ensure_function otherwise
        let call_span = node_to_span(node);
        let target_id = if is_method_call {
            helper.ensure_method(&target_qualified, None, false, false)
        } else {
            helper.ensure_callee(&target_qualified, call_span, CalleeKindHint::Function)
        };
        let span = Some(call_span);

        helper.add_call_edge_full_with_span(
            source_id,
            target_id,
            arg_count,
            is_async,
            span.into_iter().collect(),
        );
    }
}

// ============================================================================
// Import Declaration Processing
// ============================================================================

/// Process an `import_declaration` node and create appropriate import edges.
///
/// Swift import syntax:
/// - `import Foundation` - imports entire module
/// - `import UIKit.UIView` - imports specific type from module
/// - `import class UIKit.UIViewController` - imports specific kind (class/struct/func/etc)
fn process_import_declaration(helper: &mut GraphBuildHelper, node: &Node, content: &[u8]) {
    // Extract the module name from the import declaration
    // For "import Foundation", we want "Foundation"
    // For "import UIKit.UIView", we want "UIKit" (the module name)
    // For "import class UIKit.UIViewController", we want "UIKit"

    if let Some(module_name) = extract_import_module_name(node, content) {
        let span = Some(node_to_span(node));

        // Create module node for the current file
        let file_path = helper.file_path().to_string();
        let from_id = helper.add_module(&file_path, None);

        // Create import node for the imported module
        let to_id = helper.add_import(&module_name, span);

        // Add import edge
        helper.add_import_edge(from_id, to_id);
    }
}

/// Extract the module name from an `import_declaration` node.
///
/// Examples:
/// - `import Foundation` -> "Foundation"
/// - `import UIKit.UIView` -> "`UIKit`"
/// - `import class UIKit.UIViewController` -> "`UIKit`"
fn extract_import_module_name(node: &Node, content: &[u8]) -> Option<String> {
    // The import_declaration node structure in tree-sitter-swift typically has:
    // - Optional import kind (class, struct, func, etc.)
    // - One or more identifiers representing the module path

    // First, try to get the full text and parse it
    if let Ok(import_text) = node.utf8_text(content) {
        let text = import_text.trim();

        // Remove "import " prefix
        let after_import = text.strip_prefix("import")?.trim();

        // Remove optional kind keywords (class, struct, func, etc.)
        let after_kind = after_import
            .strip_prefix("class")
            .unwrap_or(after_import)
            .trim()
            .strip_prefix("struct")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("enum")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("protocol")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("typealias")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("func")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("var")
            .unwrap_or_else(|| after_import.trim())
            .trim()
            .strip_prefix("let")
            .unwrap_or_else(|| after_import.trim())
            .trim();

        // Extract the first component (module name)
        // For "Foundation", we get "Foundation"
        // For "UIKit.UIView", we get "UIKit"
        let module_name = after_kind.split('.').next()?.trim();

        if !module_name.is_empty() {
            return Some(module_name.to_string());
        }
    }

    // Fallback: try to extract from child nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Look for identifier nodes
        if (child.kind() == "simple_identifier" || child.kind() == "identifier")
            && let Ok(text) = child.utf8_text(content)
        {
            let text = text.trim();
            if !text.is_empty() && text != "import" {
                return Some(text.to_string());
            }
        }
    }

    None
}

// ============================================================================
// Call Expression Processing
// ============================================================================

/// Information extracted from a callee expression.
#[derive(Debug)]
struct CalleeInfo {
    /// The method/function name being called
    name: String,
    /// The receiver object (if method call), e.g., "self", "obj", "Type"
    receiver: Option<String>,
}

/// Extract callee information from a `call_expression` node.
fn extract_callee_info(call_node: &Node, content: &[u8]) -> Option<CalleeInfo> {
    // tree-sitter-swift represents a call as:
    // (call_expression <callee_expr> (call_suffix ...))
    // The callee expression is the first child that isn't a call suffix.
    let callee_expr = find_call_callee_node(call_node)?;

    match callee_expr.kind() {
        "simple_identifier" => {
            // Simple function call: foo()
            let name = callee_expr.utf8_text(content).ok()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(CalleeInfo {
                name,
                receiver: None,
            })
        }
        "navigation_expression" => {
            // Method call: receiver.method
            extract_navigation_callee(&callee_expr, content)
        }
        "call_expression" => {
            // Chained call: foo().bar() - extract the inner callee
            // The result of foo() is the receiver for bar()
            // We'll record both calls separately during tree walk
            extract_callee_info(&callee_expr, content)
        }
        _ => {
            // Try to extract text as fallback
            let raw = callee_expr.utf8_text(content).ok()?.trim();
            if raw.is_empty() {
                return None;
            }

            // Handle patterns like Optional.none, Type.staticMethod
            if let Some(dot_pos) = raw.rfind('.') {
                let receiver = raw[..dot_pos].trim();
                let name = raw[dot_pos + 1..].trim();

                // Clean up generic parameters: Type<T>.method -> Type, method
                let name = name.split('<').next().unwrap_or(name);
                let name = name.split('(').next().unwrap_or(name);
                let name = name.trim_matches(|c: char| c == '?' || c == '!');

                if name.is_empty() {
                    return None;
                }

                let receiver = receiver.split('<').next().unwrap_or(receiver);

                Some(CalleeInfo {
                    name: name.to_string(),
                    receiver: Some(receiver.to_string()),
                })
            } else {
                // Simple identifier or complex expression
                let name = raw.split('(').next().unwrap_or(raw);
                let name = name.split('<').next().unwrap_or(name);
                let name = name.trim_matches(|c: char| c == '?' || c == '!');

                if name.is_empty() {
                    return None;
                }

                Some(CalleeInfo {
                    name: name.to_string(),
                    receiver: None,
                })
            }
        }
    }
}

fn find_call_callee_node<'a>(call_node: &'a Node<'a>) -> Option<Node<'a>> {
    let mut cursor = call_node.walk();
    for child in call_node.children(&mut cursor) {
        if is_call_suffix_kind(child.kind()) {
            continue;
        }
        return Some(child);
    }

    call_node.named_child(0)
}

/// Extract callee information from a `navigation_expression` (receiver.method).
fn extract_navigation_callee(node: &Node, content: &[u8]) -> Option<CalleeInfo> {
    // navigation_expression has structure: <receiver> "." <suffix>
    // where suffix is typically a navigation_suffix containing an identifier

    let mut receiver_text: Option<String> = None;
    let mut method_name: Option<String> = None;

    // Use named_children to ignore token nodes (like ".") and improve robustness
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "simple_identifier" => {
                // Could be receiver or method depending on position
                if receiver_text.is_none() {
                    receiver_text = child.utf8_text(content).ok().map(|s| s.trim().to_string());
                } else {
                    method_name = child.utf8_text(content).ok().map(|s| s.trim().to_string());
                }
            }
            "navigation_suffix" => {
                // Contains the method name
                for suffix_child in child.children(&mut child.walk()) {
                    if suffix_child.kind() == "simple_identifier" {
                        method_name = suffix_child
                            .utf8_text(content)
                            .ok()
                            .map(|s| s.trim().to_string());
                        break;
                    }
                }
            }
            "self_expression" => {
                receiver_text = Some("self".to_string());
            }
            "navigation_expression" => {
                // Nested navigation: a.b.c - the inner navigation is the receiver
                if let Some(inner) = extract_navigation_callee(&child, content) {
                    receiver_text = Some(if let Some(r) = inner.receiver {
                        format!("{}.{}", r, inner.name)
                    } else {
                        inner.name
                    });
                }
            }
            "call_expression" => {
                // Chained: foo().bar - the call result is the receiver
                // Use a stable sentinel instead of embedding "()" which pollutes node names
                receiver_text = Some("<call_result>".to_string());
            }
            // Non-stable receiver types that should use sentinel to avoid node pollution
            "subscript_expression"
            | "tuple_expression"
            | "array_literal"
            | "dictionary_literal"
            | "ternary_expression"
            | "as_expression"
            | "try_expression"
            | "await_expression"
            | "prefix_expression"
            | "postfix_expression"
            | "infix_expression"
            | "parenthesized_expression" => {
                // Non-identifier receivers - use stable sentinel
                receiver_text = Some("<call_result>".to_string());
            }
            _ => {
                // For unknown node kinds, only accept identifier-like text
                // to avoid embedding complex expressions in node names
                if receiver_text.is_none() {
                    let text = child.utf8_text(content).ok()?.trim();
                    // Only accept simple identifier-like receivers (alphanumeric + underscore)
                    // Reject anything with parens, brackets, operators, etc.
                    let is_simple_identifier = !text.is_empty()
                        && !text.starts_with('.')
                        && !text.contains('(')
                        && !text.contains('[')
                        && !text.contains('{')
                        && !text.contains(' ')
                        && !text.contains('+')
                        && !text.contains('-')
                        && !text.contains('*')
                        && !text.contains('/');
                    if is_simple_identifier {
                        receiver_text = Some(text.to_string());
                    } else if !text.is_empty() {
                        // Complex expression - use sentinel
                        receiver_text = Some("<call_result>".to_string());
                    }
                }
            }
        }
    }

    // If we only got receiver, try extracting method from the full node text
    if method_name.is_none() && receiver_text.is_some() {
        let full_text = node.utf8_text(content).ok()?;
        if let Some(dot_pos) = full_text.rfind('.') {
            let after_dot = full_text[dot_pos + 1..].trim();
            let name = after_dot.split('(').next().unwrap_or(after_dot);
            let name = name.split('<').next().unwrap_or(name);
            let name = name.trim_matches(|c: char| c == '?' || c == '!');
            if !name.is_empty() {
                method_name = Some(name.to_string());
            }
        }
    }

    let name = method_name?;
    if name.is_empty() {
        return None;
    }

    Some(CalleeInfo {
        name,
        receiver: receiver_text,
    })
}

/// Count the number of arguments in a call expression.
fn count_call_arguments(call_node: &Node) -> u8 {
    let mut cursor = call_node.walk();
    for child in call_node.children(&mut cursor) {
        if is_call_suffix_kind(child.kind()) {
            // call_suffix contains value_arguments
            for suffix_child in child.children(&mut child.walk()) {
                if suffix_child.kind() == "value_arguments" {
                    let count = suffix_child
                        .children(&mut suffix_child.walk())
                        .filter(|c| c.kind() == "value_argument")
                        .count();
                    return u8::try_from(count.min(254)).unwrap_or(u8::MAX);
                }
            }
        }
    }

    // Try direct child value_arguments (some grammar versions)
    for child in call_node.children(&mut cursor) {
        if child.kind() == "value_arguments" {
            let count = child
                .children(&mut child.walk())
                .filter(|c| c.kind() == "value_argument")
                .count();
            return u8::try_from(count.min(254)).unwrap_or(u8::MAX);
        }
    }

    // Unknown argument count
    255
}

fn is_call_suffix_kind(kind: &str) -> bool {
    kind.ends_with("call_suffix")
}

// ============================================================================
// Inheritance/Conformance Processing
// ============================================================================

/// Process inheritance and protocol conformance for a type declaration.
fn process_inheritance(
    helper: &mut GraphBuildHelper,
    node: &Node,
    content: &[u8],
    type_stack: &[String],
    child_qualified_name: &str,
) {
    // Look for inheritance_specifier or type_inheritance_clause
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_inheritance_clause" || child.kind().contains("inherit") {
            // Extract parent types
            for inherit_child in child.children(&mut child.walk()) {
                let is_inheritance_node = inherit_child.kind() == "inheritance_specifier"
                    || inherit_child.kind() == "user_type"
                    || inherit_child.kind() == "type_identifier";

                if is_inheritance_node && let Ok(parent_text) = inherit_child.utf8_text(content) {
                    let parent_text = parent_text.trim();
                    // Clean up generic parameters
                    let parent_name = parent_text.split('<').next().unwrap_or(parent_text);
                    if !parent_name.is_empty() {
                        // Create parent node and add inheritance edge
                        let parent_qualified = build_qualified_name(type_stack, parent_name);

                        // Determine if this is a class inheritance or protocol conformance
                        // In Swift, the first item in inheritance list for a class is the superclass
                        // and the rest are protocols. For structs/enums, all are protocols.
                        let child_id = helper.add_class(child_qualified_name, None);
                        let parent_id = helper.add_class(&parent_qualified, None);

                        // For simplicity, use inherits for all (sqry doesn't distinguish)
                        helper.add_inherits_edge(child_id, parent_id);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract the name from a type declaration (class, struct, enum, protocol, extension).
fn extract_type_name(node: &Node, content: &[u8]) -> Option<String> {
    // Try field-based access first
    if let Some(name_node) = node.child_by_field_name("name") {
        return name_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string());
    }

    // Try finding type_identifier child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let is_type_identifier =
            child.kind() == "type_identifier" || child.kind() == "simple_identifier";

        if is_type_identifier && let Ok(text) = child.utf8_text(content) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }

        // For extension, look for user_type
        if child.kind() == "user_type"
            && let Ok(text) = child.utf8_text(content)
        {
            let text = text.trim().split('<').next().unwrap_or(text.trim());
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }

    None
}

/// Extract the name from a function declaration.
fn extract_function_name(node: &Node, content: &[u8]) -> Option<String> {
    // Try field-based access first
    if let Some(name_node) = node.child_by_field_name("name") {
        return name_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string());
    }

    // Try finding simple_identifier child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_identifier"
            && let Ok(text) = child.utf8_text(content)
        {
            let text = text.trim();
            if !text.is_empty() && text != "func" {
                return Some(text.to_string());
            }
        }
    }

    None
}

/// Build a qualified name from type stack and member name.
fn build_qualified_name(type_stack: &[String], member: &str) -> String {
    if type_stack.is_empty() {
        member.to_string()
    } else {
        format!("{}.{}", type_stack.join("."), member)
    }
}

/// Check if a node has a modifier in its modifiers children.
fn has_modifier(node: &Node, content: &[u8], modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            for mod_child in child.children(&mut child.walk()) {
                if let Ok(text) = mod_child.utf8_text(content)
                    && text.trim() == modifier
                {
                    return true;
                }
            }
        }
        // Also check direct children (some modifiers appear at this level)
        if let Ok(text) = child.utf8_text(content)
            && text.trim() == modifier
        {
            return true;
        }
    }
    false
}

/// Check if a node has a child with a specific kind.
fn has_child_kind(node: &Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return true;
        }
    }
    false
}

/// Convert a tree-sitter node to a Span.
fn node_to_span(node: &Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

/// Check if a symbol is exported (public or internal, not private/fileprivate).
///
/// Swift visibility modifiers:
/// - `public`: visible across modules
/// - `internal`: visible within the module (default if no modifier)
/// - `fileprivate`: visible within the file only
/// - `private`: visible within the scope only
fn is_exported_symbol(node: &Node, content: &[u8]) -> bool {
    // Check for explicit visibility modifiers
    if has_modifier(node, content, "private") || has_modifier(node, content, "fileprivate") {
        return false;
    }

    // If there's an explicit public or internal modifier, it's exported
    // If there's no visibility modifier at all, Swift defaults to internal (module-scoped), so it's exported
    true
}

/// Normalize Swift visibility to "public" or "private" for graph metadata.
fn visibility_for_node(node: &Node, content: &[u8]) -> &'static str {
    if has_modifier(node, content, "private") || has_modifier(node, content, "fileprivate") {
        "private"
    } else {
        "public"
    }
}

/// Export a symbol from the file module.
///
/// File-level module name for exports.
const FILE_MODULE_NAME: &str = "<file_module>";

fn export_from_file_module(helper: &mut GraphBuildHelper, exported: UnifiedNodeId) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

// ============================================================================
// TypeOf and Reference Edge Processing
// ============================================================================

/// Process top-level variable and constant declarations (Phase 3).
///
/// This function walks the root node to find `property_declaration` nodes at the top level
/// and creates TypeOf/Reference edges for them.
fn process_toplevel_variables(root: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        // Process top-level property declarations (var/let)
        if child.kind() == "property_declaration" {
            process_toplevel_variable(child, content, helper);
        }
    }
}

/// Process a single top-level variable/constant declaration.
fn process_toplevel_variable(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // FIX M-1 (Iteration 3): Pair each pattern with its adjacent type_annotation.
    // Swift can have multiple bindings where each has its own type annotation.
    // Collect pattern+type pairs by iterating children and tracking adjacent nodes.

    struct Binding<'a> {
        names: Vec<(String, Node<'a>)>,
        type_text: String,
        referenced_types: Vec<String>,
    }

    let mut bindings: Vec<Binding> = Vec::new();
    let mut current_names = Vec::new();
    let mut pending_type: Option<Node> = None;

    // Iterate through children to pair patterns with type annotations
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "pattern" | "pattern_binding" => {
                // Extract names from this pattern
                let mut pattern_cursor = child.walk();
                for pattern_child in child.children(&mut pattern_cursor) {
                    if (pattern_child.kind() == "simple_identifier"
                        || pattern_child.kind() == "name")
                        && let Ok(name) = pattern_child.utf8_text(content)
                    {
                        current_names.push((name.to_string(), pattern_child));
                    }
                }
            }
            "type_annotation" => {
                // Found a type annotation - store it as pending
                pending_type = Some(child);

                // If we have current names and a type, create a binding
                if !current_names.is_empty() {
                    if let Some(type_ann) = pending_type {
                        // Find the actual type node within type_annotation
                        let mut type_node = None;
                        let mut type_cursor = type_ann.walk();
                        for type_child in type_ann.children(&mut type_cursor) {
                            if is_swift_type_node(type_child.kind()) {
                                type_node = Some(type_child);
                                break;
                            }
                        }

                        if let Some(type_node) = type_node {
                            let type_text = type_node.utf8_text(content).map_or_else(
                                |_| "<unknown_type>".to_string(),
                                std::string::ToString::to_string,
                            );
                            let referenced_types =
                                crate::relations::extract_type_names_from_swift_type(
                                    type_node, content,
                                );

                            bindings.push(Binding {
                                names: current_names.clone(),
                                type_text,
                                referenced_types,
                            });
                        }
                    }
                    current_names.clear();
                    pending_type = None;
                }
            }
            _ => {}
        }
    }

    // Handle any remaining names with pending type
    if !current_names.is_empty()
        && pending_type.is_some()
        && let Some(type_ann) = pending_type
    {
        let mut type_node = None;
        let mut type_cursor = type_ann.walk();
        for type_child in type_ann.children(&mut type_cursor) {
            if is_swift_type_node(type_child.kind()) {
                type_node = Some(type_child);
                break;
            }
        }

        if let Some(type_node) = type_node {
            let type_text = type_node.utf8_text(content).map_or_else(
                |_| "<unknown_type>".to_string(),
                std::string::ToString::to_string,
            );
            let referenced_types =
                crate::relations::extract_type_names_from_swift_type(type_node, content);

            bindings.push(Binding {
                names: current_names,
                type_text,
                referenced_types,
            });
        }
    }

    // Create TypeOf and Reference edges for each binding
    for binding in bindings {
        for (var_name, name_node) in binding.names {
            let var_id = helper.add_variable(&var_name, Some(node_to_span(&name_node)));

            let type_id = helper.add_type(&binding.type_text, None);
            helper.add_typeof_edge_with_context(
                var_id,
                type_id,
                Some(TypeOfContext::Variable),
                None,
                Some(&var_name),
            );

            for ref_type_name in &binding.referenced_types {
                let ref_type_id = helper.add_type(ref_type_name, None);
                helper.add_reference_edge(var_id, ref_type_id);
            }
        }
    }
}

// ============================================================================
// TypeOf and Reference Edge Processing (Methods/Properties)
// ============================================================================

/// Process property declarations to create `TypeOf` and Reference edges.
///
/// Handles:
/// - `var name: String = "John"` → `TypeOf` edge: name → String
/// - `let count: Int = 42` → `TypeOf` edge: count → Int
/// - `var user: User? = nil` → `TypeOf` edge: user → User?, Reference edge: user → User
/// - `var cache: [String: User]` → `TypeOf` edge + Reference edges: String, User
///
/// This function should be called for `property_declaration` nodes within
/// classes, structs, and enums.
#[allow(clippy::too_many_lines)]
fn process_property_typeof_edges(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    qualified_owner: &str,
) {
    // FIX M-1 (Iteration 3): Pair each pattern with its adjacent type_annotation.
    // Swift can have multiple property bindings where each has its own type annotation.
    // Collect pattern+type pairs by iterating children and tracking adjacent nodes.
    //
    // C2_OTHER_SWIFT (REQ:R0001,R0003,R0004,R0005,R0023):
    // - Branch on `value_binding_pattern` (`let` vs `var`) to emit
    //   Constant vs Property under `Owner.field`.
    // - Read `modifiers` for `static`/`class` (→ is_static = true) and
    //   `visibility_modifier` (→ visibility string). Default visibility
    //   is "internal" — Swift's implicit access level for instance
    //   members. Tuple destructuring (`let (a, b) = ...`) recurses into
    //   nested `pattern` children to capture every bound name.

    struct Binding<'a> {
        names: Vec<(String, Node<'a>)>,
        type_text: String,
        referenced_types: Vec<String>,
    }

    // ---- Phase A: extract is_let / is_static / visibility from this declaration ----
    let mut is_let = true; // Swift declarations always have either let or var; default to let if absent.
    let mut is_static = false;
    // Swift default visibility for instance members is `internal`. Only
    // overridden by an explicit access-control modifier.
    let mut visibility: &str = "internal";

    let mut decl_cursor = node.walk();
    for child in node.children(&mut decl_cursor) {
        match child.kind() {
            "value_binding_pattern" => {
                // `value_binding_pattern` has a single keyword child whose
                // kind is either `let` or `var`.
                let mut vbp_cursor = child.walk();
                for vbp_child in child.children(&mut vbp_cursor) {
                    match vbp_child.kind() {
                        "let" => is_let = true,
                        "var" => is_let = false,
                        _ => {}
                    }
                }
            }
            "modifiers" => {
                let mut mod_cursor = child.walk();
                for mod_child in child.children(&mut mod_cursor) {
                    match mod_child.kind() {
                        "visibility_modifier" => {
                            // Visibility kind is the first identifier child:
                            // public / internal / fileprivate / private / open.
                            //
                            // Per Swift spec, setter access can be lower than
                            // property access via the `(set)` form
                            // (`private(set)`, `fileprivate(set)`,
                            // `internal(set)`). Those modifiers restrict the
                            // setter only — the property's overall visibility
                            // is governed by a separate bare access modifier
                            // (e.g. `public private(set) var limited: Int`
                            // remains a `public` property with a private
                            // setter). We don't model setter visibility
                            // separately yet, so SKIP `(set)` forms here and
                            // fall back to the bare modifier.
                            //
                            // Tree-sitter-swift accepts whitespace inside the
                            // parentheses (`private( set )`, `private ( set )`,
                            // etc.), so a source-text `contains("(set)")`
                            // check is not robust. Instead, walk the
                            // `visibility_modifier`'s children structurally and
                            // detect the `set` token by node kind or trimmed
                            // text — this is whitespace-agnostic.
                            let mut setter_cursor = mod_child.walk();
                            let is_setter_access =
                                mod_child.children(&mut setter_cursor).any(|c| {
                                    c.kind() == "set"
                                        || c.utf8_text(content)
                                            .map(|t| t.trim() == "set")
                                            .unwrap_or(false)
                                });
                            if is_setter_access {
                                continue;
                            }
                            let mut vis_cursor = mod_child.walk();
                            for vis_child in mod_child.children(&mut vis_cursor) {
                                match vis_child.kind() {
                                    "public" => visibility = "public",
                                    "internal" => visibility = "internal",
                                    "fileprivate" => visibility = "fileprivate",
                                    "private" => visibility = "private",
                                    "open" => visibility = "open",
                                    _ => {}
                                }
                            }
                        }
                        "property_modifier" => {
                            // `static` and `class` (class-level overridable) both
                            // mark the property as type-level rather than instance-level.
                            let mut pm_cursor = mod_child.walk();
                            for pm_child in mod_child.children(&mut pm_cursor) {
                                if matches!(pm_child.kind(), "static" | "class") {
                                    is_static = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ---- Phase B: pair patterns with type annotations as before ----
    let mut bindings: Vec<Binding> = Vec::new();
    let mut current_names: Vec<(String, Node)> = Vec::new();
    let mut pending_type: Option<Node> = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "pattern" | "pattern_binding" => {
                // Recurse into nested patterns to support tuple destructuring
                // (`let (a, b) = (1, 2)` → AST: pattern[(] pattern[a] [,] pattern[b] [)]).
                collect_pattern_names(child, content, &mut current_names);
            }
            "type_annotation" => {
                // Found a type annotation - store it as pending
                pending_type = Some(child);

                // If we have current names and a type, create a binding
                if !current_names.is_empty() {
                    if let Some(type_ann) = pending_type {
                        // Find the actual type node within type_annotation
                        let mut type_node = None;
                        let mut type_cursor = type_ann.walk();
                        for type_child in type_ann.children(&mut type_cursor) {
                            if is_swift_type_node(type_child.kind()) {
                                type_node = Some(type_child);
                                break;
                            }
                        }

                        if let Some(type_node) = type_node {
                            let type_text = type_node.utf8_text(content).map_or_else(
                                |_| "<unknown_type>".to_string(),
                                std::string::ToString::to_string,
                            );
                            let referenced_types =
                                crate::relations::extract_type_names_from_swift_type(
                                    type_node, content,
                                );

                            bindings.push(Binding {
                                names: current_names.clone(),
                                type_text,
                                referenced_types,
                            });
                        }
                    }
                    current_names.clear();
                    pending_type = None;
                }
            }
            _ => {}
        }
    }

    // Handle any remaining names with pending type
    if !current_names.is_empty()
        && pending_type.is_some()
        && let Some(type_ann) = pending_type
    {
        let mut type_node = None;
        let mut type_cursor = type_ann.walk();
        for type_child in type_ann.children(&mut type_cursor) {
            if is_swift_type_node(type_child.kind()) {
                type_node = Some(type_child);
                break;
            }
        }

        if let Some(type_node) = type_node {
            let type_text = type_node.utf8_text(content).map_or_else(
                |_| "<unknown_type>".to_string(),
                std::string::ToString::to_string,
            );
            let referenced_types =
                crate::relations::extract_type_names_from_swift_type(type_node, content);

            bindings.push(Binding {
                names: current_names.clone(),
                type_text,
                referenced_types,
            });
            current_names.clear();
        }
    }

    // ---- Phase C: emit Property/Constant nodes + edges ----
    //
    // A property name appears in exactly one of two places:
    //   (a) inside a `Binding` (the declaration has a type annotation), or
    //   (b) leftover in `current_names` (no type annotation, e.g. tuple
    //       destructuring `let (a, b) = (1, 2)`).
    //
    // In case (a) we emit the property node + TypeOf + Reference edges.
    // In case (b) we emit the property node only (no type info to bind).

    // Helper closure: emit a Property or Constant node based on `is_let`.
    let emit_node = |helper: &mut GraphBuildHelper,
                     qualified_property_name: &str,
                     name_node: &Node|
     -> sqry_core::graph::unified::node::NodeId {
        if is_let {
            helper.add_constant_with_static_and_visibility(
                qualified_property_name,
                Some(node_to_span(name_node)),
                is_static,
                Some(visibility),
            )
        } else {
            helper.add_property_with_static_and_visibility(
                qualified_property_name,
                Some(node_to_span(name_node)),
                is_static,
                Some(visibility),
            )
        }
    };

    for binding in bindings {
        for (property_name, name_node) in binding.names {
            let qualified_property_name = format!("{qualified_owner}.{property_name}");
            let property_id = emit_node(helper, &qualified_property_name, &name_node);

            let type_id = helper.add_type(&binding.type_text, None);
            helper.add_typeof_edge_with_context(
                property_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(&property_name),
            );

            for ref_type_name in &binding.referenced_types {
                let ref_type_id = helper.add_type(ref_type_name, None);
                helper.add_reference_edge(property_id, ref_type_id);
            }
        }
    }

    // Names without a binding type (e.g. `let (a, b) = (1, 2)` or `var x = 0`)
    // still need a Property/Constant node so downstream consumers can find them.
    for (property_name, name_node) in current_names {
        let qualified_property_name = format!("{qualified_owner}.{property_name}");
        let _ = emit_node(helper, &qualified_property_name, &name_node);
    }
}

/// Recursively collect identifier names from a Swift `pattern` or
/// `pattern_binding` subtree. Handles flat patterns (`let x: Int`),
/// tuple destructuring (`let (a, b)`), and nested tuples
/// (`let (a, (b, c))`).
fn collect_pattern_names<'a>(pattern: Node<'a>, content: &[u8], out: &mut Vec<(String, Node<'a>)>) {
    let mut cursor = pattern.walk();
    for child in pattern.children(&mut cursor) {
        match child.kind() {
            "simple_identifier" | "name" => {
                if let Ok(name) = child.utf8_text(content) {
                    out.push((name.to_string(), child));
                }
            }
            // Recurse into nested patterns produced by tuple destructuring.
            "pattern" | "pattern_binding" | "tuple_pattern" => {
                collect_pattern_names(child, content, out);
            }
            _ => {}
        }
    }
}

/// Process function/method parameters to create `TypeOf` and Reference edges.
///
/// In Swift's tree-sitter grammar, parameters are direct children of `function_declaration`,
/// not wrapped in a separate "parameters" node like in other languages.
fn process_function_parameters_typeof(
    func_node: Node,
    func_name: &str,
    is_method: bool,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Swift parameters are direct children of the function node
    let mut cursor = func_node.walk();
    let mut param_index = 0;

    for child in func_node.children(&mut cursor) {
        if child.kind() == "parameter" {
            process_single_parameter_typeof(
                func_name,
                is_method,
                child,
                param_index,
                content,
                helper,
            );
            param_index += 1;
        }
    }
}

/// Process a single function/method parameter.
fn process_single_parameter_typeof(
    func_name: &str,
    is_method: bool,
    param_node: Node,
    index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract parameter name (may be None for anonymous parameters)
    // Handle Swift's external/internal parameter label patterns:
    // - `name` field: internal parameter name (used in function body)
    // - `external_name` field: external label (used at call site)
    // - `first_name` / `second_name`: alternative names in some patterns
    // - `_` indicates no external label (only internal name)
    let param_name = param_node
        .child_by_field_name("name")
        .or_else(|| param_node.child_by_field_name("second_name"))
        .and_then(|n| {
            let text = n.utf8_text(content).ok()?;
            // Skip wildcard placeholders
            if text == "_" { None } else { Some(text) }
        })
        .or_else(|| {
            // Try external_name or first_name (Swift parameter labels)
            param_node
                .child_by_field_name("external_name")
                .or_else(|| param_node.child_by_field_name("first_name"))
                .and_then(|n| {
                    let text = n.utf8_text(content).ok()?;
                    if text == "_" { None } else { Some(text) }
                })
        })
        .map(str::to_string);

    // Swift parameters have a direct "type" field (not "type_annotation")
    let Some(type_node) = param_node.child_by_field_name("type") else {
        return;
    };

    // Get the full type text for TypeOf edge
    let type_text = type_node.utf8_text(content).map_or_else(
        |_| "<unknown_type>".to_string(),
        std::string::ToString::to_string,
    );

    // Extract referenced types
    let referenced_types = crate::relations::extract_type_names_from_swift_type(type_node, content);

    // Get or create function/method node based on context
    // CRITICAL FIX (H-1): Use ensure_* helpers to reference existing nodes, not create duplicates
    let func_id = if is_method {
        helper.ensure_method(func_name, None, false, false)
    } else {
        helper.ensure_callee(
            func_name,
            node_to_span(&param_node),
            CalleeKindHint::Function,
        )
    };

    // Create TypeOf edge with Parameter context
    let type_id = helper.add_type(&type_text, None);
    // Parameter indices are always small (< 255 in practice), so this cast is safe
    #[allow(clippy::cast_possible_truncation)]
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(index as u16),
        param_name.as_deref(),
    );

    // Create Reference edges
    for ref_type_name in &referenced_types {
        let ref_type_id = helper.add_type(ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Process function/method return type to create `TypeOf` and Reference edges.
fn process_function_return_typeof(
    func_node: Node,
    func_name: &str,
    is_method: bool,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // FIX M-2: Use field-based return type parsing instead of token scanning
    // Try to find return type using grammar fields first (more robust for async/throws)
    let return_type_node = func_node
        .child_by_field_name("result")
        .or_else(|| func_node.child_by_field_name("return_type"))
        .or_else(|| {
            // Fallback: Look for return type after "->" in function signature
            // Swift return types come after "->" in the function declaration
            let mut cursor = func_node.walk();
            let mut found_arrow = false;
            let mut result_type = None;

            for child in func_node.children(&mut cursor) {
                // Look for arrow operator or directly for type nodes after parameters
                if child.kind() == "->" || child.utf8_text(content).unwrap_or("") == "->" {
                    found_arrow = true;
                } else if found_arrow && is_swift_type_node(child.kind()) {
                    result_type = Some(child);
                    break;
                } else if child.kind() == "function_type" {
                    // For function types, look for the return type inside
                    let mut ft_cursor = child.walk();
                    for ft_child in child.children(&mut ft_cursor) {
                        if is_swift_type_node(ft_child.kind())
                            && ft_child.start_byte() > child.start_byte()
                        {
                            result_type = Some(ft_child);
                            break;
                        }
                    }
                }
            }
            result_type
        });

    let Some(type_node) = return_type_node else {
        // No return type (void function)
        return;
    };

    // Get the full type text for TypeOf edge
    let type_text = type_node.utf8_text(content).map_or_else(
        |_| "<unknown_type>".to_string(),
        std::string::ToString::to_string,
    );

    // Extract referenced types
    let referenced_types = crate::relations::extract_type_names_from_swift_type(type_node, content);

    // Get or create function/method node based on context
    // CRITICAL FIX (H-1): Use ensure_* helpers to reference existing nodes, not create duplicates
    let func_id = if is_method {
        helper.ensure_method(func_name, None, false, false)
    } else {
        helper.ensure_callee(
            func_name,
            node_to_span(&func_node),
            CalleeKindHint::Function,
        )
    };

    // Create TypeOf edge with Return context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0), // Return type always at index 0
        None,
    );

    // Create Reference edges
    for ref_type_name in &referenced_types {
        let ref_type_id = helper.add_type(ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Check if a node kind represents a Swift type node.
fn is_swift_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "simple_identifier"
            | "type_identifier"
            | "user_type"
            | "optional_type"
            | "array_type"
            | "dictionary_type"
            | "tuple_type"
            | "function_type"
            | "protocol_composition_type"
            | "metatype_type"
            | "metatype"  // FIX M-2 (Iteration 3): Handle both metatype node kinds
            | "some_type"
            | "any_type"
            | "attributed_type"
            | "implicitly_unwrapped_optional_type"
            | "opaque_type"
            | "existential_type"
    )
}

/// Per-language [`ShapeMapping`] for Swift: a precomputed `kind_id -> CfBucket`
/// table over the tree-sitter-swift grammar, shared process-wide via
/// [`swift_shape_mapping`]. Mirrors the C reference impl (a single array index
/// per node on the hot shape walk, identifier-blind throughout).
pub struct SwiftShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl SwiftShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
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
                *slot = cf_bucket_for_swift_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for SwiftShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // Swift's `function_declaration` carries its `parameter` nodes as direct
        // named children (the parameter list is not a wrapper node), so count them
        // structurally.
        let mut cursor = fn_node.walk();
        for child in fn_node.named_children(&mut cursor) {
            if child.kind() == "parameter" {
                shape.arity_positional = shape.arity_positional.saturating_add(1);
            }
        }
        // A `default_value` field on the declaration witnesses at least one
        // default-valued parameter; `return_type` witnesses an explicit return
        // annotation. Both are structural facts, never identifier text.
        shape.has_defaults = fn_node.child_by_field_name("default_value").is_some();
        shape.has_return_annotation = fn_node.child_by_field_name("return_type").is_some();
        shape
    }
}

/// Map one tree-sitter-swift grammar node-kind name to its canonical control-flow
/// bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_swift_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "guard_statement" | "ternary_expression" => CfBucket::Branch,
        "for_statement" | "while_statement" | "repeat_while_statement" => CfBucket::Loop,
        "switch_statement" | "switch_entry" => CfBucket::Match,
        // `do { } catch { }` is the block-level try; `try` postfix on an
        // expression also maps onto the try bucket.
        "do_statement" | "try_expression" => CfBucket::Try,
        "catch_block" => CfBucket::Catch,
        // Swift represents `throw` with a dedicated `throw_keyword` named node.
        "throw_keyword" => CfBucket::Throw,
        "await_expression" => CfBucket::Await,
        // `control_transfer_statement` covers return / break / continue /
        // fallthrough as a single node; the grammar does not disambiguate them, so
        // they share the break/continue bucket.
        "control_transfer_statement" => CfBucket::BreakContinue,
        "call_expression" | "macro_invocation" => CfBucket::Call,
        "assignment" | "property_declaration" => CfBucket::Assign,
        "lambda_literal" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Swift shape mapping, built once on first use.
#[must_use]
pub fn swift_shape_mapping() -> &'static SwiftShapeMapping {
    static MAPPING: OnceLock<SwiftShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(SwiftShapeMapping::build)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::assert_has_call_edge;
    use sqry_core::graph::unified::edge::EdgeKind;
    use std::path::PathBuf;
    use tree_sitter::Parser;

    fn parse_swift(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("error loading Swift grammar");
        parser.parse(source, None).expect("swift parse failed")
    }

    fn count_edges_of_kind(
        staging: &StagingGraph,
        kind_matcher: impl Fn(&EdgeKind) -> bool,
    ) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                if let StagingOp::AddEdge { kind, .. } = op {
                    kind_matcher(kind)
                } else {
                    false
                }
            })
            .count()
    }

    fn has_call_edge(staging: &StagingGraph) -> bool {
        count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Calls { .. })) > 0
    }

    fn count_call_edges(staging: &StagingGraph) -> usize {
        count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Calls { .. }))
    }

    fn has_async_call_edge(staging: &StagingGraph) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::Calls { is_async, .. },
                ..
            } = op
            {
                *is_async
            } else {
                false
            }
        })
    }

    #[test]
    fn test_swift_graph_builder_default() {
        let builder = SwiftGraphBuilder::default();
        assert_eq!(builder.language(), Language::Swift);
    }

    #[test]
    fn test_simple_function_call() {
        let source = r#"
func greet() {
    print("Hello")
}

func main() {
    greet()
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(
            has_call_edge(&staging),
            "Should detect simple function call"
        );
        // main -> greet, greet -> print
        assert!(
            count_call_edges(&staging) >= 2,
            "Should have at least 2 call edges"
        );
    }

    #[test]
    fn test_method_call_on_object() {
        let source = r#"
class User {
    func getName() -> String {
        return "Alice"
    }
}

func process(user: User) {
    let name = user.getName()
    print(name)
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(
            has_call_edge(&staging),
            "Should detect method call on object"
        );
    }

    #[test]
    fn test_self_method_call() {
        let source = r"
class Calculator {
    func add(_ a: Int, _ b: Int) -> Int {
        return a + b
    }

    func calculate() {
        let result = self.add(1, 2)
        print(result)
    }
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect self method call");
    }

    #[test]
    fn test_async_await_call() {
        let source = r#"
func fetchData() async -> String {
    return "data"
}

func process() async {
    let data = await fetchData()
    print(data)
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect async call");
        assert!(
            has_async_call_edge(&staging),
            "Should mark call as async when using await"
        );
    }

    #[test]
    fn test_chained_method_calls() {
        let source = r#"
class Builder {
    func setName(_ name: String) -> Builder {
        return self
    }

    func setAge(_ age: Int) -> Builder {
        return self
    }

    func build() -> String {
        return "built"
    }
}

func createUser() {
    let result = Builder().setName("Alice").setAge(30).build()
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should detect multiple calls in chain
        assert!(
            count_call_edges(&staging) >= 3,
            "Should detect chained method calls"
        );
    }

    #[test]
    fn test_static_method_call() {
        let source = r#"
class Mailer {
    static func deliver(_ message: String) {
        print(message)
    }
}

func send() {
    Mailer.deliver("Hello")
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect static method call");
    }

    #[test]
    fn test_class_with_init() {
        let source = r#"
class Person {
    var name: String

    init(name: String) {
        self.name = name
    }

    func greet() {
        print("Hello, \(name)")
    }
}

func main() {
    let person = Person(name: "Alice")
    person.greet()
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(
            has_call_edge(&staging),
            "Should detect init and method calls"
        );
    }

    #[test]
    fn test_protocol_extension_method_calls() {
        let source = r"
protocol DataProcessor {
    func process() -> String
}

extension DataProcessor {
    func validate() -> Bool {
        let result = process()
        return !result.isEmpty
    }
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // validate() should call process() and isEmpty
        assert!(
            has_call_edge(&staging),
            "Should detect calls in protocol extension"
        );
    }

    #[test]
    fn test_nested_function_calls() {
        let source = r"
func outer(_ value: Int) -> Int {
    return value * 2
}

func inner(_ value: Int) -> Int {
    return value + 1
}

func compute() {
    let result = outer(inner(5))
    print(result)
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // compute -> outer, compute -> inner, compute -> print
        assert!(
            count_call_edges(&staging) >= 3,
            "Should detect nested function calls"
        );
    }

    #[test]
    fn test_closure_with_call() {
        let source = r#"
func process(completion: () -> Void) {
    completion()
}

func main() {
    process {
        print("Done")
    }
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect calls with closures");
    }

    #[test]
    fn test_struct_method_call() {
        let source = r"
struct Point {
    var x: Int
    var y: Int

    func distance() -> Double {
        return sqrt(Double(x * x + y * y))
    }
}

func main() {
    let p = Point(x: 3, y: 4)
    let d = p.distance()
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect struct method calls");
    }

    #[test]
    fn test_enum_with_method() {
        let source = r#"
enum Status {
    case active
    case inactive

    func description() -> String {
        switch self {
        case .active: return "Active"
        case .inactive: return "Inactive"
        }
    }
}

func showStatus(status: Status) {
    print(status.description())
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(has_call_edge(&staging), "Should detect enum method calls");
    }

    #[test]
    fn test_optional_chaining_call() {
        let source = r#"
class Manager {
    func process() -> String {
        return "processed"
    }
}

func work(manager: Manager?) {
    let result = manager?.process()
    print(result ?? "none")
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(
            has_call_edge(&staging),
            "Should detect optional chaining calls"
        );
    }

    #[test]
    fn test_multiple_argument_call() {
        let source = r"
func combine(_ a: Int, _ b: Int, _ c: Int) -> Int {
    return a + b + c
}

func main() {
    let result = combine(1, 2, 3)
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Check that argument count is tracked (should be 3)
        let has_3_args = staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::Calls { argument_count, .. },
                ..
            } = op
            {
                *argument_count == 3
            } else {
                false
            }
        });

        assert!(has_3_args, "Should track argument count (expected 3)");
    }

    #[test]
    fn test_generic_method_call() {
        let source = r"
class Container<T> {
    var value: T

    init(_ value: T) {
        self.value = value
    }

    func get() -> T {
        return value
    }
}

func main() {
    let container = Container<Int>(42)
    let value = container.get()
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        assert!(
            has_call_edge(&staging),
            "Should detect generic method calls"
        );
    }

    #[test]
    fn test_implicit_receiver_resolves_to_class_method_when_exists() {
        // Tests that when foo() is called from within a method without self.,
        // and Type.foo exists in the same file, it's resolved as Type.foo method call.
        let source = r"
class Calculator {
    func add(_ a: Int, _ b: Int) -> Int {
        return a + b
    }

    func multiply(_ a: Int, _ b: Int) -> Int {
        return a * b
    }

    func calculate(_ a: Int, _ b: Int) -> Int {
        // Implicit receiver call - should resolve to Calculator.add
        let sum = add(a, b)
        // Explicit receiver call - clearly Calculator.multiply
        let product = self.multiply(a, b)
        return sum + product
    }
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should have call edges for both add() and multiply()
        assert!(
            has_call_edge(&staging),
            "Should detect method calls (both implicit and explicit receiver)"
        );

        // Verify that we captured multiple call edges
        let call_edge_count = count_call_edges(&staging);

        // Should have at least 2 calls: add() and multiply()
        assert!(
            call_edge_count >= 2,
            "Should have at least 2 call edges for add() and multiply(), got {call_edge_count}"
        );

        // Production-grade: Assert the qualified callee names to ensure implicit
        // receiver resolution produces the correct method references
        // calculate -> Calculator.add (implicit receiver resolved to class method)
        assert_has_call_edge(&staging, "Calculator::calculate", "Calculator::add");
        // calculate -> Calculator::multiply (explicit self.multiply)
        assert_has_call_edge(&staging, "Calculator::calculate", "Calculator::multiply");
    }

    #[test]
    fn test_implicit_receiver_resolves_to_method_not_function() {
        // More specific test: verify that implicit calls from within a class method
        // are resolved as method calls (with the class prefix) when the method exists
        let source = r#"
class MyClass {
    func helper() {
        print("helper")
    }

    func doWork() {
        // This should be resolved as MyClass.helper (method) not just helper (function)
        helper()
    }
}

// Top-level function with the same name should NOT be confused
func helper() {
    print("top-level helper")
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should detect the call
        assert!(
            has_call_edge(&staging),
            "Should detect implicit method call"
        );

        // The call from doWork() to helper() should exist
        let call_edge_count = count_call_edges(&staging);

        assert!(
            call_edge_count >= 1,
            "Should have at least 1 call edge for helper()"
        );

        // Production-grade: Assert that the implicit call resolves to the METHOD
        // (MyClass.helper) not the top-level FUNCTION (helper).
        // This is the critical assertion that locks in correct shadowing behavior.
        assert_has_call_edge(&staging, "MyClass::doWork", "MyClass::helper");
    }

    #[test]
    fn test_simple_import() {
        let source = "import Foundation\n\nfunc greet() {\n    print(\"Hello\")\n}\n";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Check that we have an import edge
        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });

        assert!(has_import, "Should detect import statement");

        // Verify the import node was created
        let has_import_node = staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(
                    entry.kind,
                    sqry_core::graph::unified::node::NodeKind::Import
                )
            } else {
                false
            }
        });

        assert!(
            has_import_node,
            "Should create import node for Foundation module"
        );
    }

    #[test]
    fn test_import_with_submodule() {
        let source = "import UIKit.UIView\n\nclass MyView {\n    func render() {}\n}\n";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Check that we have an import edge
        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });

        assert!(has_import, "Should detect import with submodule");

        // Verify an import node was created
        let has_import_node = staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(
                    entry.kind,
                    sqry_core::graph::unified::node::NodeKind::Import
                )
            } else {
                false
            }
        });

        assert!(
            has_import_node,
            "Should create import node for UIKit module"
        );
    }

    #[test]
    fn test_import_with_kind() {
        let source =
            "import class UIKit.UIViewController\n\nclass MyController {\n    func load() {}\n}\n";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Check that we have an import edge
        let has_import = staging.operations().iter().any(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });

        assert!(has_import, "Should detect import with kind qualifier");

        // Verify an import node was created
        let has_import_node = staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(
                    entry.kind,
                    sqry_core::graph::unified::node::NodeKind::Import
                )
            } else {
                false
            }
        });

        assert!(
            has_import_node,
            "Should create import node for UIKit module"
        );
    }

    #[test]
    fn test_multiple_imports() {
        let source = "import Foundation\nimport UIKit\nimport SwiftUI\n\nfunc main() {\n    print(\"Hello\")\n}\n";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Count import edges
        let import_edge_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Imports { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            import_edge_count, 3,
            "Should detect all 3 import statements"
        );

        // Count import nodes
        let import_node_count = staging
            .operations()
            .iter()
            .filter(|op| {
                if let StagingOp::AddNode { entry, .. } = op {
                    matches!(
                        entry.kind,
                        sqry_core::graph::unified::node::NodeKind::Import
                    )
                } else {
                    false
                }
            })
            .count();

        assert_eq!(import_node_count, 3, "Should create 3 import nodes");
    }

    // ============================================================================
    // Export Tests
    // ============================================================================

    fn count_export_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn has_export_edge(staging: &StagingGraph) -> bool {
        count_export_edges(staging) > 0
    }

    #[test]
    fn test_export_public_function() {
        let source = r#"
public func greet(name: String) -> String {
    return "Hello, \(name)!"
}

private func privateHelper() -> Int {
    return 42
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should have at least one export edge (for public function)
        assert!(has_export_edge(&staging), "Should export public function");

        // Should have exactly 1 export (greet, not privateHelper)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only public function"
        );
    }

    #[test]
    fn test_export_public_class() {
        let source = r"
public class User {
    private var name: String

    public init(name: String) {
        self.name = name
    }

    public func getName() -> String {
        return name
    }
}

private class PrivateHelper {
    func help() {}
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export public class (but not private class)
        assert!(has_export_edge(&staging), "Should export public class");

        // Should have exactly 1 export edge (User class only)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only public class"
        );
    }

    #[test]
    fn test_export_public_protocol() {
        let source = r"
public protocol Repository {
    func save(item: String)
    func findById(id: Int) -> String?
}

private protocol PrivateProtocol {
    func process()
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export public protocol
        assert!(has_export_edge(&staging), "Should export public protocol");

        // Should have exactly 1 export (Repository only)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only public protocol"
        );
    }

    #[test]
    fn test_export_public_struct() {
        let source = r"
public struct UserData {
    public var id: Int
    public var name: String

    public init(id: Int, name: String) {
        self.id = id
        self.name = name
    }
}

private struct PrivateData {
    var value: Int
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export public struct
        assert!(has_export_edge(&staging), "Should export public struct");

        // Should have exactly 1 export (UserData only)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only public struct"
        );
    }

    #[test]
    fn test_export_public_enum() {
        let source = r#"
public enum Status {
    case active
    case inactive

    public func description() -> String {
        switch self {
        case .active: return "Active"
        case .inactive: return "Inactive"
        }
    }
}

private enum PrivateStatus {
    case ok
    case error
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export public enum
        assert!(has_export_edge(&staging), "Should export public enum");

        // Should have exactly 1 export (Status only)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only public enum"
        );
    }

    #[test]
    fn test_export_internal_symbols_by_default() {
        // Swift defaults to internal visibility (module-scoped) when no modifier is present
        let source = r#"
func internalFunction() {
    print("internal")
}

class InternalClass {
    func method() {}
}

struct InternalStruct {
    var value: Int
}

enum InternalEnum {
    case one
    case two
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Swift defaults to internal (module-scoped), which should be exported
        assert!(
            has_export_edge(&staging),
            "Should export internal symbols by default"
        );

        // Should export: function, class, struct, enum (4 total)
        let export_count = count_export_edges(&staging);
        assert!(
            export_count >= 4,
            "Should export all internal symbols (expected at least 4, got {export_count})"
        );
    }

    #[test]
    fn test_no_export_for_private_symbols() {
        let source = r#"
private func privateFunction() {
    print("private")
}

fileprivate class FileprivateClass {
    func method() {}
}
"#;

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should not export private or fileprivate symbols
        assert_eq!(
            count_export_edges(&staging),
            0,
            "Should not export private or fileprivate symbols"
        );
    }

    #[test]
    fn test_no_export_for_nested_types() {
        let source = r"
public class Outer {
    public class Inner {
        public func innerMethod() {}
    }

    public func outerMethod() {}
}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should only export top-level Outer class, not nested Inner class
        assert!(has_export_edge(&staging), "Should export top-level class");

        // Should have exactly 1 export (Outer only, not Inner)
        assert_eq!(
            count_export_edges(&staging),
            1,
            "Should export only top-level class"
        );
    }

    #[test]
    fn test_export_open_symbols() {
        // 'open' is more permissive than 'public' in Swift
        let source = r"
open class OpenClass {
    open func openMethod() {}
}

open func openFunction() {}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export open symbols
        assert!(has_export_edge(&staging), "Should export open symbols");

        // Should have 2 exports (OpenClass and openFunction)
        assert_eq!(
            count_export_edges(&staging),
            2,
            "Should export open class and function"
        );
    }

    #[test]
    fn test_mixed_visibility() {
        let source = r"
public func publicFunc() {}
private func privateFunc() {}
func internalFunc() {}

public class PublicClass {}
private class PrivateClass {}
class InternalClass {}
";

        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        let file = PathBuf::from("test.swift");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build graph");

        // Should export public and internal symbols (not private)
        let export_count = count_export_edges(&staging);

        // Expected: publicFunc, internalFunc, PublicClass, InternalClass (4 total)
        // NOT exported: privateFunc, PrivateClass
        assert_eq!(
            export_count, 4,
            "Should export public and internal symbols (not private)"
        );
    }

    // ========================================================================
    // C2_OTHER_SWIFT — Property/Constant emission for `let`/`var` properties
    // REQ:R0001, R0003, R0004, R0005, R0023
    // ========================================================================

    use sqry_core::graph::unified::node::NodeKind;

    /// Locate a staged `AddNode` op by exact canonical (qualified) name.
    fn find_staged_node<'a>(
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

    /// Resolve a node's visibility string through the staging interner.
    fn visibility_of(
        staging: &StagingGraph,
        entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    ) -> Option<String> {
        entry
            .visibility
            .and_then(|sid| staging.resolve_local_string(sid).map(str::to_string))
    }

    fn build_swift(source: &str) -> StagingGraph {
        let tree = parse_swift(source);
        let mut staging = StagingGraph::new();
        let builder = SwiftGraphBuilder::default();
        builder
            .build_graph(
                &tree,
                source.as_bytes(),
                &PathBuf::from("test.swift"),
                &mut staging,
            )
            .expect("build graph");
        staging
    }

    /// AC-1 (let half) + AC-3 (default visibility): an instance `let` property
    /// emits `NodeKind::Constant` under `Container.field`, with
    /// `is_static = false` and `visibility = Some("internal")` (Swift's
    /// implicit default for instance members).
    #[test]
    fn test_let_property_emits_constant_with_internal_default() {
        let source = r"
class C {
    let x: Int = 0
}
";
        let staging = build_swift(source);
        let entry = find_staged_node(&staging, "C::x").expect("C::x must be staged");
        assert_eq!(entry.kind, NodeKind::Constant, "let → Constant");
        assert!(!entry.is_static, "instance let must not be static");
        assert_eq!(
            visibility_of(&staging, entry).as_deref(),
            Some("internal"),
            "Swift default visibility is internal (not None, not public)"
        );
        // Old NodeKind::Variable for C.x must NOT appear.
        let stale = staging.operations().iter().any(|op| {
            matches!(op, StagingOp::AddNode { entry, .. }
                if entry.kind == NodeKind::Variable
                    && staging.resolve_node_canonical_name(entry) == Some("C::x"))
        });
        assert!(
            !stale,
            "C.x must no longer be emitted as NodeKind::Variable"
        );
    }

    /// AC-1 (var half) + AC-3 (default visibility): an instance `var` property
    /// emits `NodeKind::Property` with `is_static = false` and
    /// `visibility = Some("internal")`.
    #[test]
    fn test_var_property_emits_property_with_internal_default() {
        let source = r#"
class C {
    var y: String = ""
}
"#;
        let staging = build_swift(source);
        let entry = find_staged_node(&staging, "C::y").expect("C.y must be staged");
        assert_eq!(entry.kind, NodeKind::Property, "var → Property");
        assert!(!entry.is_static, "instance var must not be static");
        assert_eq!(
            visibility_of(&staging, entry).as_deref(),
            Some("internal"),
            "Swift default visibility is internal"
        );
    }

    /// AC-2: `static` modifier on a `let` flips `is_static = true`. The
    /// kind stays `Constant` because the binding is `let`.
    #[test]
    fn test_static_let_emits_constant_static_true() {
        let source = r"
class C {
    public static let z: Int = 0
}
";
        let staging = build_swift(source);
        let entry = find_staged_node(&staging, "C::z").expect("C.z must be staged");
        assert_eq!(entry.kind, NodeKind::Constant);
        assert!(entry.is_static, "static let must have is_static = true");
        assert_eq!(visibility_of(&staging, entry).as_deref(), Some("public"));
    }

    /// AC-2: `class` modifier (used in classes for class-level overridable
    /// statics) flips `is_static = true` on a `var`. Kind stays `Property`.
    /// Note: Swift `class` type properties are computed-only — stored type
    /// properties must use `static`. The fixture therefore uses a computed
    /// `class var` form, which is the only valid way to exercise the
    /// `class`-modifier branch.
    #[test]
    fn test_class_var_emits_property_static_true() {
        let source = r#"
class C {
    open class var w: String { return "" }
}
"#;
        let staging = build_swift(source);
        let entry = find_staged_node(&staging, "C::w").expect("C.w must be staged");
        assert_eq!(entry.kind, NodeKind::Property);
        assert!(entry.is_static, "class var must have is_static = true");
        assert_eq!(visibility_of(&staging, entry).as_deref(), Some("open"));
    }

    /// AC-3: each Swift access-control modifier maps verbatim onto the node
    /// `visibility` field — `public`, `internal`, `fileprivate`, `private`,
    /// `open`. Implicit default → `internal`.
    #[test]
    fn test_visibility_modifier_round_trip() {
        let source = r"
class C {
    public let p: Int = 0
    internal let i: Int = 0
    fileprivate let fp: Int = 0
    private let pr: Int = 0
    open let o: Int = 0
    let d: Int = 0
}
";
        let staging = build_swift(source);
        for (qn, expected) in [
            ("C::p", Some("public")),
            ("C::i", Some("internal")),
            ("C::fp", Some("fileprivate")),
            ("C::pr", Some("private")),
            ("C::o", Some("open")),
            ("C::d", Some("internal")),
        ] {
            let entry =
                find_staged_node(&staging, qn).unwrap_or_else(|| panic!("{qn} must be staged"));
            assert_eq!(
                visibility_of(&staging, entry).as_deref(),
                expected,
                "{qn} visibility mismatch"
            );
        }
    }

    /// AC-3 regression: a setter-access modifier (`private(set)`,
    /// `fileprivate(set)`, `internal(set)`) lowers the SETTER's visibility
    /// only — the property's overall visibility comes from the bare
    /// access modifier that appears alongside it. `public private(set) var`
    /// must emit visibility = `public`, NOT `private`.
    #[test]
    fn test_setter_access_modifier_does_not_override_visibility() {
        let source = r"
class C {
    public private(set) var limited: Int = 0
    internal fileprivate(set) var halfOpen: Int = 0
    public internal(set) var nearlyPublic: Int = 0
}
";
        let staging = build_swift(source);
        for (qn, expected) in [
            ("C::limited", Some("public")),
            ("C::halfOpen", Some("internal")),
            ("C::nearlyPublic", Some("public")),
        ] {
            let entry =
                find_staged_node(&staging, qn).unwrap_or_else(|| panic!("{qn} must be staged"));
            assert_eq!(entry.kind, NodeKind::Property);
            assert_eq!(
                visibility_of(&staging, entry).as_deref(),
                expected,
                "{qn} visibility must come from the bare access modifier, not the (set) form"
            );
        }
    }

    /// AC-3 regression (whitespace variants): tree-sitter-swift accepts
    /// whitespace inside the setter-access parentheses (`private( set )`,
    /// `private ( set )`, `internal( set )`, etc.). The previous
    /// `mod_text.contains("(set)")` text-substring check missed these forms
    /// and incorrectly took `private`/`internal` as the property's visibility
    /// instead of the bare modifier (`public`). The structural child-walk
    /// detects the `set` token regardless of internal whitespace.
    #[test]
    fn test_setter_access_modifier_whitespace_variants_skipped() {
        let source = r"
class C {
    public private ( set ) var oddly_spaced: Int = 0
    public internal( set ) var oddBraces: Int = 0
    public fileprivate (set) var leadingSpace: Int = 0
}
";
        let staging = build_swift(source);
        for (qn, expected) in [
            ("C::oddly_spaced", Some("public")),
            ("C::oddBraces", Some("public")),
            ("C::leadingSpace", Some("public")),
        ] {
            let entry =
                find_staged_node(&staging, qn).unwrap_or_else(|| panic!("{qn} must be staged"));
            assert_eq!(entry.kind, NodeKind::Property);
            assert_eq!(
                visibility_of(&staging, entry).as_deref(),
                expected,
                "{qn} visibility must come from the bare access modifier even when the (set) form has internal whitespace"
            );
        }
    }

    /// AC-4: tuple destructuring `let (a, b) = (1, 2)` emits one
    /// `Constant` node per pattern name (`C.a` and `C.b`).
    #[test]
    fn test_tuple_destructuring_emits_one_node_per_name() {
        let source = r"
class C {
    let (a, b) = (1, 2)
}
";
        let staging = build_swift(source);
        let a = find_staged_node(&staging, "C::a").expect("C.a must be staged");
        let b = find_staged_node(&staging, "C::b").expect("C.b must be staged");
        assert_eq!(a.kind, NodeKind::Constant, "tuple-bound let → Constant");
        assert_eq!(b.kind, NodeKind::Constant, "tuple-bound let → Constant");
        assert!(!a.is_static);
        assert!(!b.is_static);
        // Old NodeKind::Variable for these names must NOT appear.
        let stale = staging.operations().iter().any(|op| {
            matches!(op, StagingOp::AddNode { entry, .. }
            if entry.kind == NodeKind::Variable
                && matches!(
                    staging.resolve_node_canonical_name(entry),
                    Some("C::a" | "C::b")
                ))
        });
        assert!(
            !stale,
            "tuple-destructured names must not emit NodeKind::Variable"
        );
    }

    /// AC-4 (var half): `var (a, b) = (1, 2)` emits one `Property` per name.
    #[test]
    fn test_tuple_destructuring_var_emits_property_per_name() {
        let source = r"
class C {
    var (m, n) = (1, 2)
}
";
        let staging = build_swift(source);
        let m = find_staged_node(&staging, "C::m").expect("C.m must be staged");
        let n = find_staged_node(&staging, "C::n").expect("C.n must be staged");
        assert_eq!(m.kind, NodeKind::Property);
        assert_eq!(n.kind, NodeKind::Property);
    }

    /// AC-5 (regression): the existing `TypeOf` edge with `TypeOfContext::Field`
    /// and the bare property name is preserved across the helper swap. The
    /// edge's source `NodeId` must point at the new Property/Constant node.
    #[test]
    fn test_property_typeof_edge_metadata_preserved() {
        let source = r"
class C {
    let x: Int = 0
}
";
        let staging = build_swift(source);
        let x_id = staging
            .operations()
            .iter()
            .find_map(|op| match op {
                StagingOp::AddNode {
                    entry,
                    expected_id: Some(id),
                } if staging.resolve_node_canonical_name(entry) == Some("C::x")
                    && entry.kind == NodeKind::Constant =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .expect("C.x Constant node must be staged");

        let typeof_edge = staging.operations().iter().find_map(|op| {
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
        let (context, name) = typeof_edge.expect("TypeOf edge from C.x must be staged");
        assert_eq!(context, Some(TypeOfContext::Field));
        let resolved_name = name.and_then(|sid| staging.resolve_local_string(sid));
        assert_eq!(
            resolved_name,
            Some("x"),
            "TypeOf edge name must be the bare field name (not qualified)"
        );
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
        "/../test-fixtures/shape/systems/sample.swift"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("load Swift grammar");
        parser.parse(src, None).expect("parse Swift sample")
    }

    fn first_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn swift_mapping_is_non_empty() {
        let mapping = swift_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
        let count = (0..lang.node_kind_count())
            .filter_map(|id| u16::try_from(id).ok())
            .filter(|id| mapping.cf_bucket(*id).is_some())
            .count();
        assert!(
            count > 0,
            "Swift cf_bucket map should cover real control-flow kinds"
        );
    }

    #[test]
    fn swift_histogram_covers_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_declaration")
            .expect("sample has a function_declaration");
        let desc = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            swift_shape_mapping(),
            &ShapeBudget::default(),
        );
        let h = &desc.cf_histogram;
        assert!(h[CfBucket::Branch.index()] >= 1, "branch present");
        assert!(h[CfBucket::Loop.index()] >= 1, "loop present");
        assert!(h[CfBucket::Match.index()] >= 1, "switch present");
        assert!(h[CfBucket::Try.index()] >= 1, "do/try present");
        assert!(h[CfBucket::Catch.index()] >= 1, "catch present");
        assert!(h[CfBucket::Throw.index()] >= 1, "throw present");
        assert!(h[CfBucket::Call.index()] >= 1, "call present");
        assert!(
            h[CfBucket::BreakContinue.index()] >= 1,
            "control transfer present"
        );
    }

    #[test]
    fn swift_signature_shape_reads_params() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_declaration")
            .expect("sample has a function_declaration");
        let shape = swift_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // classify(_ n: Int, label: String) -> Int : two positional params.
        assert_eq!(shape.arity_positional, 2, "two positional params");
        assert!(
            shape.has_return_annotation,
            "Swift return annotation present"
        );
    }
}
