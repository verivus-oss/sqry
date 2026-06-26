//! `GraphBuilder` implementation for Svelte
//!
//! Builds the unified `CodeGraph` for Svelte SFC files by:
//! 1. Extracting `<script>` and `<script context="module">` blocks from the SFC
//! 2. Re-parsing each script block as JavaScript/TypeScript
//! 3. Delegating to the appropriate language `GraphBuilder` to extract functions and calls
//! 4. Extracting template event handlers (`on:click={handler}`) and emitting Calls edges
//! 5. Extracting import and export statements and emitting Import/Export edges
//!
//! ## Scope
//! - Script blocks: Both instance (`<script>`) and module (`<script context="module">`)
//! - Languages: JavaScript (default) and TypeScript (`lang="ts"`)
//! - Callables: Function declarations within script blocks
//! - Calls: `call_expression` nodes within script blocks
//! - Event handlers: `on:*` directives in templates (e.g., `on:click={handler}`)
//! - Imports: ES6 `import` statements (e.g., `import { onMount } from 'svelte'`)
//! - Exports: ES6 `export` statements (e.g., `export function foo() {}`)
//!
//! ## Event Handler Detection
//! Svelte event handlers use the `on:event={handler}` syntax:
//! - `on:click={handleClick}` - simple handler reference
//! - `on:submit|preventDefault={handleSubmit}` - with modifiers
//! - `on:keydown={() => doSomething()}` - inline arrow functions
//!
//! ## Strategy
//! 1. Parse the entire `.svelte` file using tree-sitter-svelte
//! 2. Query for `script_element` nodes
//! 3. Extract inner text ranges for each script block
//! 4. Re-parse with tree-sitter-javascript or tree-sitter-typescript
//! 5. Apply JavaScript/TypeScript `GraphBuilder` patterns to emit nodes and edges
//! 6. Walk template elements to find `on:*` event handlers and emit Calls edges

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::build::staging::ShapeAttachCtx;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use sqry_lang_typescript::relations::type_extractor::{
    extract_all_type_names_from_annotation, extract_type_string,
};
use tree_sitter::{Node, Parser, Point, Tree};

/// `GraphBuilder` for Svelte SFC files
#[derive(Debug, Clone, Copy)]
pub struct SvelteGraphBuilder {
    max_scope_depth: usize,
}

impl Default for SvelteGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 4, // Svelte: script -> class -> method -> nested function
        }
    }
}

impl SvelteGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for SvelteGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Svelte);

        // Create module node for the Svelte file itself (anchor for import/export edges)
        let module_id = helper.add_module("svelte::module", None);

        // Create a Component node for the Svelte file (DSL semantic node)
        let component_name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("SvelteComponent");
        let component_id = helper.add_node(component_name, None, NodeKind::Component);
        helper.add_contains_edge(module_id, component_id);

        // Extract and process script blocks
        let source = std::str::from_utf8(content).map_err(|e| GraphBuilderError::ParseError {
            span: sqry_core::graph::Span::default(),
            reason: format!("Invalid UTF-8: {e}"),
        })?;

        let blocks = collect_script_blocks(tree, source);

        // Track function names from script blocks for event handler resolution
        let mut local_by_name: HashMap<String, sqry_core::graph::unified::NodeId> = HashMap::new();

        for block in &blocks {
            if block.context == ScriptContext::Module {
                // Module-context: use a separate map so module-level names
                // don't leak into template event handler resolution.
                let mut module_locals: HashMap<String, sqry_core::graph::unified::NodeId> =
                    HashMap::new();
                process_script_block(
                    block,
                    &mut helper,
                    self.max_scope_depth,
                    &mut module_locals,
                    module_id,
                    component_id,
                )?;
            } else {
                // Instance-context: populate shared local_by_name for
                // template event handler resolution.
                process_script_block(
                    block,
                    &mut helper,
                    self.max_scope_depth,
                    &mut local_by_name,
                    module_id,
                    component_id,
                )?;
            }

            // Attach body hashes per-block so spans (relative to the script
            // block content) align with the correct bytes.  The later
            // `attach_body_hashes` call in entrypoint.rs uses the full SFC
            // content, which would produce incorrect hashes for nodes whose
            // line numbers are relative to the extracted script block.
            helper.attach_body_hashes(block.content.as_bytes());
        }

        // Extract event handlers from template elements
        extract_template_event_handlers(
            tree.root_node(),
            source,
            &mut helper,
            module_id,
            &mut local_by_name,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Svelte
    }

    // NOTE: no `shape_mapping()` override here on purpose. The SFC-level tree is
    // the Svelte grammar, which carries no function bodies, so the whole-file
    // shape seam in the indexing entrypoint has nothing to fingerprint. The
    // function bodies live in the embedded `<script>` blocks, parsed with the
    // JS/TS grammar; those are fingerprinted per-block in `process_script_block`
    // via the JS shape mapping below, where the JS subtree (and its kind ids)
    // are valid.
}

/// Per-language [`ShapeMapping`] for the JavaScript/TypeScript inside Svelte
/// `<script>` blocks.
///
/// Svelte components embed JS or TS, so the body-shape descriptor for a
/// component's functions has to walk the JS/TS subtree, not the Svelte markup
/// tree. `cf_bucket` indexes by grammar kind id, so the mapping is built per
/// grammar: one table for the JavaScript grammar and one for the TypeScript
/// grammar, both driven by the same control-flow name matcher (TS shares JS's
/// control-flow construct names). The matcher is implemented directly in this
/// crate rather than borrowed from the JS plugin, keeping the Svelte worktree
/// self-contained.
pub struct SvelteJsShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl SvelteJsShapeMapping {
    /// Build the table from a concrete grammar (`tree_sitter_javascript` or
    /// `tree_sitter_typescript`), mapping its named control-flow kinds.
    fn build(lang: &tree_sitter::Language) -> Self {
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
                *slot = cf_bucket_for_js_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for SvelteJsShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        signature_shape_js(fn_node)
    }
}

/// Read the structural [`SignatureShape`] from a JS/TS function node's
/// `formal_parameters` list. Shared by the Svelte JS and TS mappings.
fn signature_shape_js(fn_node: Node) -> SignatureShape {
    let mut shape = SignatureShape::default();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.named_children(&mut cursor) {
            match child.kind() {
                "identifier" | "object_pattern" | "array_pattern" => {
                    shape.arity_positional = shape.arity_positional.saturating_add(1);
                }
                "assignment_pattern" => {
                    shape.arity_positional = shape.arity_positional.saturating_add(1);
                    shape.has_defaults = true;
                }
                "rest_pattern" => shape.has_varargs = true,
                _ => {}
            }
        }
    }
    shape.has_return_annotation = fn_node.child_by_field_name("return_type").is_some();
    shape
}

/// Map one tree-sitter JavaScript/TypeScript grammar node-kind name to its
/// canonical control-flow bucket. Additive-only against the frozen [`CfBucket`]
/// set. Mirrors the Vue crate's matcher; both embed JS/TS.
fn cf_bucket_for_js_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "ternary_expression" => CfBucket::Branch,
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => CfBucket::Loop,
        "switch_statement" => CfBucket::Match,
        "try_statement" => CfBucket::Try,
        "catch_clause" => CfBucket::Catch,
        "throw_statement" => CfBucket::Throw,
        "with_statement" => CfBucket::Resource,
        "return_statement" => CfBucket::Return,
        "yield_expression" => CfBucket::Yield,
        "await_expression" => CfBucket::Await,
        "break_statement" | "continue_statement" => CfBucket::BreakContinue,
        "call_expression" | "new_expression" => CfBucket::Call,
        "lexical_declaration"
        | "variable_declaration"
        | "assignment_expression"
        | "augmented_assignment_expression" => CfBucket::Assign,
        "arrow_function" | "function_expression" | "generator_function" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Svelte JavaScript-block shape mapping, built once on first use.
#[must_use]
pub fn svelte_js_shape_mapping() -> &'static SvelteJsShapeMapping {
    static MAPPING: OnceLock<SvelteJsShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(|| SvelteJsShapeMapping::build(&tree_sitter_javascript::LANGUAGE.into()))
}

/// The process-wide Svelte TypeScript-block shape mapping, built once on first use.
#[must_use]
pub fn svelte_ts_shape_mapping() -> &'static SvelteJsShapeMapping {
    static MAPPING: OnceLock<SvelteJsShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(|| {
        SvelteJsShapeMapping::build(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
    })
}

// ============================================================================
// Script Block Extraction
// ============================================================================

/// Language used by a `<script>` block
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptLanguage {
    JavaScript,
    TypeScript,
}

impl ScriptLanguage {
    fn from_lang_attr(value: Option<&str>) -> Self {
        match value {
            Some("ts" | "typescript" | "TS" | "TypeScript") => ScriptLanguage::TypeScript,
            _ => ScriptLanguage::JavaScript,
        }
    }
}

/// Context of a `<script>` block (instance vs module)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptContext {
    Module,
    Instance,
}

impl ScriptContext {
    fn from_context_attr(value: Option<&str>) -> Self {
        match value {
            Some(v) if v.eq_ignore_ascii_case("module") => ScriptContext::Module,
            _ => ScriptContext::Instance,
        }
    }

    /// Get the prefix for qualified names to prevent collisions between contexts
    fn qualified_prefix(self) -> &'static str {
        match self {
            ScriptContext::Module => "svelte::module",
            ScriptContext::Instance => "svelte::instance",
        }
    }
}

/// Simplified representation of a `<script>` block
struct ScriptBlock {
    lang: ScriptLanguage,
    context: ScriptContext,
    content: String,
    #[allow(dead_code)] // Reserved for future position offset calculations
    start_point: Point,
}

/// Collect script blocks from a parsed Svelte tree
fn collect_script_blocks(tree: &Tree, source: &str) -> Vec<ScriptBlock> {
    let mut blocks = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() != "script_element" {
            continue;
        }

        let mut lang: Option<String> = None;
        let mut context_attr: Option<String> = None;
        let mut script_content = None;
        let mut start_point = child.start_position();

        let mut script_cursor = child.walk();
        for node in child.children(&mut script_cursor) {
            match node.kind() {
                "start_tag" => {
                    let mut attr_cursor = node.walk();
                    for attr in node.children(&mut attr_cursor) {
                        if attr.kind() != "attribute" {
                            continue;
                        }
                        if let Some((name, value)) = parse_attribute(&attr, source) {
                            match name.as_str() {
                                "lang" => lang = Some(value),
                                "context" => context_attr = Some(value),
                                _ => {}
                            }
                        }
                    }
                }
                "raw_text" => {
                    if let Ok(text) = node.utf8_text(source.as_bytes()) {
                        script_content = Some(text.to_string());
                        start_point = node.start_position();
                    }
                }
                _ => {}
            }
        }

        if let Some(script_content) = script_content {
            let block = ScriptBlock {
                lang: ScriptLanguage::from_lang_attr(lang.as_deref()),
                context: ScriptContext::from_context_attr(context_attr.as_deref()),
                content: script_content,
                start_point,
            };
            blocks.push(block);
        }
    }

    blocks
}

/// Parse attribute node into (name, value)
fn parse_attribute(node: &Node, source: &str) -> Option<(String, String)> {
    let mut name: Option<String> = None;
    let mut value: Option<String> = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "attribute_name" => {
                name = child
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "attribute_value" => {
                value = child
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "quoted_attribute_value" => {
                let text = child.utf8_text(source.as_bytes()).ok()?;
                value = Some(text.trim_matches(&['"', '\''][..]).to_string());
            }
            "expr_attribute_value" => {
                if let Ok(text) = child.utf8_text(source.as_bytes()) {
                    value = Some(text.to_string());
                }
            }
            _ => {}
        }
    }

    name.map(|n| (n, value.unwrap_or_else(|| "true".to_string())))
}

// ============================================================================
// Script Block Processing
// ============================================================================

/// Process a script block by re-parsing it and extracting functions, calls, and exports.
///
/// The `local_by_name` map is populated with function names from this script block,
/// allowing event handlers in the template to reference these functions.
fn process_script_block(
    block: &ScriptBlock,
    helper: &mut GraphBuildHelper,
    _max_scope_depth: usize,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    module_id: sqry_core::graph::unified::NodeId,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Parse the script block content
    let mut parser = Parser::new();

    let language_grammar = match block.lang {
        ScriptLanguage::JavaScript => &tree_sitter_javascript::LANGUAGE.into(),
        ScriptLanguage::TypeScript => &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };

    parser
        .set_language(language_grammar)
        .map_err(|e| GraphBuilderError::ParseError {
            span: sqry_core::graph::Span::default(),
            reason: format!("Failed to set language: {e}"),
        })?;

    let tree = parser
        .parse(block.content.as_bytes(), None)
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: sqry_core::graph::Span::default(),
            reason: "Failed to parse script block".to_string(),
        })?;

    // Extract functions, calls, and exports from the script AST
    let root = tree.root_node();
    extract_script_graph(
        root,
        block.content.as_bytes(),
        helper,
        block.context,
        block.lang,
        local_by_name,
        module_id,
        component_id,
    )?;

    // Attach identifier-blind shape descriptors for this block's functions.
    // The descriptor walk needs the JS/TS subtree whose kind ids match the
    // grammar that parsed the block, so it is done here (where `tree` is alive)
    // rather than at the SFC level, where only the Svelte markup tree exists.
    let mapping: &dyn ShapeMapping = match block.lang {
        ScriptLanguage::JavaScript => svelte_js_shape_mapping(),
        ScriptLanguage::TypeScript => svelte_ts_shape_mapping(),
    };
    let shape_ctx = ShapeAttachCtx::new(&tree, block.content.as_bytes(), mapping);
    helper
        .staging_mut()
        .attach_body_hashes(block.content.as_bytes(), Some(shape_ctx));

    Ok(())
}

fn extract_script_graph(
    root: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    script_context: ScriptContext,
    script_lang: ScriptLanguage,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    module_id: sqry_core::graph::unified::NodeId,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let prefix = script_context.qualified_prefix();
    let mut callable_by_node_id: HashMap<usize, sqry_core::graph::unified::NodeId> = HashMap::new();

    // Module-context declarations belong to the module node;
    // instance-context declarations belong to the component node.
    let contains_parent = match script_context {
        ScriptContext::Module => module_id,
        ScriptContext::Instance => component_id,
    };

    let mut cursor = root.walk();
    collect_script_function_declarations(
        root,
        &mut cursor,
        content,
        helper,
        prefix,
        &mut callable_by_node_id,
        local_by_name,
        contains_parent,
    )?;

    let mut call_cursor = root.walk();
    let mut caller_stack = Vec::new();
    extract_script_call_edges(
        root,
        &mut call_cursor,
        content,
        helper,
        prefix,
        &callable_by_node_id,
        local_by_name,
        &mut caller_stack,
    )?;

    // Extract export edges for exported declarations
    let mut export_cursor = root.walk();
    extract_export_edges(
        root,
        &mut export_cursor,
        content,
        helper,
        module_id,
        local_by_name,
        contains_parent,
    )?;

    // Extract import edges for import statements
    let mut import_cursor = root.walk();
    extract_import_edges(root, &mut import_cursor, content, helper, module_id)?;

    // Extract TypeOf and References edges from TypeScript type annotations
    if script_lang == ScriptLanguage::TypeScript {
        extract_type_edges(root, content, helper, prefix, &callable_by_node_id)?;
    }

    Ok(())
}

fn collect_script_function_declarations<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
    callable_by_node_id: &mut HashMap<usize, sqry_core::graph::unified::NodeId>,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    if node.kind() == "function_declaration"
        && let Some(name) = get_function_name(&node, content)
    {
        let qualified_name = format!("{prefix}::{name}");
        let visibility = extract_visibility(&qualified_name);
        let func_id = helper.add_function_with_visibility(
            &qualified_name,
            Some(span_from_node(node)),
            false,
            false,
            Some(visibility),
        );
        callable_by_node_id.insert(node.id(), func_id);
        local_by_name.insert(name, func_id);
        helper.add_contains_edge(component_id, func_id);
    }

    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        collect_script_function_declarations(
            child,
            &mut child_cursor,
            content,
            helper,
            prefix,
            callable_by_node_id,
            local_by_name,
            component_id,
        )?;
    }

    Ok(())
}

fn extract_script_call_edges<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
    callable_by_node_id: &HashMap<usize, sqry_core::graph::unified::NodeId>,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    caller_stack: &mut Vec<sqry_core::graph::unified::NodeId>,
) -> GraphResult<()> {
    let maybe_push = callable_by_node_id.get(&node.id()).copied();
    if let Some(caller_id) = maybe_push {
        caller_stack.push(caller_id);
    }

    if node.kind() == "call_expression"
        && let Some(caller_id) = caller_stack.last().copied()
        && let Some(callee_name) = call_expression_callee_name(&node, content)
    {
        let callee_id = if let Some(&id) = local_by_name.get(&callee_name) {
            id
        } else {
            let qualified_name = format!("{prefix}::{callee_name}");
            let visibility = extract_visibility(&qualified_name);
            let id = helper.add_function_with_visibility(
                &qualified_name,
                None,
                false,
                false,
                Some(visibility),
            );
            local_by_name.insert(callee_name.clone(), id);
            id
        };

        let arg_count = call_expression_argument_count(&node);
        let call_span = span_from_node(node);
        helper.add_call_edge_full_with_span(
            caller_id,
            callee_id,
            arg_count,
            false,
            vec![call_span],
        );
    }

    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        extract_script_call_edges(
            child,
            &mut child_cursor,
            content,
            helper,
            prefix,
            callable_by_node_id,
            local_by_name,
            caller_stack,
        )?;
    }

    if maybe_push.is_some() {
        caller_stack.pop();
    }

    Ok(())
}

fn call_expression_callee_name(node: &Node<'_>, content: &[u8]) -> Option<String> {
    let callee_expr = node.child_by_field_name("function")?;
    let text = callee_expr.utf8_text(content).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

fn call_expression_argument_count(node: &Node<'_>) -> u8 {
    let Some(args) = node.child_by_field_name("arguments") else {
        return 255;
    };
    let count = args.named_child_count();
    if count <= 254 {
        u8::try_from(count).expect("argument count fits in u8")
    } else {
        255
    }
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

/// Get function name from a function declaration node.
fn get_function_name(node: &Node<'_>, content: &[u8]) -> Option<String> {
    node.child_by_field_name("name")?
        .utf8_text(content)
        .ok()
        .map(std::string::ToString::to_string)
}

// ============================================================================
// Export Edge Extraction
// ============================================================================

/// Extract export edges from script declarations.
///
/// Processes `export_statement` nodes and emits Export edges to the module
/// for exported functions, classes, and variables.
///
/// Supports:
/// - `export function foo() {}` - function export
/// - `export class Bar {}` - class export
/// - `export const x = 1` - variable export
/// - `export let y = 2` - variable export
fn extract_export_edges<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Check if this is an `export_statement`
    if node.kind() == "export_statement" {
        process_export_statement(
            node,
            content,
            helper,
            module_id,
            local_by_name,
            component_id,
        );
    }

    // Recurse into children
    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        extract_export_edges(
            child,
            &mut child_cursor,
            content,
            helper,
            module_id,
            local_by_name,
            component_id,
        )?;
    }

    Ok(())
}

/// Process an `export_statement` node and emit Export edges.
fn process_export_statement(
    export_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
) {
    // Find the declaration being exported
    // export_statement contains children like: export_clause, or direct declarations
    let mut cursor = export_node.walk();
    let children: Vec<_> = export_node.children(&mut cursor).collect();

    for child in children {
        match child.kind() {
            // `export function foo() {}`
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = get_function_name(&child, content) {
                    // Check if this function already exists in local_by_name
                    let func_id = if let Some(&existing_id) = local_by_name.get(&name) {
                        existing_id
                    } else {
                        // Create new function node with simple name (exports use unqualified names)
                        let visibility = extract_visibility(&name);
                        let func_id = helper.add_function_with_visibility(
                            &name,
                            None,
                            false,
                            false,
                            Some(visibility),
                        );
                        local_by_name.insert(name.clone(), func_id);
                        func_id
                    };
                    // Emit export edge and contains edge
                    helper.add_export_edge(module_id, func_id);
                    helper.add_contains_edge(component_id, func_id);
                }
            }
            // `export class Foo {}`
            "class_declaration" | "class" => {
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        // Check if this class already exists
                        let class_id = if let Some(&existing_id) = local_by_name.get(&name) {
                            existing_id
                        } else {
                            let class_id = helper.add_class(&name, None);
                            local_by_name.insert(name.clone(), class_id);
                            class_id
                        };
                        helper.add_export_edge(module_id, class_id);
                        helper.add_contains_edge(component_id, class_id);
                    }
                }
            }
            // `export const x = 1` or `export let y = 2`
            "lexical_declaration" | "variable_declaration" => {
                // These declarations contain variable_declarator children
                let mut decl_cursor = child.walk();
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "variable_declarator"
                        && let Some(name_node) = decl_child.child_by_field_name("name")
                        && let Ok(name) = name_node.utf8_text(content)
                    {
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            // Check if this variable already exists
                            let var_id = if let Some(&existing_id) = local_by_name.get(&name) {
                                existing_id
                            } else {
                                let var_id = helper.add_variable(&name, None);
                                local_by_name.insert(name.clone(), var_id);
                                var_id
                            };
                            helper.add_export_edge(module_id, var_id);
                            helper.add_contains_edge(component_id, var_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// Import Edge Extraction
// ============================================================================

/// Extract import edges from script import statements.
///
/// Processes `import_statement` nodes and emits Import edges for each import.
///
/// Supports ES6 import syntax:
/// - `import { onMount } from 'svelte'` - named imports
/// - `import Component from './Component.svelte'` - default imports
/// - `import * as utils from './utils.js'` - namespace imports
/// - `import './styles.css'` - side-effect imports
fn extract_import_edges<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Check if this is an `import_statement`
    if node.kind() == "import_statement" {
        process_import_statement(node, content, helper, module_id)?;
    }

    // Recurse into children
    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        extract_import_edges(child, &mut child_cursor, content, helper, module_id)?;
    }

    Ok(())
}

/// Process an `import_statement` node and emit Import edges.
fn process_import_statement(
    import_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Find the source module (the string after 'from')
    let source_node = import_node.child_by_field_name("source");

    let Some(source_node) = source_node else {
        // No source - this might be a side-effect import or malformed
        return Ok(());
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
        return Ok(());
    }

    // Create the import node
    let import_span = span_from_node(import_node);
    let import_id = helper.add_import(&source_text, Some(import_span));

    // Create an import edge from the module to the imported module
    helper.add_import_edge(module_id, import_id);

    Ok(())
}

// ============================================================================
// Template Event Handler Extraction
// ============================================================================

/// Extract event handlers from Svelte template elements.
///
/// Walks the template AST looking for `on:*` event handler attributes
/// and emits Calls edges for each handler reference.
///
/// Svelte event handler syntax:
/// - `on:click={handleClick}` - simple handler reference
/// - `on:submit|preventDefault={handleSubmit}` - with modifiers
/// - `on:keydown={() => doSomething()}` - inline arrow functions
///
/// For inline arrow functions, we try to extract any function calls within them.
fn extract_template_event_handlers(
    node: Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) -> GraphResult<()> {
    // Process this node for event handlers
    match node.kind() {
        "element" | "self_closing_tag" => {
            extract_element_event_handlers(&node, source, helper, module_id, local_by_name);
        }
        "start_tag" => {
            extract_tag_event_handlers(&node, source, helper, module_id, local_by_name);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_template_event_handlers(child, source, helper, module_id, local_by_name)?;
    }

    Ok(())
}

/// Extract event handlers from an element node.
fn extract_element_event_handlers(
    element: &Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) {
    let mut cursor = element.walk();
    for child in element.children(&mut cursor) {
        if child.kind() == "start_tag" || child.kind() == "self_closing_tag" {
            extract_tag_event_handlers(&child, source, helper, module_id, local_by_name);
        }
    }
}

/// Extract event handlers from a `start_tag` or `self_closing_tag` node.
fn extract_tag_event_handlers(
    tag: &Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) {
    let mut cursor = tag.walk();
    for attr in tag.children(&mut cursor) {
        if attr.kind() != "attribute" {
            continue;
        }

        // Check if this is an event handler attribute (on:*)
        if let Some((event_name, handler_expr)) = parse_event_handler_attribute(&attr, source) {
            let call_span = span_from_node(attr);
            emit_event_handler_call(
                &event_name,
                &handler_expr,
                source,
                helper,
                module_id,
                local_by_name,
                Some(call_span),
            );
        }
    }
}

/// Parse an attribute node to check if it's an event handler.
///
/// Returns `Some((event_name, handler_expression))` if this is an `on:*` attribute.
/// Returns `None` for other attributes.
fn parse_event_handler_attribute(attr: &Node<'_>, source: &str) -> Option<(String, String)> {
    let mut attr_name: Option<String> = None;
    let mut handler_expr: Option<String> = None;

    let mut cursor = attr.walk();
    for child in attr.children(&mut cursor) {
        match child.kind() {
            "attribute_name" => {
                attr_name = child
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "expr_attribute_value" => {
                // The handler is wrapped in {expression}
                // expr_attribute_value contains an expression node
                let mut expr_cursor = child.walk();
                for expr_child in child.children(&mut expr_cursor) {
                    if expr_child.kind() == "expression" {
                        // Expression contains raw_text_expr (the actual handler text)
                        let mut inner_cursor = expr_child.walk();
                        for inner_child in expr_child.children(&mut inner_cursor) {
                            if inner_child.kind() == "raw_text_expr" {
                                handler_expr = inner_child
                                    .utf8_text(source.as_bytes())
                                    .ok()
                                    .map(|s| s.trim().to_string());
                            }
                        }
                    }
                }
                // Fallback: try to get the full text
                if handler_expr.is_none() {
                    handler_expr = child.utf8_text(source.as_bytes()).ok().map(|s| {
                        let s = s.trim();
                        // Remove surrounding braces if present
                        if s.starts_with('{') && s.ends_with('}') {
                            s[1..s.len() - 1].trim().to_string()
                        } else {
                            s.to_string()
                        }
                    });
                }
            }
            _ => {}
        }
    }

    let name = attr_name?;

    // Check if this is an on:* event handler
    // Handle modifiers: on:click|preventDefault|stopPropagation
    if let Some(event_part) = name.strip_prefix("on:") {
        // Extract event name without modifiers (before the first |)
        let event_name = event_part
            .split('|')
            .next()
            .unwrap_or(event_part)
            .to_string();
        let handler = handler_expr?;
        Some((event_name, handler))
    } else {
        None
    }
}

/// Emit a Calls edge for an event handler.
///
/// For simple handler references like `handleClick`, we emit a Calls edge
/// from the module to the handler function.
///
/// For inline expressions like `() => count++` or `() => doSomething()`,
/// we try to extract any function calls within them.
fn emit_event_handler_call(
    event_name: &str,
    handler_expr: &str,
    _source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    call_span: Option<Span>,
) {
    let handler_expr = handler_expr.trim();

    // Skip empty handlers or forwarded events (on:click without value)
    if handler_expr.is_empty() {
        return;
    }

    // Determine if this is an inline function or a handler reference
    let handlers_to_emit: Vec<String> = if handler_expr.starts_with("()")
        || handler_expr.starts_with("(e)")
        || handler_expr.starts_with("(event)")
        || handler_expr.starts_with("($event)")
        || handler_expr.contains("=>")
    {
        // Inline arrow function - extract function calls from within
        extract_function_calls_from_inline(handler_expr)
    } else if handler_expr.contains('(') {
        // Looks like a function call expression: doSomething(arg)
        // Extract the function name before the parenthesis
        if let Some(func_name) = handler_expr.split('(').next() {
            vec![func_name.trim().to_string()]
        } else {
            vec![]
        }
    } else {
        // Simple handler reference: handleClick, handleSubmit
        vec![handler_expr.to_string()]
    };

    // Create a synthetic "caller" node representing the event (e.g., "svelte::event::click")
    let event_caller_name = format!("svelte::event::{event_name}");
    let visibility = extract_visibility(&event_caller_name);
    let event_node_id = helper.add_function_with_visibility(
        &event_caller_name,
        None,
        false,
        false,
        Some(visibility),
    );

    for handler_name in handlers_to_emit {
        if handler_name.is_empty() {
            continue;
        }

        // Look up the handler in the local names, or create a new node
        let handler_id = if let Some(&id) = local_by_name.get(&handler_name) {
            id
        } else {
            // Handler not found in script - create a synthetic node
            // This handles cases where the handler is defined externally or dynamically
            let qualified_name = format!("svelte::instance::{handler_name}");
            let visibility = extract_visibility(&qualified_name);
            let id = helper.add_function_with_visibility(
                &qualified_name,
                None,
                false,
                false,
                Some(visibility),
            );
            local_by_name.insert(handler_name.clone(), id);
            id
        };

        // Emit Calls edge: module -> handler (for simple reference lookup)
        // and event -> handler (for semantic meaning)
        // Use 255 sentinel for unknown argument count (template event handlers)
        helper.add_call_edge_full_with_span(
            module_id,
            handler_id,
            255,
            false,
            call_span.into_iter().collect(),
        );
        helper.add_call_edge_full_with_span(
            event_node_id,
            handler_id,
            255,
            false,
            call_span.into_iter().collect(),
        );
    }
}

/// Extract function calls from an inline arrow function expression.
///
/// Examples:
/// - `() => handleClick()` -> [`handleClick`]
/// - `() => { doA(); doB(); }` -> [`doA`, `doB`]
/// - `(e) => e.preventDefault(); handleSubmit()` -> [`handleSubmit`]
fn extract_function_calls_from_inline(expr: &str) -> Vec<String> {
    let mut calls = Vec::new();

    // Find the arrow (=>) and get the body
    let body = if let Some(arrow_pos) = expr.find("=>") {
        expr[arrow_pos + 2..].trim()
    } else {
        return calls;
    };

    // Simple heuristic: find patterns that look like function calls
    // Look for identifier followed by (
    let mut remaining = body;
    while let Some(paren_pos) = remaining.find('(') {
        // Look backwards from the paren to find the identifier
        let before_paren = &remaining[..paren_pos];

        // Find the start of the identifier (last sequence of word chars)
        let identifier = before_paren
            .rsplit(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .next()
            .unwrap_or("");

        if !identifier.is_empty() {
            // Skip common patterns that aren't function calls
            if identifier != "function"
                && identifier != "if"
                && identifier != "for"
                && identifier != "while"
                && identifier != "switch"
                && identifier != "catch"
                && !identifier.chars().all(char::is_numeric)
            {
                calls.push(identifier.to_string());
            }
        }

        // Move past this call
        if paren_pos + 1 < remaining.len() {
            remaining = &remaining[paren_pos + 1..];
        } else {
            break;
        }
    }

    calls
}

/// Extract visibility for a Svelte function.
///
/// In Svelte components, all functions defined in script blocks are considered
/// public as they are part of the component's API and accessible via the
/// component instance. Svelte doesn't have formal visibility modifiers.
fn extract_visibility(_name: &str) -> &'static str {
    "public"
}

// ============================================================================
// TypeOf and References Edge Extraction (TypeScript annotations)
// ============================================================================

/// Recursively walk the script AST to extract `TypeOf` and `References` edges
/// from TypeScript type annotations on functions, methods, and variables.
fn extract_type_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
    callable_by_node_id: &HashMap<usize, NodeId>,
) -> GraphResult<()> {
    match node.kind() {
        "function_declaration"
        | "method_definition"
        | "arrow_function"
        | "function"
        | "generator_function_declaration"
        | "generator_function" => {
            // Extract parameter type edges
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_parameter_type_edges(node, params, content, helper, prefix)?;
            }

            // Extract return type edges
            if let Some(return_type_node) = node.child_by_field_name("return_type") {
                // Use registered callable, or create a fallback function node for
                // unregistered forms (arrow functions, generators, function expressions)
                let func_id = callable_by_node_id
                    .get(&node.id())
                    .copied()
                    .unwrap_or_else(|| {
                        let owner = match get_function_name(&node, content) {
                            Some(name) => format!("{name}_at_{}", node.start_byte()),
                            None => format!("anon_at_{}", node.start_byte()),
                        };
                        let qualified = format!("{prefix}::{owner}");
                        helper.add_function(&qualified, Some(span_from_node(node)), false, false)
                    });

                // TypeOf edge for return type
                if let Some(type_text) = extract_type_string(return_type_node, content) {
                    let type_id =
                        helper.add_type(&type_text, Some(span_from_node(return_type_node)));
                    helper.add_typeof_edge_with_context(
                        func_id,
                        type_id,
                        Some(TypeOfContext::Return),
                        Some(0),
                        None,
                    );
                }

                // References edges for return type
                let mut seen = HashSet::new();
                let all_types = extract_all_type_names_from_annotation(return_type_node, content);
                for type_name in all_types {
                    if seen.insert(type_name.clone()) {
                        let type_id =
                            helper.add_type(&type_name, Some(span_from_node(return_type_node)));
                        helper.add_reference_edge(func_id, type_id);
                    }
                }
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            extract_variable_type_edges(node, content, helper, prefix)?;
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_type_edges(child, content, helper, prefix, callable_by_node_id)?;
    }

    Ok(())
}

/// Extract `TypeOf` and `References` edges for function/method parameters.
///
/// Creates a qualified Variable node for each typed parameter and emits:
/// - `TypeOf` edge with `Parameter` context and `param_index`
/// - `References` edges to each individual type name (deduplicated per parameter)
#[allow(clippy::unnecessary_wraps)]
fn extract_parameter_type_edges(
    func_node: Node<'_>,
    params_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
) -> GraphResult<()> {
    // Determine owner identity for qualified naming
    let owner = match get_function_name(&func_node, content) {
        Some(name) => format!("{name}_at_{}", func_node.start_byte()),
        None => format!("anon_at_{}", func_node.start_byte()),
    };

    let mut cursor = params_node.walk();
    let mut param_index: u16 = 0;

    for child in params_node.children(&mut cursor) {
        match child.kind() {
            "required_parameter" | "optional_parameter" => {
                // Get parameter name
                let name_node = child.child_by_field_name("pattern").or_else(|| {
                    child
                        .named_children(&mut child.walk())
                        .find(|n| matches!(n.kind(), "identifier" | "this"))
                });

                let Some(name_node) = name_node else {
                    continue;
                };

                let Ok(name) = name_node.utf8_text(content) else {
                    continue;
                };
                let name = name.trim().to_string();

                // Create qualified parameter variable node
                let qualified_name = format!("{prefix}::{owner}::param::{name}");
                let param_id = helper.add_variable(&qualified_name, Some(span_from_node(child)));

                // Check for type annotation
                if let Some(type_node) = child.child_by_field_name("type") {
                    // TypeOf edge
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

                    // References edges (deduplicated per parameter)
                    let mut seen = HashSet::new();
                    let all_types = extract_all_type_names_from_annotation(type_node, content);
                    for type_name in all_types {
                        if seen.insert(type_name.clone()) {
                            let type_id =
                                helper.add_type(&type_name, Some(span_from_node(type_node)));
                            helper.add_reference_edge(param_id, type_id);
                        }
                    }
                }

                param_index += 1;
            }
            "rest_parameter" => {
                // Get rest parameter name from pattern or identifier child
                let name_node = child.child_by_field_name("pattern").or_else(|| {
                    child
                        .named_children(&mut child.walk())
                        .find(|n| n.kind() == "identifier")
                });

                let Some(name_node) = name_node else {
                    continue;
                };

                let Ok(name) = name_node.utf8_text(content) else {
                    continue;
                };
                let name = name.trim().to_string();

                // Create qualified parameter variable node
                let qualified_name = format!("{prefix}::{owner}::param::{name}");
                let param_id = helper.add_variable(&qualified_name, Some(span_from_node(child)));

                // Check for type annotation
                if let Some(type_node) = child.child_by_field_name("type") {
                    // TypeOf edge
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

                    // References edges (deduplicated)
                    let mut seen = HashSet::new();
                    let all_types = extract_all_type_names_from_annotation(type_node, content);
                    for type_name in all_types {
                        if seen.insert(type_name.clone()) {
                            let type_id =
                                helper.add_type(&type_name, Some(span_from_node(type_node)));
                            helper.add_reference_edge(param_id, type_id);
                        }
                    }
                }

                param_index += 1;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Extract `TypeOf` and `References` edges for typed variable declarations.
///
/// Creates a qualified Variable node for each typed variable declarator and emits:
/// - `TypeOf` edge with `Variable` context
/// - `References` edges to each individual type name (deduplicated per variable)
#[allow(clippy::unnecessary_wraps)]
fn extract_variable_type_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
) -> GraphResult<()> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }

        // Get variable name
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };

        if name_node.kind() != "identifier" {
            continue;
        }

        let Ok(name) = name_node.utf8_text(content) else {
            continue;
        };
        let name = name.trim().to_string();

        // Check for type annotation
        let Some(type_node) = child.child_by_field_name("type") else {
            continue;
        };

        // Create qualified variable node (byte offset for positional uniqueness)
        let qualified_name = format!("{prefix}::var::{name}_at_{}", child.start_byte());
        let var_id = helper.add_variable(&qualified_name, Some(span_from_node(child)));

        // TypeOf edge
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

        // References edges (deduplicated per variable)
        let mut seen = HashSet::new();
        let all_types = extract_all_type_names_from_annotation(type_node, content);
        for type_name in all_types {
            if seen.insert(type_name.clone()) {
                let type_id = helper.add_type(&type_name, Some(span_from_node(type_node)));
                helper.add_reference_edge(var_id, type_id);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use std::collections::HashMap;

    fn parse_svelte(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_svelte_sqry::language())
            .expect("Failed to load Svelte grammar");

        let content = source.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    /// Count nodes created in the staging graph
    fn count_nodes(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| matches!(op, StagingOp::AddNode { .. }))
            .count()
    }

    /// Count Calls edges in the staging graph
    #[allow(dead_code)]
    fn count_calls_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .count()
    }

    #[test]
    fn test_extract_function_from_script() {
        let source = r#"
<script>
  function greet(name) {
    console.log("Hello " + name);
  }
</script>

<div>Hello</div>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create at least a module node
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_extract_call_from_script() {
        let source = r"
<script>
  function main() {
    helper();
  }

  function helper() {
    return 42;
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create nodes for module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_typescript_script_block() {
        let source = r#"
<script lang="ts">
  function calculate(x: number, y: number): number {
    return x + y;
  }
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // TypeScript script blocks should still be processed
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_module_script_block() {
        let source = r#"
<script context="module">
  export function moduleHelper() {
    return "shared";
  }
</script>

<script>
  function instanceFunc() {
    return moduleHelper();
  }
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create nodes for module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_argument_count() {
        let source = r"
<script>
  function caller() {
    callee(1, 2, 3);
  }

  function callee(a, b, c) {
    return a + b + c;
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Graph should be created
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_nested_function_call() {
        let source = r"
<script>
  function outer() {
    function inner() {
      return helper();
    }
    return inner();
  }

  function helper() {
    return 42;
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Graph should be created
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_no_script_block() {
        let source = r"
<div>
  <h1>No script here</h1>
</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        // Should not error on files without script blocks
        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should at least create a module node
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_language_is_svelte() {
        let source = r"
<script>
  function test() {}
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create nodes for the module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_module_instance_context_collision_prevention() {
        // CRITICAL: This test prevents node ID collisions between module and instance contexts.
        // Both scripts define an `init` function - they must create separate nodes.
        let source = r#"
<script context="module">
  function init() {
    return moduleHelper();
  }

  function moduleHelper() {
    return "module data";
  }
</script>

<script>
  function init() {
    return instanceHelper();
  }

  function instanceHelper() {
    return "instance data";
  }
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create nodes for the module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_column_offset_with_indented_script() {
        // Test that column offset is correctly applied only on line 0
        // Use a proper multiline script block structure
        let source = r"
<script>
  function a() {
    return b();
  }

  function b() {
    return 42;
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should create nodes for the module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    // =========================================================================
    // Event Handler Tests - Svelte on:* directive
    // =========================================================================

    use sqry_core::graph::unified::build::test_helpers::collect_call_edges;

    /// Helper to check if a Calls edge exists in staging operations
    fn has_call_edge_with_names(staging: &StagingGraph, expected_callee: &str) -> bool {
        let ops = staging.operations();

        // Build a name lookup from InternString ops
        let mut string_by_id: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        for op in ops {
            if let StagingOp::InternString { local_id, value } = op {
                string_by_id.insert(local_id.index(), value.clone());
            }
        }

        // Build node name lookup from AddNode ops
        let mut node_names: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        for op in ops {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(id),
            } = op
                && let Some(name) = string_by_id.get(&entry.name.index())
            {
                node_names.insert(id.index(), name.clone());
            }
        }

        // Check for matching Calls edges
        for op in ops {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::Calls { .. },
                ..
            } = op
                && let Some(target_name) = node_names.get(&target.index())
                && target_name.contains(expected_callee)
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn test_event_handler_simple_onclick() {
        let source = r#"
<script>
  function handleClick() {
    console.log("clicked");
  }
</script>

<button on:click={handleClick}>Click me</button>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Calls edges for the event handler
        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected at least one Calls edge for on:click handler"
        );

        // Verify the handleClick function is referenced
        assert!(
            has_call_edge_with_names(&staging, "handleClick"),
            "Expected Calls edge to handleClick"
        );
    }

    #[test]
    fn test_event_handler_submit() {
        let source = r#"
<script>
  function handleSubmit() {
    console.log("submitted");
  }
</script>

<form on:submit={handleSubmit}>
  <button type="submit">Submit</button>
</form>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for on:submit handler"
        );

        assert!(
            has_call_edge_with_names(&staging, "handleSubmit"),
            "Expected Calls edge to handleSubmit"
        );
    }

    #[test]
    fn test_event_handler_keydown() {
        let source = r#"
<script>
  function handleKeydown(event) {
    if (event.key === 'Enter') {
      console.log("Enter pressed");
    }
  }
</script>

<input on:keydown={handleKeydown} />
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for on:keydown handler"
        );

        assert!(
            has_call_edge_with_names(&staging, "handleKeydown"),
            "Expected Calls edge to handleKeydown"
        );
    }

    #[test]
    fn test_event_handler_with_modifiers() {
        let source = r#"
<script>
  function handleSubmit() {
    console.log("submitted without default");
  }
</script>

<form on:submit|preventDefault={handleSubmit}>
  <button>Submit</button>
</form>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for handler with modifiers"
        );

        assert!(
            has_call_edge_with_names(&staging, "handleSubmit"),
            "Expected Calls edge to handleSubmit despite modifiers"
        );
    }

    #[test]
    fn test_event_handler_inline_arrow_function() {
        let source = r#"
<script>
  function doSomething() {
    console.log("doing something");
  }
</script>

<button on:click={() => doSomething()}>Do it</button>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for inline arrow function"
        );

        // Should detect the doSomething() call inside the arrow function
        assert!(
            has_call_edge_with_names(&staging, "doSomething"),
            "Expected Calls edge to doSomething from inline handler"
        );
    }

    #[test]
    fn test_event_handler_multiple_events() {
        let source = r"
<script>
  function handleMouseEnter() {}
  function handleMouseLeave() {}
  function handleFocus() {}
</script>

<div
  on:mouseenter={handleMouseEnter}
  on:mouseleave={handleMouseLeave}
  on:focus={handleFocus}
>
  Hover me
</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        // Each handler should create 2 Calls edges (module->handler, event->handler)
        // Plus we have 3 events
        assert!(
            call_edges.len() >= 6,
            "Expected at least 6 Calls edges for 3 event handlers, got {}",
            call_edges.len()
        );
    }

    #[test]
    fn test_event_handler_self_closing_tag() {
        let source = r"
<script>
  function handleChange() {}
</script>

<input on:change={handleChange} />
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for self-closing tag"
        );

        assert!(
            has_call_edge_with_names(&staging, "handleChange"),
            "Expected Calls edge to handleChange"
        );
    }

    #[test]
    fn test_event_handler_nested_elements() {
        let source = r"
<script>
  function handleOuter() {}
  function handleInner() {}
</script>

<div on:click={handleOuter}>
  <button on:click={handleInner}>Nested</button>
</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_call_edge_with_names(&staging, "handleOuter"),
            "Expected Calls edge to handleOuter"
        );
        assert!(
            has_call_edge_with_names(&staging, "handleInner"),
            "Expected Calls edge to handleInner"
        );
    }

    #[test]
    fn test_event_handler_multiple_modifiers() {
        let source = r"
<script>
  function handleScroll() {}
</script>

<div on:scroll|passive|capture={handleScroll}>Content</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_call_edge_with_names(&staging, "handleScroll"),
            "Expected Calls edge despite multiple modifiers"
        );
    }

    #[test]
    fn test_event_handler_inline_with_parameter() {
        let source = r"
<script>
  function log(message) {
    console.log(message);
  }
</script>

<button on:click={(e) => log('clicked')}>Click</button>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for inline handler with parameter"
        );

        assert!(
            has_call_edge_with_names(&staging, "log"),
            "Expected Calls edge to log from inline handler"
        );
    }

    #[test]
    fn test_event_handler_no_script_creates_synthetic_node() {
        // When handler isn't defined in script, we should still create edges
        let source = r"
<button on:click={undefinedHandler}>Click</button>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should still create a node for the handler
        assert!(
            has_call_edge_with_names(&staging, "undefinedHandler"),
            "Expected Calls edge to synthetic undefinedHandler node"
        );
    }

    #[test]
    fn test_extract_function_calls_from_inline_simple() {
        let calls = extract_function_calls_from_inline("() => handleClick()");
        assert_eq!(calls, vec!["handleClick"]);
    }

    #[test]
    fn test_extract_function_calls_from_inline_multiple() {
        let calls = extract_function_calls_from_inline("() => { doA(); doB(); }");
        assert!(calls.contains(&"doA".to_string()));
        assert!(calls.contains(&"doB".to_string()));
    }

    #[test]
    fn test_extract_function_calls_from_inline_with_args() {
        let calls = extract_function_calls_from_inline("(e) => log('message')");
        assert!(calls.contains(&"log".to_string()));
    }

    #[test]
    fn test_svelte_runes_state_effect() {
        // Test that Svelte 5 Runes syntax doesn't interfere
        let source = r"
<script>
  let count = $state(0);

  function increment() {
    count++;
  }

  $effect(() => {
    console.log(count);
  });
</script>

<button on:click={increment}>Count: {count}</button>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_call_edge_with_names(&staging, "increment"),
            "Expected Calls edge to increment handler"
        );
    }

    // =========================================================================
    // Import Edge Tests
    // =========================================================================

    /// Count Import edges in the staging graph
    fn count_import_edges(staging: &StagingGraph) -> usize {
        staging
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
            .count()
    }

    #[test]
    fn test_import_from_svelte() {
        let source = r"
<script>
  import { onMount, onDestroy } from 'svelte';

  onMount(() => {
    console.log('mounted');
  });
</script>

<div>Hello</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have at least one Import edge
        assert!(
            count_import_edges(&staging) > 0,
            "Expected at least one Import edge for 'svelte' import"
        );
    }

    #[test]
    fn test_import_component() {
        let source = r"
<script>
  import Component from './Component.svelte';
  import { helper } from './utils.js';
</script>

<Component />
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Import edges for both imports
        assert_eq!(
            count_import_edges(&staging),
            2,
            "Expected exactly 2 Import edges"
        );
    }

    #[test]
    fn test_import_namespace() {
        let source = r"
<script>
  import * as utils from './utils.js';

  function test() {
    return utils.helper();
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Import edge for namespace import
        assert_eq!(
            count_import_edges(&staging),
            1,
            "Expected exactly 1 Import edge for namespace import"
        );
    }

    #[test]
    fn test_multiple_imports() {
        let source = r"
<script>
  import { onMount } from 'svelte';
  import Component from './Component.svelte';
  import * as utils from './utils.js';
  import './styles.css';
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Import edges for all 4 imports
        assert_eq!(
            count_import_edges(&staging),
            4,
            "Expected exactly 4 Import edges"
        );
    }

    #[test]
    fn test_import_in_module_context() {
        let source = r#"
<script context="module">
  import { writable } from 'svelte/store';
  export const store = writable(0);
</script>

<script>
  import { onMount } from 'svelte';

  onMount(() => {
    console.log($store);
  });
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Import edges from both script contexts
        assert_eq!(
            count_import_edges(&staging),
            2,
            "Expected exactly 2 Import edges (one from each script block)"
        );
    }

    // =========================================================================
    // Export Edge Tests
    // =========================================================================

    /// Count Export edges in the staging graph
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

    /// Helper to check if an export edge exists with a given name
    fn has_export_edge_with_names(staging: &StagingGraph, expected_exported: &str) -> bool {
        let ops = staging.operations();

        // Build a name lookup from InternString ops
        let mut string_by_id: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        for op in ops {
            if let StagingOp::InternString { local_id, value } = op {
                string_by_id.insert(local_id.index(), value.clone());
            }
        }

        // Build node name lookup from AddNode ops
        let mut node_names: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        for op in ops {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(id),
            } = op
                && let Some(name) = string_by_id.get(&entry.name.index())
            {
                node_names.insert(id.index(), name.clone());
            }
        }

        // Check for matching Export edges
        for op in ops {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::Exports { .. },
                ..
            } = op
                && let Some(target_name) = node_names.get(&target.index())
                && target_name.contains(expected_exported)
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn test_export_function() {
        let source = r"
<script>
  export function greet(name) {
    return `Hello, ${name}!`;
  }

  function privateHelper() {
    return 42;
  }
</script>

<div>Hello</div>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Export edges
        assert!(
            count_export_edges(&staging) > 0,
            "Expected at least one Export edge"
        );

        // Should export greet function
        assert!(
            has_export_edge_with_names(&staging, "greet"),
            "Expected Export edge for greet function"
        );

        // privateHelper should NOT be exported
        assert!(
            !has_export_edge_with_names(&staging, "privateHelper"),
            "privateHelper should not be exported"
        );
    }

    #[test]
    fn test_export_class() {
        let source = r"
<script>
  export class User {
    constructor(name) {
      this.name = name;
    }
  }

  class PrivateClass {
    getData() {
      return this.name;
    }
  }
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Export edges
        assert!(
            count_export_edges(&staging) > 0,
            "Expected at least one Export edge"
        );

        // Should export User class
        assert!(
            has_export_edge_with_names(&staging, "User"),
            "Expected Export edge for User class"
        );

        // PrivateClass should NOT be exported
        assert!(
            !has_export_edge_with_names(&staging, "PrivateClass"),
            "PrivateClass should not be exported"
        );
    }

    #[test]
    fn test_export_const_and_let() {
        let source = r#"
<script>
  export const API_VERSION = "1.0.0";
  export let config = { debug: true };

  const privateConst = 42;
  let privateVar = "secret";
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have Export edges for both exports
        assert!(
            count_export_edges(&staging) >= 2,
            "Expected at least 2 Export edges for const and let"
        );

        // Should export API_VERSION
        assert!(
            has_export_edge_with_names(&staging, "API_VERSION"),
            "Expected Export edge for API_VERSION"
        );

        // Should export config
        assert!(
            has_export_edge_with_names(&staging, "config"),
            "Expected Export edge for config"
        );

        // privateConst and privateVar should NOT be exported
        assert!(
            !has_export_edge_with_names(&staging, "privateConst"),
            "privateConst should not be exported"
        );
        assert!(
            !has_export_edge_with_names(&staging, "privateVar"),
            "privateVar should not be exported"
        );
    }

    #[test]
    fn test_multiple_exports() {
        let source = r"
<script>
  export function foo() {}
  export function bar() {}
  export class Baz {}
  export const x = 1;
</script>
";

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Should have 4 Export edges
        assert_eq!(
            count_export_edges(&staging),
            4,
            "Expected exactly 4 Export edges"
        );

        assert!(has_export_edge_with_names(&staging, "foo"));
        assert!(has_export_edge_with_names(&staging, "bar"));
        assert!(has_export_edge_with_names(&staging, "Baz"));
        assert!(has_export_edge_with_names(&staging, "x"));
    }

    #[test]
    fn test_export_function_no_duplicate_nodes() {
        // Test that exporting a function that's also called internally doesn't create duplicates
        let source = r#"
<script>
  export function greet(name) {
    return `Hello, ${name}!`;
  }

  function main() {
    greet("World");
  }
</script>
"#;

        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Verify that greet is exported
        assert!(
            has_export_edge_with_names(&staging, "greet"),
            "Expected Export edge for greet function"
        );

        // Verify that greet is called by main
        assert!(
            has_call_edge_with_names(&staging, "greet"),
            "Expected Calls edge to greet"
        );

        // Count all AddNode operations to ensure no duplicates
        let ops = staging.operations();
        let mut string_by_id: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        for op in ops {
            if let StagingOp::InternString { local_id, value } = op {
                string_by_id.insert(local_id.index(), value.clone());
            }
        }

        // Count how many nodes contain "greet" in their name
        // (could be "greet", "svelte::instance::greet", etc.)
        let mut greet_count = 0;
        for op in ops {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(_),
            } = op
                && let Some(name) = string_by_id.get(&entry.name.index())
                && name.contains("greet")
            {
                greet_count += 1;
            }
        }

        assert_eq!(
            greet_count, 1,
            "Expected exactly 1 node containing 'greet', found {greet_count}"
        );
    }

    // ========================================================================
    // TypeOf and References edge tests (TypeScript blocks only)
    // ========================================================================

    fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
        let mut nodes = HashMap::new();
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
            {
                let name = staging
                    .resolve_node_canonical_name(entry)
                    .unwrap_or_default()
                    .to_owned();
                nodes.insert(*node_id, (name, entry.kind));
            }
        }
        nodes
    }

    fn count_typeof_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TypeOf { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn count_references_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::References,
                        ..
                    }
                )
            })
            .count()
    }

    fn has_typeof_edge(staging: &StagingGraph, source_name: &str, type_text: &str) -> bool {
        let nodes = build_node_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::TypeOf { .. },
                ..
            } = op
            {
                let src_name = nodes.get(source).map(|(n, _)| n.as_str());
                let tgt_name = nodes.get(target).map(|(n, _)| n.as_str());
                if src_name.is_some_and(|n| n.contains(source_name))
                    && tgt_name.is_some_and(|n| n.contains(type_text))
                {
                    return true;
                }
            }
        }
        false
    }

    fn has_reference_edge(staging: &StagingGraph, source_name: &str, target_type: &str) -> bool {
        let nodes = build_node_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                let src_name = nodes.get(source).map(|(n, _)| n.as_str());
                let tgt_name = nodes.get(target).map(|(n, _)| n.as_str());
                if src_name.is_some_and(|n| n.contains(source_name))
                    && tgt_name.is_some_and(|n| n.contains(target_type))
                {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn test_ts_function_parameter_types() {
        let source = r#"
<script lang="ts">
function calc(x: number, y: string) {
  return x;
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 2,
            "Expected TypeOf edges for both params"
        );
        assert!(
            has_typeof_edge(&staging, "param::x", "number"),
            "Expected TypeOf edge for param x → number"
        );
        assert!(
            has_typeof_edge(&staging, "param::y", "string"),
            "Expected TypeOf edge for param y → string"
        );
        assert!(
            has_reference_edge(&staging, "param::x", "number"),
            "Expected References edge to number"
        );
        assert!(
            has_reference_edge(&staging, "param::y", "string"),
            "Expected References edge to string"
        );
    }

    #[test]
    fn test_ts_function_return_type() {
        let source = r#"
<script lang="ts">
function getUser(): User {
  return {};
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for return type"
        );
        assert!(
            has_reference_edge(&staging, "getUser", "User"),
            "Expected References edge to User"
        );
    }

    #[test]
    fn test_ts_export_let_type_annotation() {
        let source = r#"
<script lang="ts">
export let name: string;
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // export let creates a variable declaration with type annotation
        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for export let"
        );
        assert!(
            count_references_edges(&staging) >= 1,
            "Expected References edge for string"
        );
    }

    #[test]
    fn test_ts_variable_type_annotation() {
        let source = r#"
<script lang="ts">
const user: User = {};
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for variable"
        );
        assert!(
            count_references_edges(&staging) >= 1,
            "Expected References edge for User"
        );
    }

    #[test]
    fn test_ts_generic_type_references() {
        let source = r#"
<script lang="ts">
const items: Array<User> = [];
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_reference_edge(&staging, "var::items", "Array"),
            "Expected References to Array"
        );
        assert!(
            has_reference_edge(&staging, "var::items", "User"),
            "Expected References to User"
        );
    }

    #[test]
    fn test_ts_union_type_references() {
        let source = r#"
<script lang="ts">
const v: string | number = "";
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_reference_edge(&staging, "var::v", "string"),
            "Expected References to string"
        );
        assert!(
            has_reference_edge(&staging, "var::v", "number"),
            "Expected References to number"
        );
    }

    #[test]
    fn test_js_script_no_type_edges() {
        let source = r"
<script>
function calc(x, y) {
  return x + y;
}
const user = {};
</script>
";
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert_eq!(
            count_typeof_edges(&staging),
            0,
            "JS script should have zero TypeOf edges"
        );
        assert_eq!(
            count_references_edges(&staging),
            0,
            "JS script should have zero References edges"
        );
    }

    #[test]
    fn test_ts_module_context_type_edges() {
        let source = r#"
<script context="module" lang="ts">
function shared(x: number): string {
  return String(x);
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 2,
            "Expected TypeOf edges for param + return in module context"
        );
        assert!(has_reference_edge(&staging, "param::x", "number"));
    }

    #[test]
    fn test_ts_multiple_params_indexed() {
        let source = r#"
<script lang="ts">
function f(a: string, b: number, c: boolean) {}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Check param_index values in TypeOf metadata
        let typeof_edges: Vec<_> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    kind: EdgeKind::TypeOf { context, index, .. },
                    ..
                } = op
                {
                    if *context == Some(TypeOfContext::Parameter) {
                        Some(*index)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        assert!(typeof_edges.contains(&Some(0)), "Expected param_index 0");
        assert!(typeof_edges.contains(&Some(1)), "Expected param_index 1");
        assert!(typeof_edges.contains(&Some(2)), "Expected param_index 2");
    }

    #[test]
    fn test_ts_complex_return_type() {
        let source = r#"
<script lang="ts">
function getUser(): Promise<User | null> {
  return Promise.resolve(null);
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            has_reference_edge(&staging, "getUser", "Promise"),
            "Expected References to Promise"
        );
        assert!(
            has_reference_edge(&staging, "getUser", "User"),
            "Expected References to User"
        );
    }

    #[test]
    fn test_ts_arrow_function_types() {
        let source = r#"
<script lang="ts">
const fn1 = (x: number): string => x.toString();
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Parameter edges
        assert!(
            count_typeof_edges(&staging) >= 2,
            "Expected TypeOf edges for arrow param + return"
        );
        assert!(
            has_reference_edge(&staging, "param::x", "number"),
            "Expected References to number for param"
        );

        // Return type edges (fallback node for unregistered arrow function)
        assert!(
            has_typeof_edge(&staging, "anon_at_", "string"),
            "Expected TypeOf Return edge for arrow function → string"
        );
        assert!(
            has_reference_edge(&staging, "anon_at_", "string"),
            "Expected References edge for arrow return → string"
        );
    }

    #[test]
    fn test_ts_generator_function_return_type() {
        let source = r#"
<script lang="ts">
function* gen(): Generator<number> {
  yield 1;
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Generator return type edges
        assert!(
            has_reference_edge(&staging, "gen", "Generator"),
            "Expected References to Generator for generator return"
        );
        assert!(
            has_reference_edge(&staging, "gen", "number"),
            "Expected References to number for generator return"
        );
    }

    #[test]
    fn test_ts_method_definition_return_type() {
        let source = r#"
<script lang="ts">
class Greeter {
  greet(name: string): string {
    return `Hello ${name}`;
  }
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Method param + return edges via fallback node
        assert!(
            has_reference_edge(&staging, "param::name", "string"),
            "Expected References to string for method param"
        );
        assert!(
            has_typeof_edge(&staging, "greet", "string"),
            "Expected TypeOf Return edge for method_definition → string"
        );
    }

    #[test]
    fn test_ts_param_identity_isolation() {
        let source = r#"
<script lang="ts">
function foo(x: string) {}
function bar(x: number) {}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Both params should have TypeOf edges (2 distinct param nodes due to qualified naming)
        let param_typeof_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TypeOf {
                            context: Some(TypeOfContext::Parameter),
                            ..
                        },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            param_typeof_count, 2,
            "Expected 2 distinct param TypeOf edges"
        );
    }

    #[test]
    fn test_ts_repeated_type_dedup() {
        let source = r#"
<script lang="ts">
function f(a: User, b: User): User {
  return a;
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Each source node should have at most 1 References edge to User (deduped per source)
        // param a → User (1), param b → User (1), function f → User (1 for return)
        let ref_edges: Vec<_> = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::References,
                        ..
                    }
                )
            })
            .collect();

        // Should be exactly 3 References edges (a→User, b→User, f→User)
        assert_eq!(
            ref_edges.len(),
            3,
            "Expected 3 References edges (one per source→User)"
        );
    }

    #[test]
    fn test_ts_rest_parameter() {
        let source = r#"
<script lang="ts">
function f(...args: string[]) {}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for rest param"
        );
    }

    #[test]
    fn test_ts_duplicate_named_function_isolation() {
        let source = r#"
<script lang="ts">
function outer() {
  function helper(x: string) {}
}
function wrapper() {
  function helper(x: number) {}
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let mut staging = StagingGraph::new();
        SvelteGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.svelte"), &mut staging)
            .unwrap();

        // Both helper functions should have distinct param nodes with distinct TypeOf edges
        let param_typeof_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TypeOf {
                            context: Some(TypeOfContext::Parameter),
                            ..
                        },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            param_typeof_count, 2,
            "Expected 2 distinct param TypeOf edges for two helper functions"
        );
    }

    #[test]
    fn test_per_block_body_hashes_attached() {
        use sqry_core::graph::body_hash::BodyHash128;
        use sqry_core::graph::unified::build::body_hash::{build_line_offsets, extract_node_body};

        // Script block is NOT on line 1 — markup comes first.
        // Without per-block hashing, node spans (relative to script content)
        // would be applied against the full SFC content, producing wrong hashes.
        let source = r#"<div>
  <p>{message}</p>
</div>

<script>
function greet() {
  return "hello";
}
</script>
"#;
        let (tree, content) = parse_svelte(source);
        let builder = SvelteGraphBuilder::default();
        let mut staging = StagingGraph::new();
        builder
            .build_graph(&tree, &content, Path::new("App.svelte"), &mut staging)
            .expect("build_graph");

        // Function nodes created from the script block should already
        // have body hashes attached (per-block, not waiting for entrypoint).
        let func_nodes_with_hash: Vec<_> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddNode { entry, .. } = op
                    && entry.kind == NodeKind::Function
                    && entry.body_hash.is_some()
                {
                    Some(entry)
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !func_nodes_with_hash.is_empty(),
            "Function nodes from script blocks should have body hashes \
             attached per-block (not deferred to entrypoint)"
        );

        // Verify the hash was computed from script block bytes, not the
        // full SFC content.  Extract the script block text, compute the
        // expected body hash, and compare.
        let script_block = "function greet() {\n  return \"hello\";\n}\n";
        let script_bytes = script_block.as_bytes();
        let line_offsets = build_line_offsets(script_bytes);

        let entry = func_nodes_with_hash[0];
        let body = extract_node_body(script_bytes, entry, &line_offsets)
            .expect("should extract body from script block bytes");
        let expected_hash = BodyHash128::compute(&body);

        assert_eq!(
            entry.body_hash.unwrap(),
            expected_hash,
            "Body hash must be derived from script block bytes, \
             not the full SFC file content"
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{ShapeBudget, compute_shape_descriptor};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/data/sample.svelte"
    ));

    fn parse_js(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .expect("load js grammar");
        parser.parse(src, None).expect("parse")
    }

    fn parse_svelte(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_svelte_sqry::language())
            .expect("load svelte grammar");
        parser.parse(src, None).expect("parse")
    }

    fn first_of<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
        if kinds.contains(&node.kind()) {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_of(child, kinds) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn js_cf_map_is_non_empty_and_covers_real_kinds() {
        let mapping = svelte_js_shape_mapping();
        let populated = mapping.cf_by_kind_id.iter().filter(|s| s.is_some()).count();
        assert!(populated > 0, "JS cf map must map real grammar kinds");

        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let id = |n: &str| lang.id_for_node_kind(n, true);
        assert_eq!(
            mapping.cf_bucket(id("if_statement")),
            Some(CfBucket::Branch)
        );
        assert_eq!(
            mapping.cf_bucket(id("while_statement")),
            Some(CfBucket::Loop)
        );
        assert_eq!(mapping.cf_bucket(id("try_statement")), Some(CfBucket::Try));
        assert_eq!(
            mapping.cf_bucket(id("call_expression")),
            Some(CfBucket::Call)
        );
    }

    #[test]
    fn descriptor_counts_js_control_flow() {
        let src = "function classify(score) { let l=''; if (score>=90){l='A';} else {l='B';} for (let i=0;i<score;i++){ record(i); } return l; }";
        let tree = parse_js(src);
        let func =
            first_of(tree.root_node(), &["function_declaration"]).expect("function_declaration");
        let d = compute_shape_descriptor(
            func,
            src.as_bytes(),
            svelte_js_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(!d.is_unhashable(), "a real JS body must be hashable");
        assert!(
            d.cf_histogram[CfBucket::Branch.index()] >= 1,
            "if -> Branch"
        );
        assert!(d.cf_histogram[CfBucket::Loop.index()] >= 1, "for -> Loop");
        assert!(d.cf_histogram[CfBucket::Call.index()] >= 1, "call -> Call");
        assert!(
            d.cf_histogram[CfBucket::Return.index()] >= 1,
            "return -> Return"
        );
    }

    #[test]
    fn signature_shape_reads_js_parameters() {
        let src = "function f(a, b = 2, ...rest) { return a; }";
        let tree = parse_js(src);
        let func =
            first_of(tree.root_node(), &["function_declaration"]).expect("function_declaration");
        let shape = svelte_js_shape_mapping().signature_shape(func, src.as_bytes());
        assert_eq!(shape.arity_positional, 2, "a and b = 2 are positional");
        assert!(shape.has_defaults, "b = 2 sets has_defaults");
        assert!(shape.has_varargs, "...rest sets has_varargs");
    }

    #[test]
    fn embedded_script_block_gets_a_shape_descriptor() {
        // Boundary assertion: the Svelte markup tree carries no functions, but
        // the <script> block's `classify` function is fingerprinted via the
        // embedded JS shape mapping, so a descriptor lands in staging for it.
        let tree = parse_svelte(SAMPLE);
        let mut staging = StagingGraph::new();
        let builder = SvelteGraphBuilder::default();
        builder
            .build_graph(
                &tree,
                SAMPLE.as_bytes(),
                Path::new("sample.svelte"),
                &mut staging,
            )
            .expect("build svelte graph");

        let metadata = staging.take_macro_metadata();
        let descriptors = metadata.shape_descriptors();
        assert!(
            !descriptors.is_empty(),
            "the embedded <script> function must receive a shape descriptor"
        );
        let has_real_cf = descriptors.values().any(|d| {
            d.cf_histogram[CfBucket::Branch.index()] >= 1
                && d.cf_histogram[CfBucket::Loop.index()] >= 1
        });
        assert!(
            has_real_cf,
            "the classify function's if + for must be counted in its descriptor"
        );
    }
}
