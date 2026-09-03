//! `GraphBuilder` implementation for Elixir
//!
//! Builds the unified `CodeGraph` for Elixir files by:
//! 1. Extracting function and macro definitions (def, defp, defmacro, defmacrop, `GenServer` callbacks)
//! 2. Detecting function call expressions (local, remote, Erlang FFI)
//! 3. Creating call edges between caller and callee
//! 4. Creating export edges for public functions and macros (def, defmacro)
//! 5. Tracking protocol definitions (defprotocol) as Interface nodes
//! 6. Tracking protocol implementations (defimpl) with Implements edges
//!
//! ## Cross-Language Support
//! - Erlang FFI: Detects `:module.function()` syntax as `FfiCall` edges
//! - Pipe operator: Chains are expanded into sequential `DirectCall` edges
//!
//! ## Limitations
//! - Macros: Not expanded (compile-time only)
//! - Dynamic calls: apply/3 not tracked (runtime-only)

use std::sync::OnceLock;
use std::{collections::HashMap, path::Path};

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::storage::shape::SignatureShape;

use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::{ExportKind, GraphBuildHelper, NodeKind, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, StreamingIterator, Tree};

use super::type_extractor::{
    extract_all_type_names_from_elixir_type, extract_type_string, is_type_node,
};

/// `GraphBuilder` for Elixir files using manual tree walking approach
#[derive(Debug, Clone, Copy)]
pub struct ElixirGraphBuilder {
    max_scope_depth: usize,
}

impl Default for ElixirGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 3, // Elixir: module -> function -> nested function
        }
    }
}

impl ElixirGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for ElixirGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Elixir);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // First pass: collect protocol definitions
        let mut protocol_map = HashMap::new();
        collect_protocols(tree.root_node(), content, &mut helper, &mut protocol_map)?;

        // Create recursion guard for tree walking
        let recursion_limits =
            sqry_core::config::RecursionLimits::load_or_default().map_err(|e| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason: format!("Failed to load recursion limits: {e}"),
                }
            })?;
        let file_ops_depth = recursion_limits.effective_file_ops_depth().map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Invalid file_ops_depth configuration: {e}"),
            }
        })?;
        let mut guard =
            sqry_core::query::security::RecursionGuard::new(file_ops_depth).map_err(|e| {
                GraphBuilderError::ParseError {
                    span: Span::default(),
                    reason: format!("Failed to create recursion guard: {e}"),
                }
            })?;

        // Second pass: walk tree to extract functions, calls, and protocol implementations
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &protocol_map,
            &mut guard,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Elixir
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(elixir_shape_mapping())
    }
}

// ============================================================================
// Graph Building with GraphBuildHelper
// ============================================================================

/// First pass: collect all protocol definitions
fn collect_protocols(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    protocol_map: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) -> GraphResult<()> {
    if node.kind() == "call"
        && is_protocol_definition(&node, content)
        && let Some(protocol_id) = build_protocol_node(node, content, helper)?
    {
        // Extract protocol name to store in map
        let mut node_cursor = node.walk();
        for child in node.children(&mut node_cursor) {
            if child.kind() == "arguments" {
                let mut args_cursor = child.walk();
                for arg_child in child.children(&mut args_cursor) {
                    if (arg_child.kind() == "identifier" || arg_child.kind() == "alias")
                        && let Ok(name) = arg_child.utf8_text(content)
                    {
                        protocol_map.insert(name.to_string(), protocol_id);
                        break;
                    }
                }
                break;
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_protocols(child, content, helper, protocol_map)?;
    }

    Ok(())
}

/// Walk the tree and build graph nodes/edges using `GraphBuildHelper`
///
/// # Errors
///
/// Returns [`GraphBuilderError`] if graph operations fail or recursion depth exceeds the guard's limit.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    protocol_map: &HashMap<String, sqry_core::graph::unified::NodeId>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!("Recursion limit exceeded: {e}"),
    })?;

    // Check for @spec annotations and process TypeOf/Reference edges
    if node.kind() == "unary_operator" && is_spec_annotation(&node, content) {
        process_spec_typeof_edges(node, content, helper)?;
    }

    // Check for protocol implementations
    if node.kind() == "call" && is_protocol_implementation(&node, content) {
        build_protocol_impl(node, content, helper, protocol_map)?;
    }
    // Check for function definitions
    else if is_function_definition(&node, content) {
        // Extract function context from AST graph
        if let Some(context) = ast_graph.get_callable_context(node.id()) {
            let span = span_from_node(node);

            // Add function node with visibility
            // Visibility: defp/defmacrop = private, def/defmacro = public
            let visibility = if context.is_private {
                "private"
            } else {
                "public"
            };
            let function_id = helper.add_function_with_visibility(
                &context.qualified_name,
                Some(span),
                false, // Elixir doesn't have async in the same way
                false, // Elixir doesn't have unsafe
                Some(visibility),
            );

            // Emit Export edge for public functions and macros (def/defmacro, not defp/defmacrop)
            if !context.is_private {
                let module_id = helper.add_module("<module>", None);
                helper.add_export_edge_full(module_id, function_id, ExportKind::Direct, None);
            }
        }
    }

    // Check for Erlang NIF calls (FFI)
    if node.kind() == "call" && is_erlang_load_nif(&node, content) {
        build_nif_ffi_edge(node, content, ast_graph, helper);
    }
    // Check for import/alias/use/require statements
    else if node.kind() == "call" && is_import_statement(&node, content) {
        // Build import edge for import, alias, use, require
        build_import_edge_with_helper(node, content, helper)?;
    }
    // Check for call expressions (excluding function definitions and imports)
    else if node.kind() == "call"
        && !is_function_definition(&node, content)
        && let Ok(Some((caller_id, callee_id, argument_count, span))) =
            build_call_edge_with_helper(ast_graph, node, content, helper)
    {
        let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
        helper.add_call_edge_full_with_span(
            caller_id,
            callee_id,
            argument_count,
            false,
            vec![span],
        );
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(child, content, ast_graph, helper, protocol_map, guard)?;
    }

    guard.exit();
    Ok(())
}

/// Build a call edge from a call node using `GraphBuildHelper`
fn build_call_edge_with_helper(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<
    Option<(
        sqry_core::graph::unified::NodeId,
        sqry_core::graph::unified::NodeId,
        usize,
        Span,
    )>,
> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            is_private: false,
        };
        &module_context
    };

    // Extract the call target (the function being called)
    let Some(target_node) = call_node.child_by_field_name("target") else {
        return Ok(None);
    };

    // Determine the callee name and edge kind
    let (callee_text, _is_erlang_ffi) = extract_call_info(&target_node, content)?;

    if callee_text.is_empty() {
        return Ok(None);
    }

    // Ensure both nodes exist
    let caller_fn_id = helper.add_function(&call_context.qualified_name, None, false, false);
    let target_fn_id = helper.add_function(&callee_text, None, false, false);

    let call_span = span_from_node(call_node);
    let argument_count = count_arguments(call_node);

    Ok(Some((
        caller_fn_id,
        target_fn_id,
        argument_count,
        call_span,
    )))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a call node is a function definition (def/defp)
fn is_function_definition(call_node: &Node, content: &[u8]) -> bool {
    if let Some(target) = call_node.child_by_field_name("target")
        && let Ok(target_text) = target.utf8_text(content)
    {
        return matches!(target_text, "def" | "defp" | "defmacro" | "defmacrop");
    }
    false
}

/// Check if a call node is an import statement (import, alias, use, require)
fn is_import_statement(call_node: &Node, content: &[u8]) -> bool {
    if let Some(target) = call_node.child_by_field_name("target")
        && let Ok(target_text) = target.utf8_text(content)
    {
        return matches!(target_text, "import" | "alias" | "use" | "require");
    }
    false
}

/// Check if a call node is a protocol definition (defprotocol)
fn is_protocol_definition(call_node: &Node, content: &[u8]) -> bool {
    if let Some(target) = call_node.child_by_field_name("target")
        && let Ok(target_text) = target.utf8_text(content)
    {
        return target_text == "defprotocol";
    }
    false
}

/// Check if a call node is a protocol implementation (defimpl)
fn is_protocol_implementation(call_node: &Node, content: &[u8]) -> bool {
    if let Some(target) = call_node.child_by_field_name("target")
        && let Ok(target_text) = target.utf8_text(content)
    {
        return target_text == "defimpl";
    }
    false
}

/// Check if a `unary_operator` node is a @spec or @type annotation
fn is_spec_annotation(node: &Node, content: &[u8]) -> bool {
    if node.kind() != "unary_operator" {
        return false;
    }

    // Check if this is a spec or type annotation
    if let Some(call_node) = node.named_child(0)
        && call_node.kind() == "call"
        && let Some(target) = call_node.named_child(0)
        && let Ok(target_text) = target.utf8_text(content)
    {
        return target_text == "spec" || target_text == "type";
    }
    false
}

/// Process a @spec annotation and create TypeOf/Reference edges
#[allow(clippy::unnecessary_wraps)]
fn process_spec_typeof_edges(
    spec_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract the function name and types from the @spec
    // Pattern: @spec function_name(type1, type2) :: return_type

    if let Some(call_node) = spec_node.named_child(0)
        && call_node.kind() == "call"
        && let Some(args_node) = call_node.named_child(1)
        && args_node.kind() == "arguments"
        && let Some(binary_op) = args_node.named_child(0)
        && binary_op.kind() == "binary_operator"
        && let Some(func_call) = binary_op.named_child(0)
    {
        // Extract function name
        let func_name = if let Some(target) = func_call.named_child(0) {
            target.utf8_text(content).ok().map(String::from)
        } else {
            None
        };

        if let Some(func_name) = func_name {
            // Get or create function node
            let function_id = helper.add_function(&func_name, None, false, false);

            // Process parameter types
            if let Some(param_args) = func_call.named_child(1)
                && param_args.kind() == "arguments"
            {
                let mut param_index: u16 = 0;
                let mut cursor = param_args.walk();
                for param_type_node in param_args.named_children(&mut cursor) {
                    if is_type_node(param_type_node.kind()) {
                        // Extract full type string
                        if let Some(type_text) = extract_type_string(param_type_node, content) {
                            let type_id = helper.add_type(&type_text, None);
                            helper.add_typeof_edge_with_context(
                                function_id,
                                type_id,
                                Some(TypeOfContext::Parameter),
                                Some(param_index),
                                None,
                            );
                        }

                        // Extract nested type names for Reference edges
                        let referenced_types =
                            extract_all_type_names_from_elixir_type(param_type_node, content);
                        for ref_type_name in referenced_types {
                            let ref_type_id = helper.add_type(&ref_type_name, None);
                            helper.add_reference_edge(function_id, ref_type_id);
                        }

                        param_index += 1;
                    }
                }
            }

            // Process return type (right side of ::)
            if let Some(return_type_node) = binary_op.named_child(1)
                && is_type_node(return_type_node.kind())
            {
                // Extract full type string
                if let Some(type_text) = extract_type_string(return_type_node, content) {
                    let type_id = helper.add_type(&type_text, None);
                    helper.add_typeof_edge_with_context(
                        function_id,
                        type_id,
                        Some(TypeOfContext::Return),
                        Some(0),
                        None,
                    );
                }

                // Extract nested type names for Reference edges
                let referenced_types =
                    extract_all_type_names_from_elixir_type(return_type_node, content);
                for ref_type_name in referenced_types {
                    let ref_type_id = helper.add_type(&ref_type_name, None);
                    helper.add_reference_edge(function_id, ref_type_id);
                }
            }
        }
    }

    Ok(())
}

/// Build protocol node from a defprotocol statement
#[allow(clippy::unnecessary_wraps)]
fn build_protocol_node(
    protocol_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<Option<sqry_core::graph::unified::NodeId>> {
    // Extract protocol name from arguments
    // Pattern: defprotocol Name do ... end
    // Find the "arguments" child node (it's a direct child, not a field)
    let mut cursor = protocol_node.walk();
    for child in protocol_node.children(&mut cursor) {
        if child.kind() == "arguments" {
            // Found the arguments node - now extract the protocol name
            let mut args_cursor = child.walk();
            for arg_child in child.children(&mut args_cursor) {
                if (arg_child.kind() == "alias" || arg_child.kind() == "identifier")
                    && let Ok(name) = arg_child.utf8_text(content)
                {
                    let span = span_from_node(protocol_node);
                    // Protocols are like interfaces in other languages
                    let protocol_id = helper.add_interface(name, Some(span));
                    // issue #394: real declaration; opt dual-use bare helper into is_definition
                    helper.mark_definition(protocol_id);
                    return Ok(Some(protocol_id));
                }
            }
        }
    }
    Ok(None)
}

/// Build protocol implementation and creates Implements edge
#[allow(clippy::unnecessary_wraps)]
fn build_protocol_impl(
    impl_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    protocol_map: &HashMap<String, sqry_core::graph::unified::NodeId>,
) -> GraphResult<()> {
    // Extract protocol name and target type
    // Pattern: defimpl ProtocolName, for: TargetType do ... end
    // Find the "arguments" child node
    let mut impl_cursor = impl_node.walk();
    for child in impl_node.children(&mut impl_cursor) {
        if child.kind() == "arguments" {
            let mut protocol_name = None;
            let mut target_type = None;

            let mut cursor = child.walk();
            let mut found_protocol = false;

            for arg_child in child.children(&mut cursor) {
                // First identifier/alias is the protocol name
                if !found_protocol
                    && (arg_child.kind() == "identifier" || arg_child.kind() == "alias")
                {
                    if let Ok(name) = arg_child.utf8_text(content) {
                        protocol_name = Some(name.to_string());
                        found_protocol = true;
                    }
                }
                // Look for "for:" keyword list
                else if arg_child.kind() == "keywords" {
                    // Find the type after "for:"
                    let mut kw_cursor = arg_child.walk();
                    for kw_child in arg_child.children(&mut kw_cursor) {
                        if kw_child.kind() == "pair" {
                            // Check if this is the "for:" pair
                            if let Some(key) = kw_child.child_by_field_name("key")
                                && let Ok(key_text) = key.utf8_text(content)
                            {
                                let key_trimmed = key_text.trim().trim_end_matches(':');
                                // Match both "for" and "for:"
                                if key_trimmed == "for" {
                                    if let Some(value) = kw_child.child_by_field_name("value") {
                                        if let Ok(type_name) = value.utf8_text(content) {
                                            target_type = Some(type_name.to_string());
                                        }
                                    } else {
                                        // Try walking children to find the type
                                        let mut pair_cursor = kw_child.walk();
                                        for pair_child in kw_child.children(&mut pair_cursor) {
                                            if (pair_child.kind() == "alias"
                                                || pair_child.kind() == "identifier")
                                                && let Ok(type_name) = pair_child.utf8_text(content)
                                                && type_name != "for:"
                                                && type_name != "for"
                                            {
                                                target_type = Some(type_name.to_string());
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let (Some(protocol), Some(target)) = (protocol_name, target_type) {
                let span = span_from_node(impl_node);

                // Create a struct/class node for the implementation
                // Name it as "ProtocolName.TargetType" for uniqueness
                let impl_name = format!("{protocol}.{target}");
                let impl_id = helper.add_struct(&impl_name, Some(span));
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(impl_id);

                // If we have the protocol in the map, create an Implements edge
                if let Some(&protocol_id) = protocol_map.get(&protocol) {
                    helper.add_implements_edge(impl_id, protocol_id);
                } else {
                    // Protocol not in map - create it as external interface
                    let protocol_id = helper.add_interface(&protocol, None);
                    helper.add_implements_edge(impl_id, protocol_id);
                }
            }
            break;
        }
    }

    Ok(())
}

/// Build import edge from an import/alias/use/require statement
#[allow(clippy::too_many_lines)] // Complex AST patterns are clearer in a single pass.
#[allow(clippy::unnecessary_wraps)] // Returns GraphResult for consistency with other helpers.
fn build_import_edge_with_helper(
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Get the import type (import, alias, use, require)
    let Some(target) = call_node.child_by_field_name("target") else {
        return Ok(());
    };
    let import_type = target.utf8_text(content).unwrap_or("");

    // Get the arguments containing the module name
    // Note: tree-sitter-elixir doesn't use a field name for arguments, so we find by kind
    let mut cursor = call_node.walk();
    let args_node = call_node
        .children(&mut cursor)
        .find(|c| c.kind() == "arguments");

    let Some(args_node) = args_node else {
        return Ok(());
    };

    // Extract the module name (first argument is typically the module alias)
    let mut cursor = args_node.walk();
    let mut module_name: Option<String> = None;
    let mut alias_name: Option<String> = None;
    // is_wildcard semantics:
    // - `import Mod` = true (imports all exports from Mod)
    // - `import Mod, only: [...]` = false (selective import)
    // - `alias Mod` = false (creates a reference/alias, not a wildcard import)
    // - `use Mod` = true (injects macros/callbacks, effectively a wildcard)
    // - `require Mod` = false (makes module's macros available but doesn't import)
    let mut is_wildcard = matches!(import_type, "import" | "use");
    let mut has_only_or_except = false;

    for child in args_node.named_children(&mut cursor) {
        match child.kind() {
            "alias" => {
                // Module name like Phoenix.Controller or Enum
                if module_name.is_none()
                    && let Ok(text) = child.utf8_text(content)
                {
                    module_name = Some(text.to_string());
                    // For `alias` statements, extract the default alias (last segment)
                    // e.g., `alias Phoenix.Controller` defaults to alias `Controller`
                    if import_type == "alias"
                        && alias_name.is_none()
                        && let Some(last_segment) = text.rsplit('.').next()
                    {
                        alias_name = Some(last_segment.to_string());
                    }
                }
            }
            "dot" => {
                // Multi-alias syntax: alias Phoenix.{Socket, Channel}
                // The dot node contains the base module (alias) and the tuple of elements
                if import_type == "alias" {
                    // Extract base module and tuple from the dot node
                    let mut dot_cursor = child.walk();
                    let mut base_module: Option<String> = None;
                    let mut tuple_elements: Vec<String> = Vec::new();

                    for dot_child in child.named_children(&mut dot_cursor) {
                        match dot_child.kind() {
                            "alias" => {
                                // This is the base module (e.g., "Phoenix")
                                if base_module.is_none()
                                    && let Ok(text) = dot_child.utf8_text(content)
                                {
                                    base_module = Some(text.to_string());
                                }
                            }
                            "tuple" => {
                                // Extract the alias elements from the tuple
                                let mut tuple_cursor = dot_child.walk();
                                for tuple_elem in dot_child.named_children(&mut tuple_cursor) {
                                    if tuple_elem.kind() == "alias"
                                        && let Ok(text) = tuple_elem.utf8_text(content)
                                    {
                                        tuple_elements.push(text.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    // If we found tuple elements, emit individual edges
                    if !tuple_elements.is_empty() {
                        let span = span_from_node(call_node);
                        let module_id = helper.add_module("<module>", None);
                        let base = base_module.unwrap_or_default();

                        for element in tuple_elements {
                            // Build the full module path: e.g., Phoenix.Socket
                            let full_module = if base.is_empty() {
                                element.clone()
                            } else {
                                format!("{base}.{element}")
                            };

                            // Default alias is the element name itself
                            let alias_value = element.clone();

                            let import_id = helper.add_import(&full_module, Some(span));
                            // Multi-alias elements are NOT wildcard (they're explicit aliases)
                            helper.add_import_edge_full(
                                module_id,
                                import_id,
                                Some(&alias_value),
                                false,
                            );
                        }

                        // Return early - we've already emitted all edges
                        return Ok(());
                    }
                }
                // If we didn't find tuple elements, treat this as a regular dot access
                // (e.g., Foo.Bar.Baz) - extract the full text as module name
                if let Ok(text) = child.utf8_text(content) {
                    module_name = Some(text.to_string());
                    // For alias statements, extract the default alias (last segment)
                    if import_type == "alias"
                        && alias_name.is_none()
                        && let Some(last_segment) = text.rsplit('.').next()
                    {
                        alias_name = Some(last_segment.to_string());
                    }
                }
            }
            "tuple" => {
                // Grouped aliases without dot prefix (rare case: alias {Foo, Bar})
                // This can happen if someone writes `alias {Foo, Bar}` without a base module
                if import_type == "alias" {
                    // Extract the elements from the tuple
                    let mut tuple_cursor = child.walk();
                    let tuple_elements: Vec<String> = child
                        .named_children(&mut tuple_cursor)
                        .filter_map(|elem| {
                            if elem.kind() == "alias" {
                                elem.utf8_text(content).ok().map(String::from)
                            } else {
                                None
                            }
                        })
                        .collect();

                    // If we have tuple elements, emit individual edges for each
                    if !tuple_elements.is_empty() {
                        let span = span_from_node(call_node);
                        let module_id = helper.add_module("<module>", None);

                        for element in tuple_elements {
                            let import_id = helper.add_import(&element, Some(span));
                            // Multi-alias elements are NOT wildcard (they're explicit aliases)
                            helper.add_import_edge_full(
                                module_id,
                                import_id,
                                Some(&element),
                                false,
                            );
                        }

                        // Return early - we've already emitted all edges
                        return Ok(());
                    }
                }
                // For non-alias statements with tuple syntax (unusual), fall through
                // to default behavior with wildcard
                is_wildcard = true;
            }
            "keywords" => {
                // Options like `only: [...]` or `as: Alias`
                let mut kw_cursor = child.walk();
                for kw_pair in child.named_children(&mut kw_cursor) {
                    if kw_pair.kind() == "pair" {
                        // Look for `as:` option
                        let mut pair_cursor = kw_pair.walk();
                        let mut key: Option<String> = None;
                        let mut value: Option<String> = None;

                        for pair_child in kw_pair.named_children(&mut pair_cursor) {
                            match pair_child.kind() {
                                "keyword" | "atom" => {
                                    if key.is_none()
                                        && let Ok(text) = pair_child.utf8_text(content)
                                    {
                                        // Trim whitespace first, then the trailing colon
                                        // "as: " -> "as:" -> "as"
                                        key = Some(text.trim().trim_end_matches(':').to_string());
                                    }
                                }
                                "alias" | "identifier" => {
                                    if value.is_none()
                                        && let Ok(text) = pair_child.utf8_text(content)
                                    {
                                        value = Some(text.to_string());
                                    }
                                }
                                "list" => {
                                    // `only: [...]` or `except: [...]` - treat as partial import
                                    has_only_or_except = true;
                                    is_wildcard = false;
                                }
                                _ => {}
                            }
                        }

                        if key.as_deref() == Some("as") {
                            alias_name = value;
                        } else if key.as_deref() == Some("only") || key.as_deref() == Some("except")
                        {
                            has_only_or_except = true;
                            is_wildcard = false;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // For alias statements without explicit `as:`, the alias is already set to the default
    // For import/use/require without `only:`/`except:`, is_wildcard remains true
    let _ = has_only_or_except; // Used to set is_wildcard

    // Create the import edge if we found a module name
    if let Some(imported_module) = module_name {
        let span = span_from_node(call_node);

        // Create module node (importer) and import node (imported)
        let module_id = helper.add_module("<module>", None);

        // For `use`, we prefix the import name to distinguish semantic
        let import_name = match import_type {
            "use" => format!("use:{imported_module}"),
            "require" => format!("require:{imported_module}"),
            _ => imported_module.clone(),
        };

        let import_id = helper.add_import(&import_name, Some(span));

        // Always use add_import_edge_full to correctly set metadata
        helper.add_import_edge_full(module_id, import_id, alias_name.as_deref(), is_wildcard);
    }

    Ok(())
}

/// Count the number of arguments in a function call
fn count_arguments(call_node: Node<'_>) -> usize {
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut cursor = args_node.walk();
        let children: Vec<_> = args_node.named_children(&mut cursor).collect();

        // Filter out delimiters and count actual argument nodes
        let count = children
            .iter()
            .filter(|child| {
                // Exclude structural delimiters
                !matches!(child.kind(), "," | "(" | ")" | "[" | "]")
            })
            .count();

        tracing::trace!(
            "count_arguments: call_node.kind={}, args_node.kind={}, children={:?}, count={}",
            call_node.kind(),
            args_node.kind(),
            children
                .iter()
                .map(tree_sitter::Node::kind)
                .collect::<Vec<_>>(),
            count
        );

        count
    } else {
        // No "arguments" field - try to find arguments list directly
        // Some tree-sitter grammars use different structure
        let mut cursor = call_node.walk();
        let children: Vec<_> = call_node
            .named_children(&mut cursor)
            .filter(|child| {
                // Look for argument list nodes
                matches!(child.kind(), "arguments" | "argument_list")
            })
            .collect();

        if let Some(arg_list) = children.first() {
            let mut arg_cursor = arg_list.walk();
            let args: Vec<_> = arg_list.named_children(&mut arg_cursor).collect();
            let count = args
                .iter()
                .filter(|child| !matches!(child.kind(), "," | "(" | ")" | "[" | "]"))
                .count();

            tracing::trace!(
                "count_arguments (fallback): found argument_list, args={:?}, count={}",
                args.iter().map(tree_sitter::Node::kind).collect::<Vec<_>>(),
                count
            );

            count
        } else {
            tracing::trace!(
                "count_arguments: no arguments field or argument_list found for call_node.kind={}",
                call_node.kind()
            );
            0
        }
    }
}

/// Extract call information (name and edge kind) from a target node
fn extract_call_info(target_node: &Node, content: &[u8]) -> GraphResult<(String, bool)> {
    // Handle different call patterns
    match target_node.kind() {
        // Simple identifier: foo()
        "identifier" => {
            let name = target_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: span_from_node(*target_node),
                    reason: "failed to read call identifier".to_string(),
                })?
                .to_string();
            Ok((name, false))
        }

        // Dot operator: Module.function() or :erlang.function()
        "dot" => {
            if let Some(left) = target_node.child_by_field_name("left") {
                let left_text = left.utf8_text(content).unwrap_or("");

                // Check if it's Erlang FFI (:atom.function)
                let is_erlang_ffi = left_text.starts_with(':');

                // Get the full qualified name
                let full_name = target_node
                    .utf8_text(content)
                    .map_err(|_| GraphBuilderError::ParseError {
                        span: span_from_node(*target_node),
                        reason: "failed to read module-qualified call".to_string(),
                    })?
                    .to_string();

                Ok((full_name, is_erlang_ffi))
            } else {
                Ok((String::new(), false))
            }
        }

        // Other patterns (unary_operator, binary_operator, etc.)
        _ => {
            // Try to get the text representation
            if let Ok(text) = target_node.utf8_text(content) {
                Ok((text.to_string(), false))
            } else {
                Ok((String::new(), false))
            }
        }
    }
}

/// Convert a tree-sitter node to a Span
fn span_from_node(node: Node<'_>) -> Span {
    Span::from_node(&node)
}

// ============================================================================
// FFI Detection - Erlang NIF Support
// ============================================================================

/// Check if a call node is `:erlang.load_nif` (Erlang NIF loading)
///
/// Detects the primary FFI pattern in Elixir: loading native C libraries via Erlang's NIF system.
///
/// # Arity Handling
///
/// Accepts any arity, not just /2, because:
/// - Standard form is `load_nif(path, init_arg)` with arity 2
/// - But we want to detect incomplete/malformed calls during development
/// - Macro-generated code may have variations
/// - Graceful degradation is better than false negatives
///
/// The implementation will attempt to extract the library path from the
/// first argument when present, falling back to a generic target otherwise.
///
/// # Pattern
///
/// ```elixir
/// :erlang.load_nif('./path/to/lib', init_args)
/// ```
///
/// # AST Structure
///
/// ```text
/// call
/// ├── target: dot
/// │   ├── left: atom (:erlang)
/// │   └── right: identifier (load_nif)
/// └── arguments
/// ```
fn is_erlang_load_nif(node: &Node, content: &[u8]) -> bool {
    // Must have a target field
    let Some(target) = node.child_by_field_name("target") else {
        return false;
    };

    // Target must be a dot operator (module.function)
    if target.kind() != "dot" {
        return false;
    }

    // Left side must be :erlang atom
    let Some(left) = target.child_by_field_name("left") else {
        return false;
    };
    if left.kind() != "atom" {
        return false;
    }
    let Ok(left_text) = left.utf8_text(content) else {
        return false;
    };
    if left_text != ":erlang" {
        return false;
    }

    // Right side must be load_nif identifier
    let Some(right) = target.child_by_field_name("right") else {
        return false;
    };
    let Ok(right_text) = right.utf8_text(content) else {
        return false;
    };

    right_text == "load_nif"
}

/// Build FFI edge for Erlang NIF loading (`:erlang.load_nif/2`)
///
/// Creates an `FfiCall` edge from the calling function to the NIF loader.
///
/// # Edge Details
///
/// - **Caller**: Function containing the `:erlang.load_nif` call (from AST graph context)
/// - **Callee**: Fixed node `ffi::erlang::load_nif`
/// - **Convention**: `FfiConvention::C` (NIFs always use C ABI)
///
/// # Example
///
/// ```elixir
/// def init do
///   :erlang.load_nif('./my_nif', 0)  # Creates: init --FfiCall(C)--> ffi::erlang::load_nif
/// end
/// ```
fn build_nif_ffi_edge(
    node: Node,
    _content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    use sqry_core::graph::unified::edge::kind::FfiConvention;

    // Get caller context from AST graph
    let caller_name = if let Some(ctx) = ast_graph.get_callable_context(node.id()) {
        ctx.qualified_name.clone()
    } else {
        // Top-level call - use module context
        "<module>".to_string()
    };

    // Create caller node
    let caller_id = helper.add_function(&caller_name, None, false, false);

    // Create FFI function node (fixed name for all NIF loads)
    let ffi_func_name = "ffi::erlang::load_nif";
    let span = span_from_node(node);
    let ffi_func_id = helper.add_call_site_node(ffi_func_name, span, NodeKind::Function);

    // Add FfiCall edge with C convention (NIFs use C ABI)
    helper.add_ffi_edge(caller_id, ffi_func_id, FfiConvention::C);
}

// ============================================================================
// AST Graph - Tracks callable contexts
// ============================================================================

#[derive(Debug)]
struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], _max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();

        // Extract function and macro definitions using tree-sitter query
        // Match both public (def, defmacro) and private (defp, defmacrop) functions/macros
        let query = tree_sitter::Query::new(
            &tree_sitter_elixir_sqry::language(),
            r#"
            (call
              target: (identifier) @def_keyword
              (arguments
                (call
                  target: (identifier) @function_name
                ) @function_call
              )
              (#match? @def_keyword "^(def[p]?|defmacro[p]?)$")
            ) @function_node

            (call
              target: (identifier) @def_keyword
              (arguments
                (identifier) @function_name_simple
              )
              (#match? @def_keyword "^(def[p]?|defmacro[p]?)$")
            ) @function_node_simple
            "#,
        )
        .map_err(|e| format!("Failed to create query: {e}"))?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let root = tree.root_node();
        let capture_names = query.capture_names();
        let mut matches = cursor.matches(&query, root, content);

        while let Some(m) = matches.next() {
            let mut def_keyword = None;
            let mut function_name = None;
            let mut function_node = None;

            for capture in m.captures {
                let capture_name = capture_names[capture.index as usize];
                match capture_name {
                    "def_keyword" => def_keyword = Some(capture.node),
                    "function_name" | "function_name_simple" => function_name = Some(capture.node),
                    "function_node" | "function_node_simple" => function_node = Some(capture.node),
                    _ => {}
                }
            }

            if let (Some(def_kw), Some(name_node), Some(func_node)) =
                (def_keyword, function_name, function_node)
            {
                let name = name_node
                    .utf8_text(content)
                    .map_err(|e| format!("Failed to extract function name: {e}"))?
                    .to_string();

                let def_keyword_text = def_kw.utf8_text(content).unwrap_or("");
                let is_private = matches!(def_keyword_text, "defp" | "defmacrop");

                let context_idx = contexts.len();
                contexts.push(CallContext {
                    qualified_name: name,
                    is_private,
                });

                // Map all descendant nodes to this context
                map_descendants_to_context(&func_node, context_idx, &mut node_to_context);
            }
        }

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
    is_private: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::StringId;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::*;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;

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

    /// Helper to build a `StringId` → String map from staged `InternString` operations.
    /// This allows tests to assert the exact alias values.
    fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::InternString { local_id, value } = op {
                    Some((*local_id, value.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Helper to resolve a `StringId` to its string value using the staging operations.
    fn resolve_alias(
        alias: Option<&StringId>,
        string_map: &HashMap<StringId, String>,
    ) -> Option<String> {
        alias.as_ref().and_then(|id| string_map.get(id).cloned())
    }

    fn parse_elixir(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_elixir_sqry::language())
            .expect("Failed to load Elixir grammar");

        let content = source.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    fn print_tree_debug(node: tree_sitter::Node, source: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let text = node.utf8_text(source).unwrap_or("<invalid>");
        let text_preview = if text.len() > 30 {
            format!("{}...", &text[..30])
        } else {
            text.to_string()
        };
        eprintln!("{}{}: {:?}", indent, node.kind(), text_preview);

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_tree_debug(child, source, depth + 1);
        }
    }

    #[test]
    #[ignore = "Debug-only test for AST visualization"]
    fn test_debug_ast_elixir() {
        let source = r"alias Phoenix.Controller, as: Ctrl";
        let (tree, content) = parse_elixir(source);
        eprintln!("\n=== AST for 'alias Phoenix.Controller, as: Ctrl' ===");
        print_tree_debug(tree.root_node(), &content, 0);

        let source2 = r"alias Phoenix.{Socket, Channel}";
        let (tree2, content2) = parse_elixir(source2);
        eprintln!("\n=== AST for 'alias Phoenix.{{Socket, Channel}}' ===");
        print_tree_debug(tree2.root_node(), &content2, 0);
    }

    #[test]
    fn test_extract_public_function() {
        let source = r"
            def calculate(x, y) do
              x + y
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        assert_has_node(&staging, "calculate");
    }

    #[test]
    fn test_extract_private_function() {
        let source = r"
            defp internal_helper(data) do
              process(data)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        assert_has_node(&staging, "internal_helper");
    }

    #[test]
    fn test_extract_simple_call() {
        let source = r"
            def main(x) do
              helper(x)
            end

            def helper(y) do
              y
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected at least one call edge");
    }

    #[test]
    fn test_extract_erlang_ffi_call() {
        let source = r"
            def hash_password(password) do
              :crypto.hash(:sha256, password)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        // Erlang FFI calls (e.g. :crypto.hash) are currently emitted as Calls edges.
        // The is_erlang_ffi flag is extracted but not yet used to produce FfiCall edges.
        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected call edge for Erlang FFI call");
    }

    #[test]
    fn test_module_qualified_call() {
        let source = r#"
            def render_page(conn) do
              Phoenix.Controller.render(conn, "page.html")
            end
        "#;

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected module-qualified call edge");
    }

    #[test]
    fn test_pipe_operator_chain() {
        let source = r"
            def process_data(data) do
              data
              |> Enum.map(&transform/1)
              |> Enum.filter(&valid?/1)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected pipe operator call edges");
    }

    #[test]
    fn test_argument_count_two_args() {
        let source = r"
            def two(a, b) do
              helper(a, b)
            end

            def helper(a, b) do
              a + b
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let calls = collect_call_edges(&staging);
        assert!(!calls.is_empty(), "Expected call edge to helper");
    }

    // ============================================================================
    // Import Edge Tests (Wave 7)
    // ============================================================================

    #[test]
    fn test_import_edge_simple() {
        let source = r"
            defmodule MyModule do
              import Enum
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected at least one import edge"
        );

        // Simple import without `only:` should be wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                *is_wildcard,
                "Simple import should be wildcard (imports all)"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_import_edge_with_only() {
        let source = r"
            defmodule MyModule do
              import List, only: [first: 1, last: 1]
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected import edge with only clause"
        );

        // Import with `only:` should NOT be wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "Import with only: clause should NOT be wildcard"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_alias_edge() {
        let source = r"
            defmodule MyModule do
              alias Phoenix.Controller
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected alias edge");

        // Build string map to resolve alias values
        let string_map = build_string_map(&staging);

        // Alias without `as:` should have default alias (last segment: Controller)
        // Alias is NOT wildcard - it creates a named reference
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { alias, is_wildcard } = edge {
            assert!(
                !*is_wildcard,
                "Alias should NOT be wildcard (it's a reference)"
            );
            // Assert the exact alias value
            let alias_value = resolve_alias(alias.as_ref(), &string_map);
            assert_eq!(
                alias_value,
                Some("Controller".to_string()),
                "Default alias should be 'Controller' (last segment)"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_alias_with_as() {
        let source = r"
            defmodule MyModule do
              alias Phoenix.Controller, as: Ctrl
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected alias edge with as");

        // Build string map to resolve alias values
        let string_map = build_string_map(&staging);

        // Alias with `as:` should have explicit alias set
        // Alias is NOT wildcard - it creates a named reference
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { alias, is_wildcard } = edge {
            assert!(
                !*is_wildcard,
                "Alias should NOT be wildcard (it's a reference)"
            );
            // Assert the exact alias value
            let alias_value = resolve_alias(alias.as_ref(), &string_map);
            assert_eq!(
                alias_value,
                Some("Ctrl".to_string()),
                "Explicit alias should be 'Ctrl'"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_multi_alias_expansion() {
        let source = r"
            defmodule MyModule do
              alias Phoenix.{Socket, Channel}
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);

        // Should emit two edges: one for Socket, one for Channel
        assert_eq!(
            import_edges.len(),
            2,
            "Multi-alias should emit one edge per alias element"
        );

        // Build string map to resolve alias values
        let string_map = build_string_map(&staging);

        // Extract alias values and verify
        let mut alias_values: Vec<String> = import_edges
            .iter()
            .filter_map(|edge| {
                if let UnifiedEdgeKind::Imports { alias, is_wildcard } = edge {
                    // Each element should NOT be wildcard
                    assert!(!*is_wildcard, "Multi-alias elements should NOT be wildcard");
                    resolve_alias(alias.as_ref(), &string_map)
                } else {
                    None
                }
            })
            .collect();

        alias_values.sort();
        assert_eq!(
            alias_values,
            vec!["Channel".to_string(), "Socket".to_string()],
            "Multi-alias should expand to individual aliases"
        );
    }

    #[test]
    fn test_use_edge() {
        let source = r"
            defmodule MyModule do
              use GenServer
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected use edge");

        // Use statement should be wildcard (brings in all behavior)
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(*is_wildcard, "use statement should be wildcard");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_require_edge() {
        let source = r"
            defmodule MyModule do
              require Logger
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected require edge");

        // Require statement is NOT wildcard - it just makes macros available for compile-time
        // but doesn't import all symbols into the namespace like `import` does
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "require statement should NOT be wildcard (only makes macros available)"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_multiple_imports() {
        let source = r"
            defmodule MyModule do
              import Enum
              import List
              alias Phoenix.Controller
              use GenServer
              require Logger
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        // Extract all import edges and validate count
        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            5,
            "Expected 5 import edges (import Enum, import List, alias, use, require)"
        );

        // Verify all are EdgeKind::Imports
        for edge in &import_edges {
            assert!(
                matches!(edge, UnifiedEdgeKind::Imports { .. }),
                "All edges should be Imports"
            );
        }
    }

    // ============================================================================
    // Export Edge Tests
    // ============================================================================

    /// Helper to extract Export edges from staging operations
    fn extract_export_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Exports { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    #[test]
    fn test_export_public_function() {
        let source = r"
            defmodule Visibility do
              def public_fun do
                :ok
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            1,
            "Expected one export edge for public function"
        );

        // Verify the export edge has correct kind
        let edge = export_edges[0];
        if let UnifiedEdgeKind::Exports { kind, alias } = edge {
            assert_eq!(
                *kind,
                ExportKind::Direct,
                "Public function export should be ExportKind::Direct"
            );
            assert!(
                alias.is_none(),
                "Public function export should not have alias"
            );
        } else {
            panic!("Expected Exports edge kind");
        }
    }

    #[test]
    fn test_export_multiple_public_functions() {
        let source = r"
            defmodule MyModule do
              def function_one do
                :ok
              end

              def function_two do
                :ok
              end

              def function_three(x) do
                x * 2
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            3,
            "Expected three export edges for three public functions"
        );

        // All exports should be Direct with no alias
        for edge in export_edges {
            if let UnifiedEdgeKind::Exports { kind, alias } = edge {
                assert_eq!(*kind, ExportKind::Direct);
                assert!(alias.is_none());
            } else {
                panic!("Expected Exports edge kind");
            }
        }
    }

    #[test]
    fn test_no_export_for_private_function() {
        let source = r"
            defmodule Secret do
              defp private_fun do
                :secret
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            0,
            "Expected no export edges for private function"
        );
    }

    #[test]
    fn test_export_mixed_public_private() {
        let source = r"
            defmodule Mixed do
              def public_one, do: :ok

              defp private_one, do: :secret

              def public_two, do: :ok

              defp private_two, do: :secret
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            2,
            "Expected two export edges for two public functions (defp should not be exported)"
        );

        // All exports should be Direct with no alias
        for edge in export_edges {
            if let UnifiedEdgeKind::Exports { kind, alias } = edge {
                assert_eq!(*kind, ExportKind::Direct);
                assert!(alias.is_none());
            } else {
                panic!("Expected Exports edge kind");
            }
        }
    }

    #[test]
    fn test_export_public_macro() {
        let source = r"
            defmodule Macros do
              defmacro public_macro do
                quote do: :ok
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            1,
            "Expected one export edge for public macro"
        );

        // Verify the export edge has correct kind
        let edge = export_edges[0];
        if let UnifiedEdgeKind::Exports { kind, alias } = edge {
            assert_eq!(
                *kind,
                ExportKind::Direct,
                "Public macro export should be ExportKind::Direct"
            );
            assert!(alias.is_none(), "Public macro export should not have alias");
        } else {
            panic!("Expected Exports edge kind");
        }
    }

    #[test]
    fn test_no_export_for_private_macro() {
        let source = r"
            defmodule SecretMacros do
              defmacrop private_macro do
                quote do: :secret
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            0,
            "Expected no export edges for private macro"
        );
    }

    #[test]
    fn test_export_mixed_functions_and_macros() {
        let source = r"
            defmodule MixedTypes do
              def public_fun, do: :ok
              defp private_fun, do: :secret
              defmacro public_macro, do: quote(do: :ok)
              defmacrop private_macro, do: quote(do: :secret)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert_eq!(
            export_edges.len(),
            2,
            "Expected two export edges (one public function, one public macro)"
        );

        // All exports should be Direct with no alias
        for edge in export_edges {
            if let UnifiedEdgeKind::Exports { kind, alias } = edge {
                assert_eq!(*kind, ExportKind::Direct);
                assert!(alias.is_none());
            } else {
                panic!("Expected Exports edge kind");
            }
        }
    }

    // ============================================================================
    // FFI Edge Tests (Erlang NIF)
    // ============================================================================

    /// Helper to extract FFI edges from staging operations
    fn extract_ffi_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::FfiCall { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    #[test]
    fn test_nif_basic_loading() {
        let source = r"
            defmodule MyNif do
              @on_load :load_nifs

              def load_nifs do
                :erlang.load_nif('./priv/my_nif', 0)
              end

              def native_function(_arg), do: :erlang.nif_error(:not_loaded)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 1, "Expected one FFI edge");

        // Verify convention is C
        if let UnifiedEdgeKind::FfiCall { convention } = ffi_edges[0] {
            assert_eq!(
                *convention,
                sqry_core::graph::unified::edge::kind::FfiConvention::C,
                "NIF calls should use C convention"
            );
        } else {
            panic!("Expected FfiCall edge");
        }
    }

    #[test]
    fn test_nif_inline_call() {
        let source = r"
            defmodule SimpleNif do
              def init do
                :erlang.load_nif('./lib', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 1, "Expected one FFI edge for inline call");
    }

    #[test]
    fn test_nif_without_on_load() {
        let source = r"
            defmodule NoOnLoad do
              def init do
                :erlang.load_nif('./nif_lib', 0)
              end

              def compute(_x), do: :erlang.nif_error(:not_loaded)
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF loading without @on_load"
        );
    }

    #[test]
    fn test_nif_without_stubs() {
        let source = r"
            defmodule NoStubs do
              @on_load :init

              def init do
                :erlang.load_nif('./minimal', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF loading without stub functions"
        );
    }

    #[test]
    fn test_nif_minimal() {
        let source = r"
            defmodule Minimal do
              def go do
                :erlang.load_nif('./x', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 1, "Minimal NIF loading should be detected");
    }

    #[test]
    fn test_nif_multiple_calls() {
        let source = r"
            defmodule MultiNif do
              def load_crypto do
                :erlang.load_nif('./crypto_nif', 0)
              end

              def load_math do
                :erlang.load_nif('./math_nif', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            2,
            "Should detect multiple NIF loading calls"
        );
    }

    #[test]
    fn test_nif_string_path() {
        let source = r#"
            defmodule StringPath do
              def init do
                :erlang.load_nif("./my_lib", 0)
              end
            end
        "#;

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF with string path (double quotes)"
        );
    }

    #[test]
    fn test_nif_charlist_path() {
        let source = r"
            defmodule CharlistPath do
              def init do
                :erlang.load_nif('./path', [])
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF with charlist path (single quotes)"
        );
    }

    #[test]
    fn test_nif_variable_init_args() {
        let source = r"
            defmodule VariableArgs do
              def init(args) do
                :erlang.load_nif('./lib', args)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF with variable init args"
        );
    }

    #[test]
    fn test_nif_private_function() {
        let source = r"
            defmodule PrivateLoader do
              defp load_nif do
                :erlang.load_nif('./private', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF in private function (defp)"
        );
    }

    #[test]
    fn test_nif_public_function() {
        let source = r"
            defmodule PublicLoader do
              def load_nif do
                :erlang.load_nif('./public', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF in public function (def)"
        );
    }

    #[test]
    fn test_nif_nested_module() {
        let source = r"
            defmodule Outer do
              defmodule Inner do
                def init do
                  :erlang.load_nif('./inner_nif', 0)
                end
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 1, "Should detect NIF in nested module");
    }

    #[test]
    fn test_nif_convention_is_c() {
        let source = r"
            defmodule ConventionTest do
              def init do
                :erlang.load_nif('./lib', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert!(!ffi_edges.is_empty(), "Expected at least one FFI edge");

        for edge in ffi_edges {
            if let UnifiedEdgeKind::FfiCall { convention } = edge {
                assert_eq!(
                    *convention,
                    sqry_core::graph::unified::edge::kind::FfiConvention::C,
                    "All NIF edges should use C convention"
                );
            }
        }
    }

    #[test]
    fn test_nif_edge_count() {
        let source = r"
            defmodule EdgeCount do
              def one do
                :erlang.load_nif('./one', 0)
              end

              def two do
                :erlang.load_nif('./two', 0)
              end

              def three do
                :erlang.load_nif('./three', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            3,
            "Should create exactly one edge per load_nif call"
        );
    }

    #[test]
    #[allow(clippy::similar_names)] // Domain variable naming is intentional
    fn test_nif_edge_endpoints() {
        let source = r"
            defmodule NifModule do
              def load_nif do
                :erlang.load_nif('./mylib', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        // Verify FfiCall edge exists
        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 1, "Expected exactly one FfiCall edge");

        // Verify convention is C
        if let UnifiedEdgeKind::FfiCall { convention } = ffi_edges[0] {
            assert_eq!(
                *convention,
                sqry_core::graph::unified::edge::kind::FfiConvention::C,
                "NIF calls should use C convention"
            );
        } else {
            panic!("Expected FfiCall edge");
        }

        // Extract all nodes to find caller and callee by name
        let mut caller_node_id: Option<NodeId> = None;
        #[allow(clippy::similar_names)] // AST node variables
        let mut callee_node_id: Option<NodeId> = None;

        for op in staging.operations() {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let canonical_name = staging
                    .resolve_node_canonical_name(entry)
                    .expect("Node name should resolve");

                // Find caller node (should be "load_nif" function, not the FFI target)
                if canonical_name == "load_nif"
                    && matches!(entry.kind, sqry_core::graph::unified::NodeKind::Function)
                {
                    caller_node_id = *expected_id;
                }

                // Find callee node by its canonical graph identity.
                if canonical_name == "ffi::erlang::load_nif" {
                    callee_node_id = *expected_id;
                }
            }
        }

        // Verify we found both nodes
        assert!(
            caller_node_id.is_some(),
            "Expected to find caller node named 'load_nif'"
        );
        assert!(
            callee_node_id.is_some(),
            "Expected to find callee node named 'ffi::erlang::load_nif'"
        );

        let caller_id = caller_node_id.unwrap();
        let callee_id = callee_node_id.unwrap();

        // Verify that the FfiCall edge connects these specific nodes
        let has_correct_edge = staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                matches!(kind, UnifiedEdgeKind::FfiCall { .. })
                    && *source == caller_id
                    && *target == callee_id
            } else {
                false
            }
        });

        assert!(
            has_correct_edge,
            "Expected FfiCall edge connecting NifModule::load_nif to ffi::erlang::load_nif"
        );
    }

    // Negative test cases

    #[test]
    fn test_no_ffi_regular_erlang_call() {
        let source = r"
            defmodule MyModule do
              def process(list) do
                :lists.map(fn x -> x * 2 end, list)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Should not detect regular Erlang calls as FFI"
        );
    }

    #[test]
    fn test_no_ffi_comment() {
        let source = r"
            defmodule CommentTest do
              # :erlang.load_nif('./commented', 0)
              def init do
                :ok
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 0, "Should not detect load_nif in comments");
    }

    #[test]
    fn test_no_ffi_string_literal() {
        let source = r#"
            defmodule StringTest do
              def message do
                "Call :erlang.load_nif to load"
              end
            end
        "#;

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Should not detect load_nif in string literals"
        );
    }

    #[test]
    fn test_no_ffi_similar_name() {
        let source = r"
            defmodule SimilarName do
              def init do
                :erlang.load_nif_module('./lib', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Should not detect similar function names (load_nif_module)"
        );
    }

    #[test]
    fn test_no_ffi_wrong_module() {
        let source = r"
            defmodule WrongModule do
              def init do
                :other.load_nif('./lib', 0)
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Should not detect load_nif from modules other than :erlang"
        );
    }

    // Edge case tests

    #[test]
    fn test_nif_malformed_incomplete_args() {
        let source = r"
            defmodule Malformed do
              def init do
                :erlang.load_nif()
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        // Should not crash, even with malformed call
        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        // May or may not detect - depends on tree-sitter parsing
        // Just ensure no panic
        let _ffi_edges = extract_ffi_edges(&staging);
    }

    #[test]
    fn test_nif_empty_arguments() {
        let source = r"
            defmodule EmptyArgs do
              def init do
                :erlang.load_nif('./lib')
              end
            end
        ";

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        // Should not crash with missing second argument
        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        // Should still detect even with non-standard arity
        assert!(
            ffi_edges.len() <= 1,
            "Should handle NIF calls with non-standard arity gracefully"
        );
    }

    #[test]
    fn test_nif_complex_path() {
        let source = r#"
            defmodule ComplexPath do
              def init(base_path) do
                :erlang.load_nif(base_path <> "/nif", 0)
              end
            end
        "#;

        let (tree, content) = parse_elixir(source);
        let mut staging = StagingGraph::new();
        let builder = ElixirGraphBuilder::default();

        // Should handle path interpolation without crashing
        builder
            .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            1,
            "Should detect NIF with complex/interpolated paths"
        );
    }
}

/// Per-language [`ShapeMapping`] for Elixir (identifier-blind body-shape feature).
///
/// Precomputed `kind_id -> CfBucket` table built once from the vendored
/// tree-sitter-elixir-sqry grammar (whose `language()` returns a `Language`
/// directly). Elixir's grammar is macro-uniform: `if`/`case`/`cond`/`with`/`for`/
/// `try` are all plain `call` nodes whose head is an identifier, so they cannot be
/// bucketed without reading that identifier (which would break
/// identifier-blindness). The honest, structural control flow Elixir exposes is
/// clause-based pattern matching (`stab_clause` = the `->` arms of case/cond/with/
/// fn) plus the block kinds (`rescue_block`/`catch_block`/`after_block`), the
/// anonymous-function literal, and the `call`. Pattern-match clauses therefore map
/// to `Match`, the rescue/catch blocks to `Catch`, the after block to `Resource`,
/// and `fn ... end` to `Closure`.
pub struct ElixirShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl ElixirShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_elixir_sqry::language();
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
                *slot = cf_bucket_for_elixir_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for ElixirShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        // `def name(args) do ... end` parses as a `call` whose first argument is
        // the function-head `call` (`name(args)`); the head's `arguments` node
        // holds the formal parameters. Read positional arity from there; fall back
        // to the default when the structure does not match.
        let mut shape = SignatureShape::default();
        let mut cursor = fn_node.walk();
        let head = fn_node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "arguments")
            .and_then(|args| {
                let mut ac = args.walk();
                args.named_children(&mut ac).find(|c| c.kind() == "call")
            });
        if let Some(head) = head {
            let mut hc = head.walk();
            if let Some(params) = head
                .named_children(&mut hc)
                .find(|c| c.kind() == "arguments")
            {
                let mut pc = params.walk();
                shape.arity_positional =
                    u16::try_from(params.named_children(&mut pc).count()).unwrap_or(u16::MAX);
            }
        }
        shape
    }
}

/// Map one tree-sitter-elixir-sqry node-kind name to its canonical control-flow
/// bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_elixir_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        // Every `->` clause of case/cond/with/fn is a pattern-match arm.
        "stab_clause" => CfBucket::Match,
        "rescue_block" | "catch_block" => CfBucket::Catch,
        "after_block" => CfBucket::Resource,
        "anonymous_function" => CfBucket::Closure,
        "call" => CfBucket::Call,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Elixir shape mapping, built once on first use.
#[must_use]
pub fn elixir_shape_mapping() -> &'static ElixirShapeMapping {
    static MAPPING: OnceLock<ElixirShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(ElixirShapeMapping::build)
}

#[cfg(test)]
mod shape_tests {
    //! Coverage for the Elixir [`ShapeMapping`]. Consumes the hand-written
    //! control-flow fixture so the test is load-bearing.

    use super::{cf_bucket_for_elixir_kind, elixir_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };
    use tree_sitter::{Node, Parser, Tree};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/dynamic/sample.ex"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_elixir_sqry::language())
            .expect("load elixir grammar");
        parser.parse(src, None).expect("parse elixir")
    }

    /// The `def classify(...)` head: the first `call` whose target identifier the
    /// grammar exposes as a `def` form. We locate it structurally as the first
    /// `call` that has a `do_block` and whose first argument is itself a `call`
    /// (the function head), which is exactly the `def classify(...) do ... end`.
    fn classify_def<'t>(tree: &'t Tree) -> Node<'t> {
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call" {
                let mut c = node.walk();
                let kids: Vec<Node> = node.named_children(&mut c).collect();
                let has_do = kids.iter().any(|k| k.kind() == "do_block");
                let head_is_call = kids
                    .iter()
                    .find(|k| k.kind() == "arguments")
                    .map(|args| {
                        let mut ac = args.walk();
                        args.named_children(&mut ac).any(|c| c.kind() == "call")
                    })
                    .unwrap_or(false);
                if has_do && head_is_call {
                    return node;
                }
            }
            let mut cursor = node.walk();
            let mut children: Vec<Node> = node.named_children(&mut cursor).collect();
            children.reverse();
            stack.extend(children);
        }
        panic!("no def-with-head call in elixir fixture");
    }

    #[test]
    fn mapping_is_non_empty_and_covers_real_kinds() {
        assert_eq!(
            cf_bucket_for_elixir_kind("stab_clause"),
            Some(CfBucket::Match)
        );
        assert_eq!(
            cf_bucket_for_elixir_kind("rescue_block"),
            Some(CfBucket::Catch)
        );
        assert_eq!(
            cf_bucket_for_elixir_kind("catch_block"),
            Some(CfBucket::Catch)
        );
        assert_eq!(
            cf_bucket_for_elixir_kind("after_block"),
            Some(CfBucket::Resource)
        );
        assert_eq!(
            cf_bucket_for_elixir_kind("anonymous_function"),
            Some(CfBucket::Closure)
        );
        assert_eq!(cf_bucket_for_elixir_kind("call"), Some(CfBucket::Call));
        assert_eq!(cf_bucket_for_elixir_kind("nope"), None);

        let lang: tree_sitter::Language = tree_sitter_elixir_sqry::language();
        let id = (0..lang.node_kind_count())
            .map(|i| i as u16)
            .find(|&i| {
                lang.node_kind_is_named(i) && lang.node_kind_for_id(i) == Some("stab_clause")
            })
            .expect("grammar exposes named stab_clause");
        assert_eq!(elixir_shape_mapping().cf_bucket(id), Some(CfBucket::Match));
    }

    #[test]
    fn descriptor_covers_fixture_control_flow() {
        let tree = parse(SAMPLE);
        let func = classify_def(&tree);
        let descriptor = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            elixir_shape_mapping(),
            &ShapeBudget::default(),
        );
        let hist = descriptor.cf_histogram;
        // case/cond/with/fn clauses all surface as pattern-match arms.
        assert!(hist[CfBucket::Match.index()] >= 1, "stab clauses");
        assert!(hist[CfBucket::Catch.index()] >= 1, "rescue/catch block");
        assert!(hist[CfBucket::Resource.index()] >= 1, "after block");
        assert!(hist[CfBucket::Closure.index()] >= 1, "anonymous function");
        assert!(hist[CfBucket::Call.index()] >= 1, "call");
    }

    #[test]
    fn signature_shape_reads_def_head_arity() {
        let tree = parse(SAMPLE);
        let func = classify_def(&tree);
        let shape = elixir_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // `def classify(value, label \\ "n/a", rest \\ [])` has three head params.
        assert_eq!(shape.arity_positional, 3, "value + label + rest");
    }
}
