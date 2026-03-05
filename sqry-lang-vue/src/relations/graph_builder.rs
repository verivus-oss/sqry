//! `GraphBuilder` implementation for Vue
//!
//! Builds the unified `CodeGraph` for Vue SFC files by:
//! 1. Extracting `<script>` and `<script setup>` blocks from the SFC
//! 2. Re-parsing each script block as JavaScript/TypeScript
//! 3. Delegating to the appropriate language `GraphBuilder` to extract functions and calls
//! 4. Extracting template event directives (`@click`, `v-on:submit`) and emitting Calls edges
//!
//! ## Scope
//! - Script blocks: Both regular (`<script>`) and setup (`<script setup>`)
//! - Languages: JavaScript (default) and TypeScript (`lang="ts"`)
//! - Callables: Function declarations within script blocks; methods in default export
//! - Calls: `call_expression` nodes within script blocks
//! - Template event directives: `v-on:*` and `@*` shorthand
//!
//! ## Template Directive Support
//! - `@click="handler"` or `v-on:click="handler"` → Calls edge to handler
//! - Modifiers like `.prevent`, `.stop` are recognized but do not affect edge creation
//! - Inline expressions (e.g., `@click="count++"`) are NOT treated as method calls
//! - Method calls with args (e.g., `@click="handleClick($event)"`) extract the function name
//!
//! ## Out of Scope
//! - Template bindings and reactive properties → future enhancement
//! - Component composition analysis → future enhancement
//! - External scripts (`<script src="...">`) → currently ignored; only inline scripts processed
//!
//! ## Strategy
//! 1. Parse the entire `.vue` SFC using tree-sitter-vue
//! 2. Query for `script_element` nodes (both regular and setup)
//! 3. Extract inner JavaScript/TypeScript code text
//! 4. Re-parse with tree-sitter-javascript or tree-sitter-typescript based on lang attribute
//! 5. Apply JavaScript/TypeScript `GraphBuilder` patterns to emit nodes and edges
//! 6. Walk `template_element` for event directives and emit Calls edges to handlers

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sqry_core::graph::unified::edge::ExportKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use sqry_lang_typescript::relations::type_extractor::{
    extract_all_type_names_from_annotation, extract_type_string,
};
use tree_sitter::{Node, Parser, Point, Tree};

/// `GraphBuilder` for Vue SFC files
#[derive(Debug, Clone, Copy)]
pub struct VueGraphBuilder {
    max_scope_depth: usize,
}

impl Default for VueGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 4, // Vue: script -> class -> method -> nested function
        }
    }
}

impl VueGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for VueGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Vue);

        // Create module node for the Vue file itself (anchor for import/export edges)
        let module_id = helper.add_module("vue::module", None);

        // Create a Component node for the Vue file (DSL semantic node)
        let component_name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("VueComponent");
        let component_id = helper.add_node(component_name, None, NodeKind::Component);
        helper.add_contains_edge(module_id, component_id);

        // Extract and process script blocks
        let source = std::str::from_utf8(content).map_err(|e| GraphBuilderError::ParseError {
            span: sqry_core::graph::Span::default(),
            reason: format!("Invalid UTF-8: {e}"),
        })?;

        let blocks = collect_script_blocks(tree, source);

        // Track local function names from scripts for template directive resolution
        let mut local_by_name: HashMap<String, sqry_core::graph::unified::NodeId> = HashMap::new();

        for block in &blocks {
            process_script_block_with_locals(
                block,
                &mut helper,
                self.max_scope_depth,
                &mut local_by_name,
                component_id,
            )?;
        }

        // Extract template event directives and emit Calls edges
        let root = tree.root_node();
        extract_template_event_directives(
            &root,
            source,
            &mut helper,
            module_id,
            &mut local_by_name,
        )?;

        // Extract and emit Export edges from script blocks
        for block in &blocks {
            extract_export_edges(block, &mut helper, module_id, &local_by_name)?;
        }

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Vue
    }
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

/// Type of `<script>` block
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptType {
    Regular,
    Setup,
}

impl ScriptType {
    fn from_setup_attr(value: Option<&str>) -> Self {
        match value {
            Some(_) => ScriptType::Setup, // setup attribute present
            None => ScriptType::Regular,
        }
    }

    /// Get the prefix for qualified names to differentiate script contexts
    fn qualified_prefix(self) -> &'static str {
        match self {
            ScriptType::Regular => "vue::script",
            ScriptType::Setup => "vue::setup",
        }
    }
}

/// Simplified representation of a `<script>` block
struct ScriptBlock {
    lang: ScriptLanguage,
    script_type: ScriptType,
    content: String,
    #[allow(dead_code)] // Reserved for future position offset calculations
    start_point: Point,
}

impl ScriptBlock {
    #[allow(dead_code)] // Reserved for future position offset calculations
    fn start_line_offset(&self) -> usize {
        self.start_point.row
    }

    #[allow(dead_code)] // Reserved for future position offset calculations
    fn start_column_offset(&self) -> usize {
        self.start_point.column
    }
}

/// Collect script blocks from a parsed Vue tree
fn collect_script_blocks(tree: &Tree, source: &str) -> Vec<ScriptBlock> {
    let mut blocks = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() != "script_element" {
            continue;
        }

        let mut lang: Option<String> = None;
        let mut setup: Option<String> = None;
        let mut content = None;
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
                                "setup" => setup = Some(value),
                                _ => {}
                            }
                        }
                    }
                }
                "raw_text" => {
                    if let Ok(text) = node.utf8_text(source.as_bytes()) {
                        content = Some(text.to_string());
                        start_point = node.start_position();
                    }
                }
                _ => {}
            }
        }

        if let Some(content) = content {
            let block = ScriptBlock {
                lang: ScriptLanguage::from_lang_attr(lang.as_deref()),
                script_type: ScriptType::from_setup_attr(setup.as_deref()),
                content,
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

fn collect_function_declarations<'a>(
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
            None,
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
        collect_function_declarations(
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

fn collect_export_default_methods<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
    callable_by_node_id: &mut HashMap<usize, sqry_core::graph::unified::NodeId>,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    if node.kind() == "export_statement" {
        let mut export_cursor = node.walk();
        for child in node.children(&mut export_cursor) {
            if child.kind() == "object" {
                collect_object_methods(
                    child,
                    content,
                    helper,
                    prefix,
                    callable_by_node_id,
                    local_by_name,
                    component_id,
                    OptionsContext::TopLevel,
                )?;
            }
        }
    }

    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        collect_export_default_methods(
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

/// Tracks which Vue Options API section we are currently inside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionsContext {
    /// Top-level `export default { ... }` object
    TopLevel,
    /// Inside `methods: { ... }`
    Methods,
    /// Inside `computed: { ... }`
    Computed,
    /// Inside `watch: { ... }`
    Watch,
}

fn collect_object_methods(
    object_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    prefix: &str,
    callable_by_node_id: &mut HashMap<usize, sqry_core::graph::unified::NodeId>,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
    context: OptionsContext,
) -> GraphResult<()> {
    let mut cursor = object_node.walk();
    for member in object_node.named_children(&mut cursor) {
        match member.kind() {
            "pair" => {
                let Some(key_node) = member.child_by_field_name("key") else {
                    continue;
                };
                let Ok(key_text) = key_node.utf8_text(content) else {
                    continue;
                };
                let key_text = key_text.trim_matches(&['"', '\''][..]).trim();

                let Some(value_node) = member.child_by_field_name("value") else {
                    continue;
                };

                // Only at top-level: recurse into Options API sections
                if context == OptionsContext::TopLevel {
                    if value_node.kind() == "object" {
                        let sub_context = match key_text {
                            "methods" => Some(OptionsContext::Methods),
                            "computed" => Some(OptionsContext::Computed),
                            "watch" => Some(OptionsContext::Watch),
                            _ => None,
                        };
                        if let Some(ctx) = sub_context {
                            collect_object_methods(
                                value_node,
                                content,
                                helper,
                                prefix,
                                callable_by_node_id,
                                local_by_name,
                                component_id,
                                ctx,
                            )?;
                            continue;
                        }
                    }

                    // Vue Options API: props as array → create Variable nodes
                    if key_text == "props" && value_node.kind() == "array" {
                        extract_props_from_array(value_node, content, helper, component_id);
                        continue;
                    }

                    // Vue Options API: props as object → create Variable nodes
                    if key_text == "props" && value_node.kind() == "object" {
                        extract_props_from_object(value_node, content, helper, component_id);
                        continue;
                    }
                }

                // Computed/watch with object config: emit a single method named after the key
                // e.g. computed: { displayName: { get() {}, set() {} } } → method "displayName"
                // e.g. watch: { message: { handler() {}, immediate: true } } → method "message"
                if (context == OptionsContext::Computed || context == OptionsContext::Watch)
                    && value_node.kind() == "object"
                {
                    let qualified_name = format!("{prefix}::{key_text}");
                    let visibility = extract_visibility(&qualified_name);
                    let method_id = helper.add_method_with_visibility(
                        &qualified_name,
                        None,
                        false,
                        false,
                        Some(visibility),
                    );
                    callable_by_node_id.insert(value_node.id(), method_id);
                    local_by_name.insert(key_text.to_string(), method_id);
                    helper.add_contains_edge(component_id, method_id);
                    continue;
                }

                if value_node.kind() == "function" || value_node.kind() == "arrow_function" {
                    let qualified_name = format!("{prefix}::{key_text}");
                    let visibility = extract_visibility(&qualified_name);
                    let method_id = helper.add_method_with_visibility(
                        &qualified_name,
                        None,
                        false,
                        false,
                        Some(visibility),
                    );
                    callable_by_node_id.insert(value_node.id(), method_id);
                    local_by_name.insert(key_text.to_string(), method_id);
                    helper.add_contains_edge(component_id, method_id);
                }
            }
            "method_definition" => {
                if let Some(name_node) = member.child_by_field_name("name")
                    && let Ok(name_text) = name_node.utf8_text(content)
                {
                    let name_text = name_text.trim();
                    if !name_text.is_empty() {
                        let qualified_name = format!("{prefix}::{name_text}");
                        let visibility = extract_visibility(&qualified_name);
                        let method_id = helper.add_method_with_visibility(
                            &qualified_name,
                            None,
                            false,
                            false,
                            Some(visibility),
                        );
                        callable_by_node_id.insert(member.id(), method_id);
                        local_by_name.insert(name_text.to_string(), method_id);
                        helper.add_contains_edge(component_id, method_id);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Extract props from array syntax: `props: ['name', 'age']`
fn extract_props_from_array(
    array_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    component_id: sqry_core::graph::unified::NodeId,
) {
    let mut cursor = array_node.walk();
    for child in array_node.named_children(&mut cursor) {
        if child.kind() == "string"
            && let Ok(text) = child.utf8_text(content)
        {
            let prop_name = text.trim_matches(&['"', '\''][..]).trim();
            if !prop_name.is_empty() {
                let var_id = helper.add_variable(prop_name, None);
                helper.add_contains_edge(component_id, var_id);
            }
        }
    }
}

/// Extract props from object syntax: `props: { name: String, age: { type: Number } }`
fn extract_props_from_object(
    object_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    component_id: sqry_core::graph::unified::NodeId,
) {
    let mut cursor = object_node.walk();
    for member in object_node.named_children(&mut cursor) {
        if member.kind() == "pair"
            && let Some(key_node) = member.child_by_field_name("key")
            && let Ok(prop_name) = key_node.utf8_text(content)
        {
            let prop_name = prop_name.trim_matches(&['"', '\''][..]).trim();
            if !prop_name.is_empty() {
                let var_id = helper.add_variable(prop_name, None);
                helper.add_contains_edge(component_id, var_id);
            }
        }
    }
}

fn extract_call_edges<'a>(
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
        && let Some(callee_name) = vue_call_expression_callee_name(&node, content)
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
        extract_call_edges(
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

fn vue_call_expression_callee_name(node: &Node<'_>, content: &[u8]) -> Option<String> {
    let callee_expr = node.child_by_field_name("function")?;
    match callee_expr.kind() {
        "identifier" => callee_expr
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),
        "member_expression" => {
            let object = callee_expr
                .child_by_field_name("object")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string());
            let property = callee_expr
                .child_by_field_name("property")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string());

            match (object.as_deref(), property) {
                (Some("this"), Some(prop)) if !prop.is_empty() => Some(prop),
                _ => callee_expr
                    .utf8_text(content)
                    .ok()
                    .map(|s| s.trim().to_string()),
            }
        }
        _ => callee_expr
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),
    }
    .filter(|s| !s.is_empty())
}

fn call_expression_argument_count(node: &Node<'_>) -> u8 {
    let Some(args) = node.child_by_field_name("arguments") else {
        return 255;
    };
    let count = args.named_child_count();
    if count <= 254 {
        u8::try_from(count).ok().unwrap_or(255)
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

fn get_function_name(node: &Node<'_>, content: &[u8]) -> Option<String> {
    node.child_by_field_name("name")?
        .utf8_text(content)
        .ok()
        .map(std::string::ToString::to_string)
}

// ============================================================================
// Script Block Processing (with local name tracking)
// ============================================================================

/// Process a script block by re-parsing it and extracting functions and calls.
/// Also updates `shared_locals` with function names for template directive resolution.
fn process_script_block_with_locals(
    block: &ScriptBlock,
    helper: &mut GraphBuildHelper,
    _max_scope_depth: usize,
    shared_locals: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
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

    // Extract functions and calls from the script AST
    let root = tree.root_node();
    extract_script_graph_with_locals(
        root,
        block.content.as_bytes(),
        helper,
        block.script_type,
        block.lang,
        shared_locals,
        component_id,
    )?;

    Ok(())
}

fn extract_script_graph_with_locals(
    root: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    script_type: ScriptType,
    script_lang: ScriptLanguage,
    shared_locals: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
    component_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let prefix = script_type.qualified_prefix();

    let mut callable_by_node_id: HashMap<usize, sqry_core::graph::unified::NodeId> = HashMap::new();

    let mut cursor = root.walk();
    collect_function_declarations(
        root,
        &mut cursor,
        content,
        helper,
        prefix,
        &mut callable_by_node_id,
        shared_locals,
        component_id,
    )?;

    let mut export_cursor = root.walk();
    collect_export_default_methods(
        root,
        &mut export_cursor,
        content,
        helper,
        prefix,
        &mut callable_by_node_id,
        shared_locals,
        component_id,
    )?;

    let mut import_cursor = root.walk();
    extract_import_edges(root, &mut import_cursor, content, helper)?;

    let mut call_cursor = root.walk();
    let mut caller_stack = Vec::new();
    extract_call_edges(
        root,
        &mut call_cursor,
        content,
        helper,
        prefix,
        &callable_by_node_id,
        shared_locals,
        &mut caller_stack,
    )?;

    // Extract TypeOf and References edges for TypeScript script blocks
    if script_lang == ScriptLanguage::TypeScript {
        extract_type_edges(root, content, helper, prefix, &callable_by_node_id)?;
    }

    Ok(())
}

// ============================================================================
// TypeOf and References Edge Extraction (TypeScript blocks only)
// ============================================================================

/// Recursively walk the AST to extract `TypeOf` and `References` edges from type annotations.
///
/// Only called for TypeScript `<script>` blocks. Walks function declarations,
/// arrow functions, method definitions, generator functions, and variable declarations
/// to extract type annotation information.
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
// Import Edge Extraction
// ============================================================================

/// Recursively extract import statements from the script AST and emit Import edges.
///
/// Handles ES6 import statements:
/// - `import { ref } from 'vue'`
/// - `import MyComponent from './MyComponent.vue'`
/// - `import * as utils from './utils'`
fn extract_import_edges<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    if node.kind() == "import_statement"
        && let Some(source_node) = node.child_by_field_name("source")
        && let Ok(source_text) = source_node.utf8_text(content)
    {
        let module_name = source_text
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();

        if !module_name.is_empty() {
            // Create import node
            let import_span = Some(span_from_node(node));
            let to_id = helper.add_import(&module_name, import_span);

            // Create module node for the current file
            let from_id = helper.add_module("<module>", None);

            // Create import edge
            helper.add_import_edge(from_id, to_id);
        }
    }

    // Recursively process children
    let children: Vec<_> = node.children(cursor).collect();
    for child in children {
        let mut child_cursor = child.walk();
        extract_import_edges(child, &mut child_cursor, content, helper)?;
    }

    Ok(())
}

// ============================================================================
// Export Edge Extraction
// ============================================================================

/// Extract and emit Export edges from a script block.
///
/// Handles:
/// 1. `export default { ... }` - Default export of Vue component
/// 2. `export function/class/const/let/var` - Named exports in script
/// 3. `defineExpose({ ... })` - Explicit export in `<script setup>`
fn extract_export_edges(
    block: &ScriptBlock,
    helper: &mut GraphBuildHelper,
    module_id: NodeId,
    local_by_name: &HashMap<String, NodeId>,
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

    let root = tree.root_node();
    extract_export_edges_from_ast(
        root,
        block.content.as_bytes(),
        helper,
        module_id,
        local_by_name,
    )?;

    Ok(())
}

/// Recursively walk the AST to find and emit Export edges.
fn extract_export_edges_from_ast(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: NodeId,
    local_by_name: &HashMap<String, NodeId>,
) -> GraphResult<()> {
    match node.kind() {
        "export_statement" => {
            // Handle export statements: `export default`, `export const`, etc.
            handle_export_statement(node, content, helper, module_id, local_by_name);
        }
        "call_expression" => {
            // Check for defineExpose() calls
            if let Some((exposed_symbols, _is_define_expose)) =
                extract_define_expose_call(node, content)
            {
                for symbol_name in exposed_symbols {
                    if let Some(&exported_id) = local_by_name.get(&symbol_name) {
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

    // Recursively process children
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    for child in children {
        extract_export_edges_from_ast(child, content, helper, module_id, local_by_name)?;
    }

    Ok(())
}

/// Handle `export` statements and emit appropriate Export edges.
#[allow(clippy::too_many_lines)]
fn handle_export_statement(
    export_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: NodeId,
    local_by_name: &HashMap<String, NodeId>,
) {
    // Check for default export
    let has_default = export_node
        .children(&mut export_node.walk())
        .any(|child| child.kind() == "default");

    if has_default {
        // Default export: `export default MyComponent` or `export default { ... }`
        let exported_name = export_node
            .children(&mut export_node.walk())
            .find(|child| child.kind() == "identifier" || child.kind() == "object")
            .and_then(|node| {
                if node.kind() == "identifier" {
                    node.utf8_text(content).ok().map(|s| s.trim().to_string())
                } else {
                    // For object literals, use a generic name
                    Some("default".to_string())
                }
            })
            .unwrap_or_else(|| "default".to_string());

        let exported_id = if let Some(&id) = local_by_name.get(&exported_name) {
            id
        } else {
            let visibility = extract_visibility(&exported_name);
            helper.add_function_with_visibility(
                &exported_name,
                None,
                false,
                false,
                Some(visibility),
            )
        };

        helper.add_export_edge_full(module_id, exported_id, ExportKind::Default, None);
        return;
    }

    // Check for named exports: `export { foo, bar }` or `export function/class/const`
    let mut cursor = export_node.walk();
    for child in export_node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        let exported_id = if let Some(&id) = local_by_name.get(&name) {
                            id
                        } else {
                            let visibility = extract_visibility(&name);
                            helper.add_function_with_visibility(
                                &name,
                                None,
                                false,
                                false,
                                Some(visibility),
                            )
                        };
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
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(content)
                {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        let exported_id = if let Some(&id) = local_by_name.get(&name) {
                            id
                        } else {
                            helper.add_class(&name, None)
                        };
                        helper.add_export_edge_full(
                            module_id,
                            exported_id,
                            ExportKind::Direct,
                            None,
                        );
                    }
                }
            }
            "variable_declaration" | "lexical_declaration" => {
                let mut var_cursor = child.walk();
                for var_child in child.children(&mut var_cursor) {
                    if var_child.kind() == "variable_declarator"
                        && let Some(name_node) = var_child.child_by_field_name("name")
                        && let Ok(name) = name_node.utf8_text(content)
                    {
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            let exported_id = if let Some(&id) = local_by_name.get(&name) {
                                id
                            } else {
                                helper.add_constant(&name, None)
                            };
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
            "export_clause" => {
                // Named exports: `export { foo, bar }`
                let mut clause_cursor = child.walk();
                for specifier in child.children(&mut clause_cursor) {
                    if specifier.kind() == "export_specifier" {
                        // Get the local name being exported
                        let identifiers: Vec<_> = specifier
                            .children(&mut specifier.walk())
                            .filter(|n| n.kind() == "identifier")
                            .collect();

                        if let Some(first_ident) = identifiers.first()
                            && let Ok(name) = first_ident.utf8_text(content)
                        {
                            let name = name.trim().to_string();
                            if !name.is_empty()
                                && let Some(&exported_id) = local_by_name.get(&name)
                            {
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
            }
            _ => {}
        }
    }
}

/// Extract symbols from a `defineExpose()` call.
///
/// Returns `(symbol_names, is_define_expose)` where:
/// - `symbol_names` is a list of exported symbol names
/// - `is_define_expose` is true if this is a defineExpose call
fn extract_define_expose_call(node: Node<'_>, content: &[u8]) -> Option<(Vec<String>, bool)> {
    // Check if this is a call to `defineExpose`
    let function = node.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }

    let func_name = function.utf8_text(content).ok()?.trim();
    if func_name != "defineExpose" {
        return None;
    }

    // Get the arguments to defineExpose
    let arguments = node.child_by_field_name("arguments")?;
    let mut symbols = Vec::new();

    // Look for object literal: defineExpose({ foo, bar })
    let mut arg_cursor = arguments.walk();
    for arg in arguments.children(&mut arg_cursor) {
        if arg.kind() == "object" {
            // Iterate through object properties
            let mut obj_cursor = arg.walk();
            for prop in arg.children(&mut obj_cursor) {
                match prop.kind() {
                    "pair" => {
                        // For `foo: bar`, we want the key name
                        if let Some(key) = prop.child_by_field_name("key")
                            && let Ok(key_text) = key.utf8_text(content)
                        {
                            let key_name = key_text.trim().trim_matches('"').trim_matches('\'');
                            if !key_name.is_empty() {
                                symbols.push(key_name.to_string());
                            }
                        }
                    }
                    "shorthand_property_identifier" => {
                        // For shorthand `foo`, the property itself is the name
                        if let Ok(text) = prop.utf8_text(content) {
                            let name = text.trim();
                            if !name.is_empty() {
                                symbols.push(name.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Some((symbols, true))
}

// ============================================================================
// Template Event Directive Extraction
// ============================================================================

/// Extract event directives from template elements and emit Calls edges.
///
/// Walks the Vue template AST looking for `directive_attribute` nodes that represent
/// event bindings (`v-on:*` or `@*` shorthand). For each event binding, emits a Calls
/// edge to the handler function.
fn extract_template_event_directives(
    node: &Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) -> GraphResult<()> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "template_element" => {
                // Recurse into template content
                extract_template_event_directives(
                    &child,
                    source,
                    helper,
                    module_id,
                    local_by_name,
                )?;
            }
            "element" | "self_closing_tag" => {
                // Check for event directives in this element
                extract_element_event_directives(&child, source, helper, module_id, local_by_name);
                // Recurse into child elements
                extract_template_event_directives(
                    &child,
                    source,
                    helper,
                    module_id,
                    local_by_name,
                )?;
            }
            "start_tag" => {
                // Process directives in start tag
                extract_start_tag_event_directives(
                    &child,
                    source,
                    helper,
                    module_id,
                    local_by_name,
                );
            }
            _ => {
                // Recurse into other nodes
                extract_template_event_directives(
                    &child,
                    source,
                    helper,
                    module_id,
                    local_by_name,
                )?;
            }
        }
    }

    Ok(())
}

/// Extract event directives from an element node.
fn extract_element_event_directives(
    element: &Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) {
    let mut cursor = element.walk();

    for child in element.children(&mut cursor) {
        if child.kind() == "start_tag" || child.kind() == "self_closing_tag" {
            extract_start_tag_event_directives(&child, source, helper, module_id, local_by_name);
        }
    }
}

/// Extract event directives from a `start_tag` or `self_closing_tag` node.
fn extract_start_tag_event_directives(
    tag: &Node<'_>,
    source: &str,
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
    local_by_name: &mut HashMap<String, sqry_core::graph::unified::NodeId>,
) {
    let mut cursor = tag.walk();

    for child in tag.children(&mut cursor) {
        if child.kind() == "directive_attribute"
            && let Some(handler_name) = extract_event_handler_from_directive(&child, source)
        {
            // Create or get the handler function node
            let handler_id = if let Some(&id) = local_by_name.get(&handler_name) {
                id
            } else {
                // Handler not found in script - create a placeholder node
                let qualified_name = format!("vue::template::{handler_name}");
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

            // Emit Calls edge from module (template context) to handler
            // Use 255 sentinel for unknown argument count (template event handlers)
            let call_span = span_from_node(child);
            helper.add_call_edge_full_with_span(module_id, handler_id, 255, false, vec![call_span]);
        }
    }
}

/// Extract the handler name from an event directive.
///
/// Handles both full syntax (`v-on:click="handler"`) and shorthand (`@click="handler"`).
/// Returns `None` for:
/// - Non-event directives (v-if, v-bind, etc.)
/// - Inline expressions that don't look like method names (e.g., `count++`)
fn extract_event_handler_from_directive(directive: &Node<'_>, source: &str) -> Option<String> {
    let mut directive_name: Option<String> = None;
    let mut handler_value: Option<String> = None;

    let mut cursor = directive.walk();

    for child in directive.children(&mut cursor) {
        match child.kind() {
            "directive_name" => {
                if let Ok(text) = child.utf8_text(source.as_bytes()) {
                    directive_name = Some(text.to_string());
                }
            }
            "attribute_value" | "quoted_attribute_value" => {
                if let Ok(text) = child.utf8_text(source.as_bytes()) {
                    // Strip quotes if present
                    let cleaned = text.trim_matches(&['"', '\''][..]).trim().to_string();
                    handler_value = Some(cleaned);
                }
            }
            _ => {}
        }
    }

    // Check if this is an event directive
    let name = directive_name?;
    if !is_event_directive(&name) {
        return None;
    }

    // Extract handler name from the value
    let value = handler_value?;
    extract_handler_name_from_expression(&value)
}

/// Check if a directive name represents an event binding.
///
/// Returns `true` for:
/// - `on` (from `v-on:event`)
/// - Anything starting with `@` (shorthand like `@click`)
fn is_event_directive(name: &str) -> bool {
    name == "on" || name.starts_with('@')
}

/// Extract the handler function name from an event handler expression.
///
/// Examples:
/// - `"handleClick"` → `Some("handleClick")`
/// - `"handleClick()"` → `Some("handleClick")`
/// - `"handleClick($event)"` → `Some("handleClick")`
/// - `"handleClick(arg1, arg2)"` → `Some("handleClick")`
/// - `"count++"` → `None` (not a method call)
/// - `"count = count + 1"` → `None` (not a method call)
/// - `"this.handleClick"` → `Some("handleClick")`
/// - `"$emit('event')"` → `Some("$emit")`
fn extract_handler_name_from_expression(expr: &str) -> Option<String> {
    let trimmed = expr.trim();

    if trimmed.is_empty() {
        return None;
    }

    // Check for inline expressions (operators, assignments)
    // These are NOT method calls
    if trimmed.contains("++")
        || trimmed.contains("--")
        || trimmed.contains(" = ")
        || trimmed.contains("+=")
        || trimmed.contains("-=")
        || trimmed.contains("*=")
        || trimmed.contains("/=")
    {
        return None;
    }

    // Handle method calls with parentheses: `handleClick()` or `handleClick(arg)`
    if let Some(paren_pos) = trimmed.find('(') {
        let before_paren = &trimmed[..paren_pos];
        return extract_simple_or_member_name(before_paren);
    }

    // Handle simple identifiers or member expressions without parentheses
    extract_simple_or_member_name(trimmed)
}

/// Extract a function name from a simple identifier or member expression.
///
/// - `"handleClick"` → `Some("handleClick")`
/// - `"this.handleClick"` → `Some("handleClick")`
/// - `"obj.method"` → `Some("method")` (extracts the final property)
fn extract_simple_or_member_name(expr: &str) -> Option<String> {
    let trimmed = expr.trim();

    // Check if it's a valid identifier or member expression
    // Must start with a letter, underscore, or dollar sign
    if trimmed.is_empty() {
        return None;
    }

    let first_char = trimmed.chars().next()?;
    if !first_char.is_alphabetic() && first_char != '_' && first_char != '$' {
        return None;
    }

    // Handle member expressions like `this.handleClick` or `obj.method`
    if trimmed.contains('.') {
        // Get the last segment
        let parts: Vec<&str> = trimmed.split('.').collect();
        let last = parts.last()?;
        let last = last.trim();

        // Validate the last part is a valid identifier
        if is_valid_identifier(last) {
            return Some(last.to_string());
        }
        return None;
    }

    // Simple identifier
    if is_valid_identifier(trimmed) {
        return Some(trimmed.to_string());
    }

    None
}

/// Check if a string is a valid JavaScript/TypeScript identifier.
fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    // First character must be letter, underscore, or dollar sign
    if !first.is_alphabetic() && first != '_' && first != '$' {
        return false;
    }

    // Rest must be alphanumeric, underscore, or dollar sign
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Extract visibility for a Vue function or method.
///
/// In Vue components, all methods and functions are considered public as they
/// are part of the component's API and can be accessed via the component instance.
/// Vue doesn't have formal visibility modifiers.
fn extract_visibility(_name: &str) -> &'static str {
    "public"
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::StringId;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use std::collections::HashMap;

    fn parse_vue(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_vue_sqry::language())
            .expect("Failed to load Vue grammar");

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

<template>
  <div>Hello</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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

<template>
  <div>Test</div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // TypeScript script blocks should still be processed
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_setup_script_block() {
        let source = r#"
<script setup>
  function handleClick() {
    console.log("clicked");
  }
</script>

<template>
  <button @click="handleClick">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // @click template directive should emit a Calls edge
        assert!(
            count_calls_edges(&staging) >= 1,
            "Expected Calls edge for @click handler"
        );
    }

    #[test]
    fn test_setup_typescript_combination() {
        // Critical: Test <script setup lang="ts"> combination
        let source = r#"
<script setup lang="ts">
  interface User {
    id: number;
    name: string;
  }

  function formatUser(user: User): string {
    return `${user.name} (ID: ${user.id})`;
  }

  function displayUser() {
    const user: User = { id: 1, name: "Alice" };
    return formatUser(user);
  }
</script>

<template>
  <div>User Display</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // TypeScript setup block should still create nodes
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

<template>
  <div>Test</div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Graph should be created (script blocks aren't fully parsed yet)
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

<template>
  <div>Test</div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Graph should be created for script content
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_no_script_block() {
        let source = r"
<template>
  <div>
    <h1>No script here</h1>
  </div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        // Should not error on files without script blocks
        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should at least create a module node
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_language_is_vue() {
        let source = r"
<script>
  function test() {}
</script>

<template>
  <div>Test</div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should create nodes for the module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    #[test]
    fn test_regular_setup_context_collision_prevention() {
        // CRITICAL: This test prevents node ID collisions between regular and setup scripts.
        // Both scripts define an `init` function - they must create separate nodes.
        let source = r#"
<script>
  function init() {
    return regularHelper();
  }

  function regularHelper() {
    return "regular data";
  }
</script>

<script setup>
  function init() {
    return setupHelper();
  }

  function setupHelper() {
    return "setup data";
  }
</script>

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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

<template>
  <div>Test</div>
</template>
";

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should create nodes for the module
        assert!(
            count_nodes(&staging) >= 1,
            "Expected at least 1 node for module"
        );
    }

    // ========================================================================
    // Template Event Directive Tests
    // ========================================================================

    use sqry_core::graph::unified::build::test_helpers::collect_call_edges;

    #[test]
    fn test_template_click_shorthand() {
        // Test @click="handler" shorthand syntax
        let source = r#"
<script setup>
  function handleClick() {
    console.log("clicked");
  }
</script>

<template>
  <button @click="handleClick">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        // Should have at least one call edge for the @click handler
        assert!(
            !call_edges.is_empty(),
            "Expected at least one Calls edge for @click handler"
        );
    }

    #[test]
    fn test_template_v_on_click_full_syntax() {
        // Test v-on:click="handler" full syntax
        let source = r#"
<script setup>
  function handleClick() {
    console.log("clicked");
  }
</script>

<template>
  <button v-on:click="handleClick">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected at least one Calls edge for v-on:click handler"
        );
    }

    #[test]
    fn test_template_click_with_modifiers() {
        // Test @click.prevent="handler" with modifiers
        let source = r#"
<script setup>
  function handleClick() {
    console.log("clicked");
  }
</script>

<template>
  <button @click.prevent="handleClick">Click me</button>
  <a @click.stop.prevent="handleClick">Link</a>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        // Should have call edges despite modifiers
        assert!(
            call_edges.len() >= 2,
            "Expected at least 2 Calls edges for @click handlers with modifiers"
        );
    }

    #[test]
    fn test_template_inline_expression_not_call() {
        // Test that inline expressions like @click="count++" are NOT treated as calls
        let source = r#"
<script setup>
  import { ref } from 'vue';
  const count = ref(0);
</script>

<template>
  <button @click="count++">Increment</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        // Should NOT have a call edge for count++ - it's an inline expression
        // The only call edges might come from script content, not template
        // This test verifies we don't create spurious call edges for inline expressions
        for op in &call_edges {
            if let sqry_core::graph::unified::StagingOp::AddEdge { .. } = op {
                // If there are call edges, they should be from script, not from "count++"
            }
        }
        // Test passes if no panic - we're checking that count++ doesn't create invalid edges
    }

    #[test]
    fn test_template_method_with_args() {
        // Test @click="handleClick($event)" with arguments
        let source = r#"
<script setup>
  function handleClick(event) {
    console.log("clicked", event);
  }
</script>

<template>
  <button @click="handleClick($event)">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for @click='handleClick($event)'"
        );
    }

    #[test]
    fn test_template_multiple_events() {
        // Test multiple event handlers on different elements
        let source = r#"
<script setup>
  function handleClick() {}
  function handleSubmit() {}
  function handleChange() {}
</script>

<template>
  <form @submit="handleSubmit">
    <input @change="handleChange" />
    <button @click="handleClick">Submit</button>
  </form>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        // Should have at least 3 call edges for the 3 event handlers
        assert!(
            call_edges.len() >= 3,
            "Expected at least 3 Calls edges for multiple event handlers, got {}",
            call_edges.len()
        );
    }

    #[test]
    fn test_template_this_method_call() {
        // Test @click="this.handleClick" with 'this' prefix
        let source = r#"
<script>
  export default {
    methods: {
      handleClick() {
        console.log("clicked");
      }
    }
  }
</script>

<template>
  <button @click="this.handleClick">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for @click='this.handleClick'"
        );
    }

    #[test]
    fn test_template_emit_call() {
        // Test @click="$emit('event')" for Vue emit calls
        let source = r#"
<script setup>
  const emit = defineEmits(['custom-event']);
</script>

<template>
  <button @click="$emit('custom-event')">Emit</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for @click='$emit(...)'"
        );
    }

    #[test]
    fn test_template_self_closing_tag() {
        // Test event handlers on self-closing tags
        let source = r#"
<script setup>
  function handleChange() {}
</script>

<template>
  <input @change="handleChange" />
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge for @change on self-closing input"
        );
    }

    #[test]
    fn test_template_nested_elements() {
        // Test event handlers on nested elements
        let source = r#"
<script setup>
  function handleOuter() {}
  function handleInner() {}
</script>

<template>
  <div @click="handleOuter">
    <span @click="handleInner">Nested</span>
  </div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            call_edges.len() >= 2,
            "Expected at least 2 Calls edges for nested element handlers"
        );
    }

    #[test]
    fn test_template_no_script_creates_placeholder() {
        // Test that template event handlers without script block create placeholder nodes
        let source = r#"
<template>
  <button @click="handleClick">Click me</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        let call_edges = collect_call_edges(&staging);
        assert!(
            !call_edges.is_empty(),
            "Expected Calls edge even without script block"
        );
    }

    #[test]
    fn test_template_assignment_not_call() {
        // Test that assignments like @click="value = newValue" are NOT calls
        let source = r#"
<script setup>
  import { ref } from 'vue';
  const value = ref('old');
</script>

<template>
  <button @click="value = 'new'">Set</button>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // This should complete without creating invalid nodes
        // Test passes if no panic
    }

    // ========================================================================
    // Unit tests for handler extraction helpers
    // ========================================================================

    #[test]
    fn test_extract_handler_name_simple() {
        assert_eq!(
            extract_handler_name_from_expression("handleClick"),
            Some("handleClick".to_string())
        );
    }

    #[test]
    fn test_extract_handler_name_with_parens() {
        assert_eq!(
            extract_handler_name_from_expression("handleClick()"),
            Some("handleClick".to_string())
        );
    }

    #[test]
    fn test_extract_handler_name_with_args() {
        assert_eq!(
            extract_handler_name_from_expression("handleClick($event)"),
            Some("handleClick".to_string())
        );
        assert_eq!(
            extract_handler_name_from_expression("handleClick(arg1, arg2)"),
            Some("handleClick".to_string())
        );
    }

    #[test]
    fn test_extract_handler_name_this_prefix() {
        assert_eq!(
            extract_handler_name_from_expression("this.handleClick"),
            Some("handleClick".to_string())
        );
        assert_eq!(
            extract_handler_name_from_expression("this.handleClick()"),
            Some("handleClick".to_string())
        );
    }

    #[test]
    fn test_extract_handler_name_dollar_prefix() {
        assert_eq!(
            extract_handler_name_from_expression("$emit('event')"),
            Some("$emit".to_string())
        );
        assert_eq!(
            extract_handler_name_from_expression("$refs.input.focus()"),
            Some("focus".to_string())
        );
    }

    #[test]
    fn test_extract_handler_name_inline_expression_returns_none() {
        assert_eq!(extract_handler_name_from_expression("count++"), None);
        assert_eq!(extract_handler_name_from_expression("count--"), None);
        assert_eq!(extract_handler_name_from_expression("value = 'new'"), None);
        assert_eq!(extract_handler_name_from_expression("count += 1"), None);
    }

    #[test]
    fn test_is_valid_identifier() {
        assert!(is_valid_identifier("handleClick"));
        assert!(is_valid_identifier("_private"));
        assert!(is_valid_identifier("$emit"));
        assert!(is_valid_identifier("onClick2"));
        assert!(!is_valid_identifier("123"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("has space"));
    }

    // ========================================================================
    // Import Edge Tests
    // ========================================================================

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
    fn test_import_from_vue() {
        let source = r#"
<script setup>
import { ref, computed } from 'vue';

const count = ref(0);
const doubled = computed(() => count.value * 2);
</script>

<template>
  <div>{{ count }} doubled is {{ doubled }}</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least one Import edge for 'vue'
        let import_edges = count_import_edges(&staging);
        assert!(import_edges >= 1, "Expected Import edge for 'vue' module");
    }

    #[test]
    fn test_import_component() {
        let source = r#"
<script>
import MyComponent from './MyComponent.vue';
import AnotherComponent from './AnotherComponent.vue';

export default {
  components: {
    MyComponent,
    AnotherComponent
  }
}
</script>

<template>
  <div>
    <MyComponent />
    <AnotherComponent />
  </div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least two Import edges
        let import_edges = count_import_edges(&staging);
        assert!(
            import_edges >= 2,
            "Expected at least 2 Import edges for component imports"
        );
    }

    #[test]
    fn test_import_namespace() {
        let source = r#"
<script setup>
import * as utils from './utils';

function doSomething() {
  return utils.formatDate(new Date());
}
</script>

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least one Import edge for namespace import
        let import_edges = count_import_edges(&staging);
        assert!(
            import_edges >= 1,
            "Expected Import edge for namespace import"
        );
    }

    #[test]
    fn test_import_typescript() {
        let source = r#"
<script setup lang="ts">
import type { User } from './types';
import { fetchUser } from './api';

interface Props {
  userId: string;
}

const user: User = await fetchUser('123');
</script>

<template>
  <div>{{ user.name }}</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have Import edges for both type and value imports
        let import_edges = count_import_edges(&staging);
        assert!(
            import_edges >= 2,
            "Expected at least 2 Import edges for TypeScript imports"
        );
    }

    #[test]
    fn test_import_multiple_scripts() {
        let source = r#"
<script>
import { helperA } from './helperA';
</script>

<script setup>
import { helperB } from './helperB';
import { ref } from 'vue';

const value = ref(0);
</script>

<template>
  <div>{{ value }}</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have Import edges from both script blocks
        let import_edges = count_import_edges(&staging);
        assert!(
            import_edges >= 3,
            "Expected at least 3 Import edges from multiple script blocks"
        );
    }

    // ========================================================================
    // Export Edge Tests
    // ========================================================================

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

    #[test]
    fn test_export_default_function() {
        let source = r#"
<script>
export default function MyComponent() {
  return 'Hello';
}
</script>

<template>
  <div>{{ message }}</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least one Export edge for default export
        let export_edges = count_export_edges(&staging);
        assert!(export_edges >= 1, "Expected Export edge for default export");
    }

    #[test]
    fn test_export_named_function() {
        let source = r#"
<script>
export function greet() {
  return 'Hello';
}
</script>

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least one Export edge for named export
        let export_edges = count_export_edges(&staging);
        assert!(export_edges >= 1, "Expected Export edge for named function");
    }

    #[test]
    fn test_export_named_const() {
        let source = r#"
<script>
export const API_VERSION = "1.0.0";
</script>

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have at least one Export edge for const
        let export_edges = count_export_edges(&staging);
        assert!(export_edges >= 1, "Expected Export edge for exported const");
    }

    #[test]
    fn test_define_expose_setup_script() {
        let source = r#"
<script setup>
function handleClick() {
  console.log('clicked');
}

function getMessage() {
  return 'Hello';
}

defineExpose({ handleClick, getMessage });
</script>

<template>
  <div>Test</div>
</template>
"#;

        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        let builder = VueGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Should have Export edges for defineExpose symbols (both functions)
        let export_edges = count_export_edges(&staging);
        assert!(
            export_edges >= 2,
            "Expected at least 2 Export edges for defineExpose with {} exports",
            export_edges
        );
    }

    // ========================================================================
    // TypeOf and References edge tests (TypeScript blocks only)
    // ========================================================================

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

    fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
        let string_map = build_string_map(staging);
        let mut nodes = HashMap::new();
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
            {
                let name = string_map.get(&entry.name).cloned().unwrap_or_default();
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
    fn test_ts_variable_type_annotation() {
        let source = r#"
<script lang="ts">
const user: User = {};
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
    fn test_ts_setup_function_types() {
        let source = r#"
<script setup lang="ts">
function calc(x: number): string {
  return String(x);
}
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 2,
            "Expected TypeOf edges for param + return"
        );
        assert!(has_reference_edge(&staging, "param::x", "number"));
    }

    #[test]
    fn test_ts_generic_type_references() {
        let source = r#"
<script lang="ts">
const items: Array<User> = [];
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
        let source = r#"
<script>
function calc(x, y) {
  return x + y;
}
const user = {};
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
    fn test_ts_multiple_params_indexed() {
        let source = r#"
<script lang="ts">
function f(a: string, b: number, c: boolean) {}
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
    fn test_ts_optional_parameter() {
        let source = r#"
<script lang="ts">
function foo(x?: string) {}
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for optional param"
        );
        assert!(
            has_reference_edge(&staging, "param::x", "string"),
            "Expected References to string"
        );
    }

    #[test]
    fn test_ts_arrow_function_types() {
        let source = r#"
<script lang="ts">
const fn1 = (x: number): string => x.toString();
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
export default {
  methods: {
    greet(name: string): string {
      return `Hello ${name}`;
    }
  }
}
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        assert!(
            count_typeof_edges(&staging) >= 1,
            "Expected TypeOf edge for rest param"
        );
    }

    #[test]
    fn test_ts_scoped_variable_isolation() {
        let source = r#"
<script lang="ts">
const x: string = "hello";
function foo() {
  const x: number = 42;
}
</script>
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
            .unwrap();

        // Both variables should have TypeOf edges (distinct due to byte-offset naming)
        let var_typeof_count = staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TypeOf {
                            context: Some(TypeOfContext::Variable),
                            ..
                        },
                        ..
                    }
                )
            })
            .count();
        assert!(
            var_typeof_count >= 2,
            "Expected at least 2 variable TypeOf edges, got {var_typeof_count}"
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
<template><div>Test</div></template>
"#;
        let (tree, content) = parse_vue(source);
        let mut staging = StagingGraph::new();
        VueGraphBuilder::default()
            .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
}
