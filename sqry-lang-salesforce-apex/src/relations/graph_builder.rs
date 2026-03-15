//! Salesforce Apex `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts relationships from Apex code:
//! - Class inheritance (extends)
//! - Interface implementations (implements)
//! - Method calls
//! - SOQL queries (sObject access) -> `TableRead` edges
//! - DML operations (insert/update/delete/upsert) -> `TableWrite` edges
//! - Database class method calls -> `TableRead`/`TableWrite` edges

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span,
    unified::{GraphBuildHelper, StagingGraph, TableWriteOp},
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

use super::type_extractor::{extract_all_type_names_from_annotation, extract_type_string};

/// `GraphBuilder` for Salesforce Apex files
#[derive(Debug, Default)]
pub struct ApexGraphBuilder;

impl ApexGraphBuilder {
    /// Create a new Apex graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[allow(dead_code)] // Scaffolding for language introspection
    fn language() -> tree_sitter::Language {
        tree_sitter_sfapex::apex::LANGUAGE.into()
    }
}

impl GraphBuilder for ApexGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for this file
        let mut helper = GraphBuildHelper::new(staging, file, Language::Apex);

        // Create module node from file path
        let module_name = extract_module_name_from_path(file);
        let module_id = helper.add_module(&module_name, None);

        // Compile tree-sitter queries
        let language = tree_sitter_sfapex::apex::LANGUAGE.into();
        let queries = ApexQueries::new(&language)?;

        // Extract callable definitions (classes, methods) for context
        let callables = extract_callables(tree, content, &queries.callables, &mut helper);
        let callable_map: HashMap<String, sqry_core::graph::unified::NodeId> = callables
            .iter()
            .map(|callable| (callable.name.clone(), callable.node_id))
            .collect();

        // Extract OOP edges (Inherits, Implements)
        extract_oop_edges(tree, content, &callables, &mut helper);

        // Create export edges for publicly accessible callables only.
        // A method is only exported if it is public/global AND its enclosing class
        // (if any) is also public/global. This prevents exporting public methods
        // inside private or default-visibility classes.
        for callable in &callables {
            let is_public_or_global = matches!(
                callable.visibility.as_deref(),
                Some("public") | Some("global")
            );
            let should_export = callable.is_trigger || is_public_or_global;
            if !should_export {
                continue;
            }
            // Check effective visibility: if this callable is inside a class,
            // the enclosing class must also be public/global
            let enclosing_class_exported =
                find_enclosing_class(callable, &callables).is_none_or(|parent| {
                    matches!(
                        parent.visibility.as_deref(),
                        Some("public") | Some("global")
                    )
                });
            if enclosing_class_exported {
                helper.add_export_edge(module_id, callable.node_id);
            }
        }

        // Build a map of variable declarations to their types for sObject resolution
        let type_map = build_type_map(tree, content);

        // Extract trigger events and emit TriggeredBy edges
        let trigger_events = extract_trigger_events(tree, content, &queries.trigger_events);
        emit_trigger_edges(&trigger_events, &callable_map, &mut helper);

        // Extract SOQL queries -> TableRead edges
        let table_reads = extract_soql_queries(tree, content, &queries.soql_queries, &mut helper);

        // Extract DML operations -> TableWrite edges
        let table_writes = extract_dml_operations(
            tree,
            content,
            &queries.dml_operations,
            &type_map,
            &mut helper,
        );

        // Extract Database class method calls -> TableRead/TableWrite edges
        let (db_reads, db_writes) =
            extract_database_method_calls(tree, content, &queries.database_calls, &mut helper);

        // Extract annotations -> annotation nodes + call edges
        extract_annotation_edges(tree, content, &queries.annotations, &callables, &mut helper);

        // Extract method invocations and constructor calls -> Call edges
        extract_method_invocations(
            tree,
            content,
            &queries.method_invocations,
            &queries.constructor_calls,
            &callables,
            &mut helper,
        );

        // Extract TypeOf and References edges from type annotations
        extract_typeof_and_reference_edges(tree, content, &callables, &mut helper);

        // Synthesize edges from callables to table operations based on lexical containment
        for op in table_reads.into_iter().chain(db_reads) {
            if let Some(caller) = find_enclosing_callable(&callables, op.span_bytes) {
                helper.add_table_read_edge_with_span(
                    caller.node_id,
                    op.table_node_id,
                    &op.table_name,
                    None, // Apex doesn't use schema qualifiers
                    vec![op.span],
                );
            }
        }

        for op in table_writes.into_iter().chain(db_writes) {
            if let Some(caller) = find_enclosing_callable(&callables, op.span_bytes) {
                helper.add_table_write_edge_with_span(
                    caller.node_id,
                    op.table_node_id,
                    &op.table_name,
                    None, // Apex doesn't use schema qualifiers
                    op.operation,
                    vec![op.span],
                );
            }
        }

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Apex
    }
}

// =============================================================================
// Internal Types
// =============================================================================

/// Represents a callable entity (class, method, trigger) that can contain table operations
#[derive(Debug, Clone)]
struct ApexCallable {
    node_id: sqry_core::graph::unified::NodeId,
    start_byte: usize,
    end_byte: usize,
    name: String,
    /// Visibility modifier: "public", "global", "private", "protected", or None (default private)
    visibility: Option<String>,
    /// Whether this is a trigger (triggers are always externally accessible)
    is_trigger: bool,
}

/// Represents a table read operation
#[derive(Debug, Clone)]
struct TableReadOp {
    span_bytes: (usize, usize),
    table_name: String,
    table_node_id: sqry_core::graph::unified::NodeId,
    span: Span,
}

/// Represents a table write operation
#[derive(Debug, Clone)]
struct TableWriteOp_ {
    span_bytes: (usize, usize),
    table_name: String,
    operation: TableWriteOp,
    table_node_id: sqry_core::graph::unified::NodeId,
    span: Span,
}

// =============================================================================
// Tree-sitter Queries
// =============================================================================

/// Tree-sitter queries for Apex relationship extraction
struct ApexQueries {
    callables: Query,
    trigger_events: Query,
    soql_queries: Query,
    dml_operations: Query,
    database_calls: Query,
    annotations: Query,
    method_invocations: Query,
    constructor_calls: Query,
}

impl ApexQueries {
    fn new(language: &tree_sitter::Language) -> GraphResult<Self> {
        // Query for callable definitions (classes, methods, triggers)
        let callables = Query::new(
            language,
            r"
            [
              (class_declaration
                (identifier) @callable.name) @callable

              (interface_declaration
                (identifier) @callable.name) @callable

              (method_declaration
                (identifier) @callable.name) @callable

              (trigger_declaration
                (identifier) @callable.name) @callable
            ]
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile callables query: {e}"),
        })?;

        // Query for trigger events
        let trigger_events = Query::new(
            language,
            r"
            (trigger_declaration
              (identifier) @trigger.name
              (identifier) @trigger.sobject
              (trigger_event) @trigger.event) @trigger.decl
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile trigger events query: {e}"),
        })?;

        // Query for SOQL queries
        // AST: (query_expression (soql_query_body (from_clause (storage_identifier (identifier)))))
        let soql_queries = Query::new(
            language,
            r"
            (query_expression
              (soql_query_body
                (from_clause
                  (storage_identifier
                    (identifier) @sobject.name)))) @soql.query
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile SOQL query: {e}"),
        })?;

        // Query for DML operations
        // AST: (dml_expression (dml_type) ...)
        let dml_operations = Query::new(
            language,
            r"
            (dml_expression
              (dml_type) @dml.type) @dml.expr
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile DML query: {e}"),
        })?;

        // Query for Database class method calls
        // Matches: Database.query(), Database.insert(), Database.update(), etc.
        let database_calls = Query::new(
            language,
            r"
            (method_invocation
              (identifier) @class.name
              (identifier) @method.name
              (argument_list) @args) @db.call
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile Database calls query: {e}"),
        })?;

        // Query for method annotations
        let annotations = Query::new(
            language,
            r"
            (method_declaration
              (modifiers
                (annotation
                  (identifier) @annotation.name))) @annotation.method
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile annotation query: {e}"),
        })?;

        // Query for method invocations
        let method_invocations = Query::new(
            language,
            r"
            (method_invocation
              name: (identifier) @callee.name) @call
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile method invocation query: {e}"),
        })?;

        // Query for constructor calls (object_creation_expression)
        let constructor_calls = Query::new(
            language,
            r"
            (object_creation_expression) @constructor
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile constructor call query: {e}"),
        })?;

        Ok(Self {
            callables,
            trigger_events,
            soql_queries,
            dml_operations,
            database_calls,
            annotations,
            method_invocations,
            constructor_calls,
        })
    }
}

// =============================================================================
// Extraction Functions
// =============================================================================

/// Extract visibility modifier from a declaration node's `modifiers` child.
///
/// In tree-sitter-sfapex, class/method declarations may have a `modifiers` child
/// containing keywords like `public`, `private`, `protected`, or `global`.
fn extract_visibility(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.children(&mut mod_cursor) {
                if let Ok(text) = modifier.utf8_text(content) {
                    let lower = text.trim().to_lowercase();
                    if matches!(
                        lower.as_str(),
                        "public" | "private" | "protected" | "global"
                    ) {
                        return Some(lower);
                    }
                }
            }
        }
    }
    None
}

/// Find the enclosing class for a callable by byte range containment.
///
/// Returns `None` if the callable is a top-level declaration (not inside any class).
/// Returns `Some(&class)` if the callable's byte range is strictly contained within a class.
fn find_enclosing_class<'a>(
    callable: &ApexCallable,
    callables: &'a [ApexCallable],
) -> Option<&'a ApexCallable> {
    // Find the innermost (smallest) containing class by selecting the container
    // with the smallest byte range that still strictly contains the callable.
    callables
        .iter()
        .filter(|c| {
            // Must be a class (not the callable itself), and must strictly contain it
            c.node_id != callable.node_id
                && c.start_byte <= callable.start_byte
                && c.end_byte >= callable.end_byte
                && (c.start_byte != callable.start_byte || c.end_byte != callable.end_byte)
        })
        .min_by_key(|c| c.end_byte - c.start_byte)
}

/// Extract module name from file path.
/// For Apex files, use the file stem (filename without extension) as the module name.
fn extract_module_name_from_path(file: &Path) -> String {
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string()
}

/// Extract callable definitions (classes, methods, triggers) from the AST
fn extract_callables(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<ApexCallable> {
    let mut callables = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut callable_name = None;
        let mut callable_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "callable.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        callable_name = Some(text.to_string());
                    }
                }
                "callable" => {
                    callable_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(name), Some(node)) = (callable_name, callable_node) {
            let span = span_from_node(&node);
            let is_trigger = node.kind() == "trigger_declaration";
            let visibility = extract_visibility(node, content);
            let node_id = match node.kind() {
                "class_declaration" => helper.add_class(&name, Some(span)),
                "interface_declaration" => helper.add_interface(&name, Some(span)),
                "method_declaration" => helper.add_method(&name, Some(span), false, false),
                _ => helper.add_function(&name, Some(span), false, false),
            };
            callables.push(ApexCallable {
                node_id,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                name,
                visibility,
                is_trigger,
            });
        }
    }

    callables
}

/// Extract OOP edges (Inherits, Implements) from class and interface declarations.
///
/// Walks the AST to find:
/// - `class_declaration` with `superclass` field → Inherits edge
/// - `class_declaration` with `interfaces` field → Implements edges
/// - `interface_declaration` with `extends_interfaces` child → Inherits edges
fn extract_oop_edges(
    tree: &Tree,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    walk_for_oop_edges(tree.root_node(), content, callables, helper);
}

/// Recursively walk AST nodes to extract OOP edges.
fn walk_for_oop_edges(
    node: Node,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    match node.kind() {
        "class_declaration" => {
            process_class_oop_edges(node, content, callables, helper);
        }
        "interface_declaration" => {
            process_interface_extends(node, content, callables, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_oop_edges(child, content, callables, helper);
    }
}

/// Process OOP edges for a class declaration.
///
/// Extracts:
/// - `superclass` field → Inherits edge (class extends class)
/// - `interfaces` field → Implements edges (class implements interface(s))
fn process_class_oop_edges(
    class_node: Node,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    // Find the class callable by byte range match
    let class_callable = callables
        .iter()
        .find(|c| c.start_byte == class_node.start_byte() && c.end_byte == class_node.end_byte());
    let Some(class_callable) = class_callable else {
        return;
    };
    let class_id = class_callable.node_id;

    // Process extends (superclass)
    if let Some(superclass_node) = class_node.child_by_field_name("superclass") {
        let parent_name = extract_type_name_from_node(superclass_node, content);
        if !parent_name.is_empty() {
            let parent_id = helper.add_class(&parent_name, None);
            helper.add_inherits_edge(class_id, parent_id);
        }
    }

    // Process implements (interfaces)
    if let Some(interfaces_node) = class_node.child_by_field_name("interfaces") {
        extract_implemented_interfaces(interfaces_node, content, class_id, helper);
    }
}

/// Process extends edges for an interface declaration.
///
/// `extends_interfaces` is a child node kind (NOT a named field) in tree-sitter-sfapex.
/// We must iterate children to find it by kind.
fn process_interface_extends(
    interface_node: Node,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    // Find the interface callable by byte range match
    let interface_callable = callables.iter().find(|c| {
        c.start_byte == interface_node.start_byte() && c.end_byte == interface_node.end_byte()
    });
    let Some(interface_callable) = interface_callable else {
        return;
    };
    let interface_id = interface_callable.node_id;

    // Walk children to find extends_interfaces node by kind
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        if child.kind() == "extends_interfaces" {
            extract_parent_interfaces(child, content, interface_id, helper);
            return;
        }
    }
}

/// Extract interface types from a `super_interfaces` or `interfaces` (`type_list`) node
/// and create `Implements` edges.
fn extract_implemented_interfaces(
    interfaces_node: Node,
    content: &[u8],
    implementor_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = interfaces_node.walk();
    for child in interfaces_node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    let name = text.trim();
                    if !name.is_empty() {
                        let iface_id = helper.add_interface(name, None);
                        helper.add_implements_edge(implementor_id, iface_id);
                    }
                }
            }
            "type_list" => {
                let mut type_cursor = child.walk();
                for type_child in child.children(&mut type_cursor) {
                    if let Some(name) = extract_type_identifier_text(type_child, content) {
                        let iface_id = helper.add_interface(&name, None);
                        helper.add_implements_edge(implementor_id, iface_id);
                    }
                }
            }
            "generic_type" | "scoped_type_identifier" => {
                if let Some(name) = extract_type_identifier_text(child, content) {
                    let iface_id = helper.add_interface(&name, None);
                    helper.add_implements_edge(implementor_id, iface_id);
                }
            }
            _ => {}
        }
    }
}

/// Extract parent interfaces from an `extends_interfaces` node and create `Inherits` edges.
fn extract_parent_interfaces(
    extends_node: Node,
    content: &[u8],
    child_interface_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = extends_node.walk();
    for child in extends_node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" => {
                if let Ok(text) = child.utf8_text(content) {
                    let name = text.trim();
                    if !name.is_empty() {
                        let parent_id = helper.add_interface(name, None);
                        helper.add_inherits_edge(child_interface_id, parent_id);
                    }
                }
            }
            "type_list" => {
                let mut type_cursor = child.walk();
                for type_child in child.children(&mut type_cursor) {
                    if let Some(name) = extract_type_identifier_text(type_child, content) {
                        let parent_id = helper.add_interface(&name, None);
                        helper.add_inherits_edge(child_interface_id, parent_id);
                    }
                }
            }
            "generic_type" | "scoped_type_identifier" => {
                if let Some(name) = extract_type_identifier_text(child, content) {
                    let parent_id = helper.add_interface(&name, None);
                    helper.add_inherits_edge(child_interface_id, parent_id);
                }
            }
            _ => {}
        }
    }
}

/// Extract the type name from a superclass node.
///
/// Handles both simple `type_identifier` and `generic_type` patterns.
fn extract_type_name_from_node(node: Node, content: &[u8]) -> String {
    // Check for type_identifier directly
    if node.kind() == "type_identifier" {
        return node
            .utf8_text(content)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    }

    // Handle scoped_type_identifier (e.g., Outer.Base) — use full text
    if node.kind() == "scoped_type_identifier" {
        return node
            .utf8_text(content)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    }

    // Walk children for type nodes (type_identifier, scoped_type_identifier, generic_type)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" => {
                return child
                    .utf8_text(content)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
            }
            "scoped_type_identifier" => {
                return child
                    .utf8_text(content)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
            }
            "generic_type" => {
                // For generic types, recurse to extract the base type
                return extract_type_name_from_node(child, content);
            }
            _ => {}
        }
    }

    // Fallback: use node text directly, stripping any leading keyword (extends/implements)
    let text = node
        .utf8_text(content)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // Strip leading "extends " or "implements " if present
    text.strip_prefix("extends ")
        .or_else(|| text.strip_prefix("implements "))
        .unwrap_or(&text)
        .to_string()
}

/// Extract the base type name from a type node, handling `type_identifier` and `generic_type`.
fn extract_type_identifier_text(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(content).ok().map(|s| s.trim().to_string()),
        "scoped_type_identifier" => {
            // Use full text for scoped types (e.g., Outer.Payable)
            node.utf8_text(content).ok().map(|s| s.trim().to_string())
        }
        "generic_type" => {
            // Get first type_identifier or scoped_type_identifier child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "type_identifier" => {
                        return child.utf8_text(content).ok().map(|s| s.trim().to_string());
                    }
                    "scoped_type_identifier" => {
                        return child.utf8_text(content).ok().map(|s| s.trim().to_string());
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

/// Build a map of variable names to their sObject types
fn build_type_map(tree: &Tree, content: &[u8]) -> HashMap<String, String> {
    let mut type_map = HashMap::new();

    // Walk the AST to find variable declarations with types
    walk_for_type_declarations(tree.root_node(), content, &mut type_map);

    type_map
}

#[derive(Debug, Clone)]
struct TriggerEventOp {
    trigger_name: String,
    sobject_name: String,
    events: Vec<String>,
    span: Span,
}

fn extract_trigger_events(tree: &Tree, content: &[u8], query: &Query) -> Vec<TriggerEventOp> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut trigger_name = None;
        let mut sobject_name = None;
        let mut events: Vec<String> = Vec::new();
        let mut trigger_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "trigger.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        trigger_name = Some(text.trim().to_string());
                    }
                }
                "trigger.sobject" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        sobject_name = Some(text.trim().to_string());
                    }
                }
                "trigger.event" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        events.push(text.trim().to_string());
                    }
                }
                "trigger.decl" => {
                    trigger_node = Some(capture.node);
                }
                _ => {}
            }
        }

        let (Some(trigger_name), Some(sobject_name), Some(node)) =
            (trigger_name, sobject_name, trigger_node)
        else {
            continue;
        };

        let span = span_from_node(&node);
        ops.push(TriggerEventOp {
            trigger_name,
            sobject_name,
            events,
            span,
        });
    }

    ops
}

fn emit_trigger_edges(
    trigger_events: &[TriggerEventOp],
    callable_map: &HashMap<String, sqry_core::graph::unified::NodeId>,
    helper: &mut GraphBuildHelper,
) {
    for trigger in trigger_events {
        let trigger_id = callable_map
            .get(&trigger.trigger_name)
            .copied()
            .unwrap_or_else(|| {
                helper.add_function(&trigger.trigger_name, Some(trigger.span), false, false)
            });
        let table_id = helper.add_variable(&trigger.sobject_name, Some(trigger.span));

        if trigger.events.is_empty() {
            helper.add_triggered_by_edge_with_span(
                trigger_id,
                table_id,
                &trigger.trigger_name,
                None,
                vec![trigger.span],
            );
            continue;
        }

        for event in &trigger.events {
            let label = format!("{}:{}", trigger.trigger_name, event);
            helper.add_triggered_by_edge_with_span(
                trigger_id,
                table_id,
                &label,
                None,
                vec![trigger.span],
            );
        }
    }
}

/// Recursively walk AST to extract variable type declarations
fn walk_for_type_declarations(node: Node, content: &[u8], type_map: &mut HashMap<String, String>) {
    // Look for local_variable_declaration or field_declaration
    // Pattern: type_identifier variable_declarator
    if node.kind() == "local_variable_declaration" || node.kind() == "field_declaration" {
        let mut type_name: Option<String> = None;
        let mut var_name: Option<String> = None;

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "type_identifier" | "generic_type" => {
                    if let Ok(text) = child.utf8_text(content) {
                        type_name = Some(extract_sobject_from_type(text.trim()));
                    }
                }
                "variable_declarator" => {
                    // Look for the identifier inside
                    let mut inner_cursor = child.walk();
                    for inner in child.children(&mut inner_cursor) {
                        if inner.kind() == "identifier" {
                            if let Ok(text) = inner.utf8_text(content) {
                                var_name = Some(text.trim().to_string());
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        }

        if let (Some(type_n), Some(var_n)) = (type_name, var_name) {
            // Only track if it looks like an sObject type
            if is_sobject_type(&type_n) {
                type_map.insert(var_n, type_n);
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_type_declarations(child, content, type_map);
    }
}

/// Extract SOQL queries and create `TableRead` operations
fn extract_soql_queries(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<TableReadOp> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut sobject_name = None;
        let mut query_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "sobject.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        sobject_name = Some(text.trim().to_string());
                    }
                }
                "soql.query" => {
                    query_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(sobject), Some(node)) = (sobject_name, query_node) {
            let span = span_from_node(&node);
            let table_node_id = helper.add_variable(&sobject, Some(span));
            ops.push(TableReadOp {
                span_bytes: (node.start_byte(), node.end_byte()),
                table_name: sobject,
                table_node_id,
                span,
            });
        }
    }

    ops
}

/// Extract DML operations and create `TableWrite` operations
fn extract_dml_operations(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    type_map: &HashMap<String, String>,
    helper: &mut GraphBuildHelper,
) -> Vec<TableWriteOp_> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut dml_type: Option<String> = None;
        let mut dml_node = None;
        let mut dml_expr_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "dml.type" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        dml_type = Some(text.trim().to_lowercase());
                    }
                    dml_node = Some(capture.node);
                }
                "dml.expr" => {
                    dml_expr_node = Some(capture.node);
                }
                _ => {}
            }
        }

        let Some(dml_op_str) = dml_type else {
            continue;
        };
        let Some(expr_node) = dml_expr_node else {
            continue;
        };

        // Determine the operation type
        let operation = match dml_op_str.as_str() {
            "insert" | "undelete" => TableWriteOp::Insert,
            "update" | "upsert" => TableWriteOp::Update,
            "delete" => TableWriteOp::Delete,
            _ => continue,
        };

        // Try to resolve the sObject type from the DML expression target
        let sobject_name = resolve_dml_target_type(expr_node, content, type_map, dml_node);

        let span = span_from_node(&expr_node);
        let table_node_id = helper.add_variable(&sobject_name, Some(span));
        ops.push(TableWriteOp_ {
            span_bytes: (expr_node.start_byte(), expr_node.end_byte()),
            table_name: sobject_name,
            operation,
            table_node_id,
            span,
        });
    }

    ops
}

fn extract_annotation_edges(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut annotation_names: Vec<(String, Span)> = Vec::new();
        let mut method_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "annotation.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        annotation_names
                            .push((text.trim().to_string(), span_from_node(&capture.node)));
                    }
                }
                "annotation.method" => {
                    method_node = Some(capture.node);
                }
                _ => {}
            }
        }

        let Some(method_node) = method_node else {
            continue;
        };

        let span_bytes = (method_node.start_byte(), method_node.end_byte());
        let Some(callable) = find_enclosing_callable(callables, span_bytes) else {
            continue;
        };

        for (annotation, span) in annotation_names {
            let annotation_name = format!("annotation::{annotation}");
            let annotation_id = helper.add_node(&annotation_name, Some(span), NodeKind::Other);
            helper.add_call_edge_full_with_span(
                callable.node_id,
                annotation_id,
                0,
                false,
                vec![span],
            );
        }
    }
}

/// Extract method invocations and constructor calls, creating Call edges.
///
/// For method invocations, the callee name is qualified based on the call pattern:
/// - `this.foo()` → `ClassName.foo` (resolve enclosing class)
/// - `super.foo()` → `super.foo` (fallback; superclass resolution not available at this stage)
/// - `obj.foo()` → `obj.foo` (object expression as prefix)
/// - `foo()` (unqualified) → `ClassName.foo` (resolve enclosing class)
///
/// For constructor calls (`new Type(...)`), the callee name is `Type.<init>`.
///
/// Deduplicates by `(caller_id, callee_name, start_byte)`.
fn extract_method_invocations(
    tree: &Tree,
    content: &[u8],
    method_query: &Query,
    constructor_query: &Query,
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    let mut emitted: HashSet<(sqry_core::graph::unified::NodeId, String, usize)> = HashSet::new();

    // Process method invocations
    let mut cursor = QueryCursor::new();
    let capture_names = method_query.capture_names();
    let mut matches = cursor.matches(method_query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut method_name = None;
        let mut call_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "callee.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        method_name = Some(text.trim().to_string());
                    }
                }
                "call" => {
                    call_node = Some(capture.node);
                }
                _ => {}
            }
        }

        let (Some(name), Some(node)) = (method_name, call_node) else {
            continue;
        };

        let span_bytes = (node.start_byte(), node.end_byte());
        let Some(caller) = find_enclosing_callable(callables, span_bytes) else {
            continue;
        };

        // Qualify the callee name based on the call pattern
        let callee_name = qualify_method_call(&name, node, content, callables, caller);

        // Dedup by (caller_id, callee_name, start_byte)
        let dedup_key = (caller.node_id, callee_name.clone(), node.start_byte());
        if !emitted.insert(dedup_key) {
            continue;
        }

        let call_span = span_from_node(&node);
        let callee_id = helper.add_method(&callee_name, Some(call_span), false, false);
        let arg_count = count_arguments(node);
        let arg_count = u8::try_from(arg_count).unwrap_or(u8::MAX);
        helper.add_call_edge_full_with_span(
            caller.node_id,
            callee_id,
            arg_count,
            false,
            vec![call_span],
        );
    }

    // Process constructor calls (object_creation_expression)
    let mut cursor = QueryCursor::new();
    let capture_names = constructor_query.capture_names();
    let mut matches = cursor.matches(constructor_query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut ctor_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            if name == "constructor" {
                ctor_node = Some(capture.node);
            }
        }

        let Some(node) = ctor_node else {
            continue;
        };

        let span_bytes = (node.start_byte(), node.end_byte());
        let Some(caller) = find_enclosing_callable(callables, span_bytes) else {
            continue;
        };

        // Extract type name from the "type" field
        let Some(type_node) = node.child_by_field_name("type") else {
            continue;
        };
        let type_name = extract_constructor_type_name(type_node, content);
        if type_name.is_empty() {
            continue;
        }

        let callee_name = format!("{type_name}.<init>");

        // Dedup by (caller_id, callee_name, start_byte)
        let dedup_key = (caller.node_id, callee_name.clone(), node.start_byte());
        if !emitted.insert(dedup_key) {
            continue;
        }

        let call_span = span_from_node(&node);
        let callee_id = helper.add_method(&callee_name, Some(call_span), false, false);
        let arg_count = count_arguments(node);
        let arg_count = u8::try_from(arg_count).unwrap_or(u8::MAX);
        helper.add_call_edge_full_with_span(
            caller.node_id,
            callee_id,
            arg_count,
            false,
            vec![call_span],
        );
    }
}

/// Qualify a method call name based on the call pattern.
///
/// Returns the fully qualified callee name.
fn qualify_method_call(
    method_name: &str,
    call_node: Node,
    content: &[u8],
    callables: &[ApexCallable],
    caller: &ApexCallable,
) -> String {
    // Check if the method invocation has an object field
    let object_node = call_node.child_by_field_name("object");

    match object_node {
        Some(obj) => {
            match obj.kind() {
                "this" => {
                    // this.foo() → ClassName.foo (enclosing class)
                    let class_name = find_enclosing_class_name(caller, callables);
                    format!("{class_name}.{method_name}")
                }
                "super" => {
                    // super.foo() → super.foo (fallback; superclass not resolvable at this stage)
                    format!("super.{method_name}")
                }
                _ => {
                    // obj.foo() → obj.foo (use object text as qualifier)
                    let qualifier = obj
                        .utf8_text(content)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if qualifier.is_empty() {
                        method_name.to_string()
                    } else {
                        format!("{qualifier}.{method_name}")
                    }
                }
            }
        }
        None => {
            // Unqualified call: foo() → ClassName.foo
            let class_name = find_enclosing_class_name(caller, callables);
            format!("{class_name}.{method_name}")
        }
    }
}

/// Find the name of the enclosing class for a callable.
///
/// Returns the class name if the callable is inside a class, or the callable's own name
/// if it IS a class.
fn find_enclosing_class_name(callable: &ApexCallable, callables: &[ApexCallable]) -> String {
    // If the callable itself is a class/interface, return its name
    // (this handles the case where we're in a class-level context)

    // Find the enclosing class by containment
    if let Some(enclosing) = find_enclosing_class(callable, callables) {
        return enclosing.name.clone();
    }

    // If we ARE the class, return our own name
    callable.name.clone()
}

/// Extract the base type name from a constructor's type node.
///
/// Handles:
/// - `type_identifier` → use text directly (e.g., `Account`)
/// - `generic_type` → extract first `type_identifier` child (e.g., `List` from `List<Contact>`)
fn extract_constructor_type_name(type_node: Node, content: &[u8]) -> String {
    match type_node.kind() {
        "type_identifier" => type_node
            .utf8_text(content)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        "generic_type" => {
            // Get first type_identifier child
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    return child
                        .utf8_text(content)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                }
            }
            type_node
                .utf8_text(content)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        }
        _ => type_node
            .utf8_text(content)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}

/// Count the number of arguments in a call node's `argument_list`.
fn count_arguments(call_node: Node) -> usize {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return 0;
    };
    let mut count = 0;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        // Count non-punctuation children (commas, parens are not arguments)
        if child.is_named() {
            count += 1;
        }
    }
    count
}

/// Extract `Database` class method calls such as `Database.query` and `Database.insert`.
#[allow(clippy::too_many_lines)]
fn extract_database_method_calls(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> (Vec<TableReadOp>, Vec<TableWriteOp_>) {
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut class_name: Option<String> = None;
        let mut method_name: Option<String> = None;
        let mut args_node: Option<Node> = None;
        let mut call_node: Option<Node> = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "class.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        class_name = Some(text.trim().to_string());
                    }
                }
                "method.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        method_name = Some(text.trim().to_lowercase());
                    }
                }
                "args" => {
                    args_node = Some(capture.node);
                }
                "db.call" => {
                    call_node = Some(capture.node);
                }
                _ => {}
            }
        }

        // Only process Database class calls
        let Some(class) = class_name else {
            continue;
        };
        if class != "Database" {
            continue;
        }

        let Some(method) = method_name else {
            continue;
        };
        let Some(node) = call_node else {
            continue;
        };

        let span = span_from_node(&node);

        // Extract sObject name from arguments (if available)
        let sobject_name = args_node
            .and_then(|args| extract_sobject_from_database_call_args(args, content, &method))
            .unwrap_or_else(|| "sObject".to_string());

        match method.as_str() {
            "query" | "querylocator" | "getquerylocator" | "countquery" => {
                // Database.query() -> TableRead
                let table_node_id = helper.add_variable(&sobject_name, Some(span));
                reads.push(TableReadOp {
                    span_bytes: (node.start_byte(), node.end_byte()),
                    table_name: sobject_name,
                    table_node_id,
                    span,
                });
            }
            "insert" | "insertasync" | "insertimmediate" => {
                // Database.insert() -> TableWrite (Insert)
                let table_node_id = helper.add_variable(&sobject_name, Some(span));
                writes.push(TableWriteOp_ {
                    span_bytes: (node.start_byte(), node.end_byte()),
                    table_name: sobject_name,
                    operation: TableWriteOp::Insert,
                    table_node_id,
                    span,
                });
            }
            "update" | "updateasync" | "updateimmediate" => {
                // Database.update() -> TableWrite (Update)
                let table_node_id = helper.add_variable(&sobject_name, Some(span));
                writes.push(TableWriteOp_ {
                    span_bytes: (node.start_byte(), node.end_byte()),
                    table_name: sobject_name,
                    operation: TableWriteOp::Update,
                    table_node_id,
                    span,
                });
            }
            "delete" | "deleteasync" | "deleteimmediate" => {
                // Database.delete() -> TableWrite (Delete)
                let table_node_id = helper.add_variable(&sobject_name, Some(span));
                writes.push(TableWriteOp_ {
                    span_bytes: (node.start_byte(), node.end_byte()),
                    table_name: sobject_name,
                    operation: TableWriteOp::Delete,
                    table_node_id,
                    span,
                });
            }
            "upsert" | "upsertasync" => {
                // Database.upsert() -> TableWrite (Update, closest semantic match)
                let table_node_id = helper.add_variable(&sobject_name, Some(span));
                writes.push(TableWriteOp_ {
                    span_bytes: (node.start_byte(), node.end_byte()),
                    table_name: sobject_name,
                    operation: TableWriteOp::Update,
                    table_node_id,
                    span,
                });
            }
            _ => {
                // Other Database methods are ignored
            }
        }
    }

    (reads, writes)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Find the innermost callable that contains the given byte range.
fn find_enclosing_callable(
    callables: &[ApexCallable],
    span_bytes: (usize, usize),
) -> Option<&ApexCallable> {
    let (start_byte, end_byte) = span_bytes;
    callables
        .iter()
        .filter(|c| c.start_byte <= start_byte && end_byte <= c.end_byte)
        .min_by_key(|c| c.end_byte.saturating_sub(c.start_byte))
}

/// Create a `Span` from a tree-sitter node.
fn span_from_node(node: &Node) -> Span {
    Span::new(
        Position::new(node.start_position().row, node.start_position().column),
        Position::new(node.end_position().row, node.end_position().column),
    )
}

/// Extract the sObject type from a type string (handles `List<Account>`, `Account`, etc.)
fn extract_sobject_from_type(type_str: &str) -> String {
    let trimmed = type_str.trim();

    // Handle List<SObjectType> pattern
    if trimmed.starts_with("List<") && trimmed.ends_with('>') {
        return trimmed[5..trimmed.len() - 1].trim().to_string();
    }

    // Handle Set<SObjectType> pattern
    if trimmed.starts_with("Set<") && trimmed.ends_with('>') {
        return trimmed[4..trimmed.len() - 1].trim().to_string();
    }

    // Handle Map<K, SObjectType> - take the value type
    if trimmed.starts_with("Map<") && trimmed.ends_with('>') {
        let inner = &trimmed[4..trimmed.len() - 1];
        if let Some(comma_pos) = inner.rfind(',') {
            return inner[comma_pos + 1..].trim().to_string();
        }
    }

    trimmed.to_string()
}

/// Check if a type name looks like an `sObject` type.
fn is_sobject_type(type_name: &str) -> bool {
    // Standard Salesforce sObjects start with uppercase
    // Custom objects end with __c
    // Common standard objects: Account, Contact, Lead, Opportunity, Case, etc.
    if type_name.is_empty() {
        return false;
    }

    // Exclude primitive types and common non-sObject types
    let non_sobject_types = [
        "String", "Integer", "Long", "Double", "Decimal", "Boolean", "Date", "Datetime", "Time",
        "Id", "Blob", "Object", "void", "Map", "List", "Set",
    ];

    if non_sobject_types.contains(&type_name) {
        return false;
    }

    // Custom objects always end with __c
    if type_name.ends_with("__c") {
        return true;
    }

    // Check if it starts with uppercase (typical sObject convention)
    type_name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// Resolve the sObject type from a DML expression target
fn resolve_dml_target_type(
    dml_expr_node: Node,
    content: &[u8],
    type_map: &HashMap<String, String>,
    _dml_type_node: Option<Node>,
) -> String {
    // Look for the target expression after the DML type
    // DML expression structure: (dml_expression (dml_type) <target>)
    let mut cursor = dml_expr_node.walk();
    let mut found_dml_type = false;

    for child in dml_expr_node.children(&mut cursor) {
        if child.kind() == "dml_type" {
            found_dml_type = true;
            continue;
        }

        if found_dml_type {
            // This is the target expression
            if let Ok(text) = child.utf8_text(content) {
                let text = text.trim();

                // Check if it's a variable in our type map
                if let Some(sobject_type) = type_map.get(text) {
                    return sobject_type.clone();
                }

                // Check if it's a 'new SObjectType(...)' expression
                if let Some(sobject) = extract_sobject_from_new_expression(text) {
                    return sobject;
                }

                // If it looks like an identifier, try to use it as the type
                if text
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                {
                    // Could be a direct variable reference
                    if let Some(sobject_type) = type_map.get(text) {
                        return sobject_type.clone();
                    }
                }
            }
        }
    }

    // Fallback to generic sObject if we can't determine the type
    "sObject".to_string()
}

/// Extract sObject name from 'new `SObjectType`(...)' expression
fn extract_sobject_from_new_expression(expr: &str) -> Option<String> {
    let trimmed = expr.trim();

    // Handle 'new Account(...)' pattern
    if let Some(after_new) = trimmed.strip_prefix("new ").map(str::trim)
        && let Some(paren_pos) = after_new.find('(')
    {
        let type_name = after_new[..paren_pos].trim();
        if is_sobject_type(type_name) {
            return Some(type_name.to_string());
        }
    }

    None
}

/// Extract an `sObject` name from `Database` class method arguments.
fn extract_sobject_from_database_call_args(
    args_node: Node,
    content: &[u8],
    method: &str,
) -> Option<String> {
    // For Database.query(), look for SOQL string
    if (method == "query" || method == "querylocator" || method == "getquerylocator")
        && let Ok(text) = args_node.utf8_text(content)
        && let Some(sobject) = extract_sobject_from_soql_string(text)
    {
        // Try to extract sObject from SOQL string: 'SELECT ... FROM Account ...'
        return Some(sobject);
    }

    // For DML methods, the first argument is typically the sObject or List<sObject>
    // We'd need type inference to resolve this properly
    None
}

/// Extract an `sObject` name from a SOQL query string.
fn extract_sobject_from_soql_string(soql: &str) -> Option<String> {
    // Simple regex-free extraction: find "FROM <sObject>"
    let upper = soql.to_uppercase();
    if let Some(from_pos) = upper.find("FROM ") {
        let after_from = &soql[from_pos + 5..];
        // Skip whitespace and extract the sObject name
        let trimmed = after_from.trim_start();
        // Extract until space, WHERE, LIMIT, ORDER, etc.
        let end_pos = trimmed
            .find(|c: char| c.is_whitespace() || c == ',' || c == ']' || c == '\'')
            .unwrap_or(trimmed.len());
        let sobject = &trimmed[..end_pos];
        if !sobject.is_empty() && is_sobject_type(sobject) {
            return Some(sobject.to_string());
        }
    }

    None
}

// =============================================================================
// TypeOf and References Edge Extraction
// =============================================================================

/// Extract `TypeOf` and `References` edges from type annotations in the AST.
///
/// Walks the AST to find:
/// - `field_declaration` -> `TypeOf(Field)` + `References`
/// - `local_variable_declaration` -> `TypeOf(Variable)` + `References`
/// - `formal_parameter` -> `TypeOf(Parameter, param_index)` + `References`
/// - `method_declaration` return type -> `TypeOf(Return)` + `References` (skip `void`)
fn extract_typeof_and_reference_edges(
    tree: &Tree,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    walk_for_type_edges(tree.root_node(), content, callables, helper);
}

fn walk_for_type_edges(
    node: Node<'_>,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    match node.kind() {
        "field_declaration" => {
            extract_field_type_edges(node, content, helper);
        }
        "local_variable_declaration" => {
            extract_local_variable_type_edges(node, content, callables, helper);
        }
        "method_declaration" | "constructor_declaration" => {
            // Extract parameter type edges
            extract_parameter_type_edges(node, content, callables, helper);
            // Extract return type edges (skip void, skip constructors)
            if node.kind() == "method_declaration" {
                extract_return_type_edges(node, content, callables, helper);
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_type_edges(child, content, callables, helper);
    }
}

fn extract_field_type_edges(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // Find the type node and declarator name
    let mut type_node = None;
    let mut var_name = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "generic_type" | "scoped_type_identifier" => {
                type_node = Some(child);
            }
            "variable_declarator" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    var_name = name_node
                        .utf8_text(content)
                        .ok()
                        .map(|s| s.trim().to_string());
                }
            }
            _ => {}
        }
    }

    let Some(type_n) = type_node else { return };
    let Some(ref field_name) = var_name else {
        return;
    };
    if field_name.is_empty() {
        return;
    }

    // Find enclosing class to create qualified name
    let qualified_name = find_enclosing_class_name_from_node(node, content).map_or_else(
        || field_name.clone(),
        |class_name| format!("{class_name}.{field_name}"),
    );

    let source_id = helper.add_variable(&qualified_name, Some(span_from_node(&node)));

    // TypeOf edge
    if let Some(type_str) = extract_type_string(type_n, content) {
        let target_id = helper.add_type(&type_str, Some(span_from_node(&type_n)));
        helper.add_typeof_edge_with_context(
            source_id,
            target_id,
            Some(TypeOfContext::Field),
            None,
            Some(field_name),
        );
    }

    // References edges (deduped)
    let mut seen = std::collections::HashSet::new();
    for type_name in extract_all_type_names_from_annotation(type_n, content) {
        if seen.insert(type_name.clone()) {
            let ref_target = helper.add_type(&type_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }
    }
}

fn extract_local_variable_type_edges(
    node: Node<'_>,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    let mut type_node = None;
    let mut var_name = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "generic_type" | "scoped_type_identifier" => {
                type_node = Some(child);
            }
            "variable_declarator" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    var_name = name_node
                        .utf8_text(content)
                        .ok()
                        .map(|s| s.trim().to_string());
                }
            }
            _ => {}
        }
    }

    let Some(type_n) = type_node else { return };
    let Some(ref local_name) = var_name else {
        return;
    };
    if local_name.is_empty() {
        return;
    }

    // Find enclosing callable for context
    let qualified_name = find_enclosing_callable_name(node, callables).map_or_else(
        || local_name.clone(),
        |caller| format!("{caller}.{local_name}"),
    );

    let source_id = helper.add_variable(&qualified_name, Some(span_from_node(&node)));

    // TypeOf edge
    if let Some(type_str) = extract_type_string(type_n, content) {
        let target_id = helper.add_type(&type_str, Some(span_from_node(&type_n)));
        helper.add_typeof_edge_with_context(
            source_id,
            target_id,
            Some(TypeOfContext::Variable),
            None,
            Some(local_name),
        );
    }

    // References edges (deduped)
    let mut seen = std::collections::HashSet::new();
    for type_name in extract_all_type_names_from_annotation(type_n, content) {
        if seen.insert(type_name.clone()) {
            let ref_target = helper.add_type(&type_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn extract_parameter_type_edges(
    method_node: Node<'_>,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    let Some(params) = method_node.child_by_field_name("parameters") else {
        return;
    };

    let mut param_index: u16 = 0;
    let mut ref_seen = std::collections::HashSet::new();
    let mut cursor = params.walk();
    for param in params.named_children(&mut cursor) {
        if param.kind() != "formal_parameter" {
            continue;
        }

        let mut type_node = None;
        let mut param_name = None;
        let mut inner_cursor = param.walk();
        for child in param.children(&mut inner_cursor) {
            match child.kind() {
                "type_identifier" | "generic_type" | "scoped_type_identifier" => {
                    type_node = Some(child);
                }
                "identifier" => {
                    // The parameter name is typically the last identifier
                    param_name = child.utf8_text(content).ok().map(|s| s.trim().to_string());
                }
                _ => {}
            }
        }

        let Some(type_n) = type_node else {
            param_index += 1;
            continue;
        };
        let p_name = param_name.unwrap_or_else(|| format!("param{param_index}"));

        // Find the callable's node_id
        let caller_id = find_callable_node_id(method_node, callables);
        let source_id =
            caller_id.unwrap_or_else(|| helper.add_variable(&p_name, Some(span_from_node(&param))));

        // TypeOf edge
        if let Some(type_str) = extract_type_string(type_n, content) {
            let target_id = helper.add_type(&type_str, Some(span_from_node(&type_n)));
            helper.add_typeof_edge_with_context(
                source_id,
                target_id,
                Some(TypeOfContext::Parameter),
                Some(param_index),
                Some(&p_name),
            );
        }

        // References edges (deduped per method)
        for type_name in extract_all_type_names_from_annotation(type_n, content) {
            if ref_seen.insert(type_name.clone()) {
                let ref_target = helper.add_type(&type_name, None);
                helper.add_reference_edge(source_id, ref_target);
            }
        }

        param_index += 1;
    }
}

fn extract_return_type_edges(
    method_node: Node<'_>,
    content: &[u8],
    callables: &[ApexCallable],
    helper: &mut GraphBuildHelper,
) {
    // In Apex, the return type comes before the method name
    // Pattern: modifiers type_identifier method_name(params) { body }
    let mut type_node = None;
    let mut cursor = method_node.walk();
    for child in method_node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "generic_type" | "scoped_type_identifier" | "void_type" => {
                type_node = Some(child);
            }
            _ => {}
        }
    }

    let Some(type_n) = type_node else { return };

    // Skip void return types
    if type_n.kind() == "void_type" {
        return;
    }
    if let Ok(text) = type_n.utf8_text(content)
        && text.trim().eq_ignore_ascii_case("void")
    {
        return;
    }

    let caller_id = find_callable_node_id(method_node, callables);
    let source_id = caller_id.unwrap_or_else(|| {
        let method_name = method_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok())
            .unwrap_or("unknown");
        helper.add_function(
            method_name,
            Some(span_from_node(&method_node)),
            false,
            false,
        )
    });

    // TypeOf edge
    if let Some(type_str) = extract_type_string(type_n, content) {
        let target_id = helper.add_type(&type_str, Some(span_from_node(&type_n)));
        helper.add_typeof_edge_with_context(
            source_id,
            target_id,
            Some(TypeOfContext::Return),
            None,
            None,
        );
    }

    // References edges (deduped)
    let mut seen = std::collections::HashSet::new();
    for type_name in extract_all_type_names_from_annotation(type_n, content) {
        if seen.insert(type_name.clone()) {
            let ref_target = helper.add_type(&type_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }
    }
}

/// Find the enclosing class name for a field declaration by walking AST parents.
fn find_enclosing_class_name_from_node(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(parent.kind(), "class_declaration" | "class_body") {
            if parent.kind() == "class_declaration" {
                return parent
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(content).ok())
                    .map(|s| s.trim().to_string());
            }
            // If class_body, go up one more level
            if let Some(class_decl) = parent.parent()
                && class_decl.kind() == "class_declaration"
            {
                return class_decl
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(content).ok())
                    .map(|s| s.trim().to_string());
            }
        }
        current = parent.parent();
    }
    None
}

/// Find the enclosing callable's name by matching byte ranges.
fn find_enclosing_callable_name(node: Node<'_>, callables: &[ApexCallable]) -> Option<String> {
    let byte_pos = node.start_byte();
    let mut best: Option<&ApexCallable> = None;
    for callable in callables {
        if byte_pos >= callable.start_byte
            && byte_pos <= callable.end_byte
            && best.as_ref().is_none_or(|b| {
                (callable.end_byte - callable.start_byte) < (b.end_byte - b.start_byte)
            })
        {
            best = Some(callable);
        }
    }
    best.map(|c| c.name.clone())
}

/// Find the callable's `NodeId` for a method declaration node.
fn find_callable_node_id(
    method_node: Node<'_>,
    callables: &[ApexCallable],
) -> Option<sqry_core::graph::unified::NodeId> {
    let start = method_node.start_byte();
    let end = method_node.end_byte();
    callables
        .iter()
        .find(|c| c.start_byte == start && c.end_byte == end)
        .or_else(|| {
            // Fallback: find innermost callable containing this node
            callables
                .iter()
                .filter(|c| c.start_byte <= start && end <= c.end_byte)
                .min_by_key(|c| c.end_byte.saturating_sub(c.start_byte))
        })
        .map(|c| c.node_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_call_edge, build_node_name_lookup,
    };
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::id::NodeId;

    fn parse_apex(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sfapex::apex::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    fn build_apex_display_name_lookup(staging: &StagingGraph) -> HashMap<NodeId, String> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddNode { entry, expected_id } = op {
                    let node_id = (*expected_id)?;
                    let display_name = staging.resolve_node_display_name(Language::Apex, entry)?;
                    Some((node_id, display_name.to_string()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn assert_has_display_call_edge(staging: &StagingGraph, caller: &str, callee: &str) {
        let node_names = build_apex_display_name_lookup(staging);

        let found = staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::Calls { .. },
                ..
            } = op
            {
                let source_name = node_names.get(source);
                let target_name = node_names.get(target);
                source_name.is_some_and(|name| name == caller)
                    && target_name.is_some_and(|name| name == callee)
            } else {
                false
            }
        });

        if found {
            return;
        }

        let call_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind:
                        EdgeKind::Calls {
                            argument_count,
                            is_async,
                        },
                    ..
                } = op
                {
                    let source_name = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let target_name = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!(
                        "  {source_name} -> {target_name} (args={argument_count}, async={is_async})"
                    ))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected Apex display call edge from '{caller}' to '{callee}' not found.\nStaged call edges:\n{}",
            if call_edges.is_empty() {
                "  (none)".to_string()
            } else {
                call_edges.join("\n")
            }
        );
    }

    #[test]
    fn test_graph_builder_language() {
        let builder = ApexGraphBuilder::new();
        assert_eq!(GraphBuilder::language(&builder), Language::Apex);
    }

    #[test]
    fn test_build_graph_empty() {
        let source = "";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Empty.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle empty input");
    }

    // =========================================================================
    // SOQL Query Tests -> TableRead
    // =========================================================================

    #[test]
    fn test_soql_simple_query() {
        let source = r"
public class AccountController {
    public void queryAccounts() {
        List<Account> accounts = [SELECT Id, Name FROM Account];
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("AccountController.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for SOQL query");

        // Verify edges were created
        let stats = staging.stats();
        assert!(
            stats.edges_staged > 0,
            "Should have created edges for SOQL query"
        );
    }

    #[test]
    fn test_soql_with_where_clause() {
        let source = r"
public class ContactService {
    public void findActiveContacts() {
        List<Contact> contacts = [SELECT Id FROM Contact WHERE Active__c = true];
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ContactService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for SOQL with WHERE");
    }

    #[test]
    fn test_soql_custom_object() {
        let source = r"
public class CustomController {
    public void getCustomRecords() {
        List<Custom_Object__c> records = [SELECT Id FROM Custom_Object__c];
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("CustomController.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for custom object SOQL");
    }

    // =========================================================================
    // DML Operation Tests -> TableWrite
    // =========================================================================

    #[test]
    fn test_dml_insert() {
        let source = r"
public class AccountService {
    public void createAccount() {
        Account acc = new Account(Name = 'Test');
        insert acc;
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("AccountService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for INSERT DML");
    }

    #[test]
    fn test_dml_update() {
        let source = r"
public class ContactService {
    public void updateContacts() {
        List<Contact> contacts = [SELECT Id FROM Contact];
        update contacts;
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ContactService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for UPDATE DML");
    }

    #[test]
    fn test_dml_delete() {
        let source = r"
public class LeadService {
    public void deleteOldLeads() {
        List<Lead> leads = [SELECT Id FROM Lead WHERE CreatedDate < LAST_N_DAYS:90];
        delete leads;
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("LeadService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for DELETE DML");
    }

    #[test]
    fn test_dml_upsert() {
        let source = r"
public class OpportunityService {
    public void upsertOpportunities() {
        List<Opportunity> opps = new List<Opportunity>();
        opps.add(new Opportunity(Name = 'Test', StageName = 'Prospecting', CloseDate = Date.today()));
        upsert opps;
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("OpportunityService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for UPSERT DML");
    }

    // =========================================================================
    // Database Class Method Tests
    // =========================================================================

    #[test]
    fn test_database_query() {
        let source = r"
public class DynamicQueryService {
    public void runDynamicQuery() {
        String query = 'SELECT Id FROM Lead';
        List<Lead> leads = Database.query(query);
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("DynamicQueryService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for Database.query()");
    }

    #[test]
    fn test_database_insert() {
        let source = r"
public class BatchInsertService {
    public void batchInsert() {
        List<Account> accounts = new List<Account>();
        accounts.add(new Account(Name = 'Test'));
        Database.insert(accounts, false);
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("BatchInsertService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for Database.insert()");
    }

    #[test]
    fn test_database_update() {
        let source = r"
public class BatchUpdateService {
    public void batchUpdate() {
        List<Contact> contacts = [SELECT Id FROM Contact];
        Database.update(contacts, false);
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("BatchUpdateService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for Database.update()");
    }

    #[test]
    fn test_database_delete() {
        let source = r"
public class BatchDeleteService {
    public void batchDelete() {
        List<Case> cases = [SELECT Id FROM Case WHERE Status = 'Closed'];
        Database.delete(cases, false);
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("BatchDeleteService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for Database.delete()");
    }

    #[test]
    fn test_database_upsert() {
        let source = r"
public class BatchUpsertService {
    public void batchUpsert() {
        List<Account> accounts = new List<Account>();
        Database.upsert(accounts, false);
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("BatchUpsertService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for Database.upsert()");
    }

    // =========================================================================
    // Trigger Tests
    // =========================================================================

    #[test]
    fn test_trigger_with_soql_and_dml() {
        let source = r"
trigger AccountTrigger on Account (before insert, after update) {
    List<Contact> contacts = [SELECT Id FROM Contact WHERE AccountId IN :Trigger.new];
    update contacts;
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("AccountTrigger.trigger");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(
            result.is_ok(),
            "Should build graph for trigger with SOQL and DML"
        );
    }

    // =========================================================================
    // Helper Function Tests
    // =========================================================================

    #[test]
    fn test_extract_sobject_from_type_simple() {
        assert_eq!(extract_sobject_from_type("Account"), "Account");
        assert_eq!(extract_sobject_from_type("Contact"), "Contact");
        assert_eq!(extract_sobject_from_type("Custom__c"), "Custom__c");
    }

    #[test]
    fn test_extract_sobject_from_type_list() {
        assert_eq!(extract_sobject_from_type("List<Account>"), "Account");
        assert_eq!(extract_sobject_from_type("List<Contact>"), "Contact");
        assert_eq!(extract_sobject_from_type("List< Lead >"), "Lead");
    }

    #[test]
    fn test_extract_sobject_from_type_set() {
        assert_eq!(extract_sobject_from_type("Set<Account>"), "Account");
        assert_eq!(extract_sobject_from_type("Set<Id>"), "Id");
    }

    #[test]
    fn test_extract_sobject_from_type_map() {
        assert_eq!(extract_sobject_from_type("Map<Id, Account>"), "Account");
        assert_eq!(extract_sobject_from_type("Map<String, Contact>"), "Contact");
    }

    #[test]
    fn test_is_sobject_type() {
        // Standard sObjects
        assert!(is_sobject_type("Account"));
        assert!(is_sobject_type("Contact"));
        assert!(is_sobject_type("Lead"));
        assert!(is_sobject_type("Opportunity"));

        // Custom objects
        assert!(is_sobject_type("Custom_Object__c"));
        assert!(is_sobject_type("My_Custom__c"));

        // Non-sObject types
        assert!(!is_sobject_type("String"));
        assert!(!is_sobject_type("Integer"));
        assert!(!is_sobject_type("Boolean"));
        assert!(!is_sobject_type("List"));
        assert!(!is_sobject_type("Map"));
        assert!(!is_sobject_type("Set"));
    }

    #[test]
    fn test_extract_sobject_from_soql_string() {
        assert_eq!(
            extract_sobject_from_soql_string("SELECT Id FROM Account"),
            Some("Account".to_string())
        );
        assert_eq!(
            extract_sobject_from_soql_string("SELECT Id, Name FROM Contact WHERE Active = true"),
            Some("Contact".to_string())
        );
        assert_eq!(
            extract_sobject_from_soql_string("'SELECT Id FROM Lead'"),
            Some("Lead".to_string())
        );
        assert_eq!(
            extract_sobject_from_soql_string("SELECT Id FROM Custom_Obj__c LIMIT 10"),
            Some("Custom_Obj__c".to_string())
        );
    }

    #[test]
    fn test_extract_sobject_from_new_expression() {
        assert_eq!(
            extract_sobject_from_new_expression("new Account()"),
            Some("Account".to_string())
        );
        assert_eq!(
            extract_sobject_from_new_expression("new Contact(Name = 'Test')"),
            Some("Contact".to_string())
        );
        assert_eq!(
            extract_sobject_from_new_expression("new Custom_Obj__c()"),
            Some("Custom_Obj__c".to_string())
        );
        assert_eq!(extract_sobject_from_new_expression("new String()"), None);
        assert_eq!(extract_sobject_from_new_expression("new Integer()"), None);
    }

    // =========================================================================
    // Complex/Edge Case Tests
    // =========================================================================

    #[test]
    fn test_multiple_soql_queries() {
        let source = r"
public class MultiQueryService {
    public void multipleQueries() {
        List<Account> accounts = [SELECT Id FROM Account];
        List<Contact> contacts = [SELECT Id FROM Contact];
        List<Lead> leads = [SELECT Id FROM Lead];
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MultiQueryService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(
            result.is_ok(),
            "Should build graph for multiple SOQL queries"
        );
    }

    #[test]
    fn test_mixed_dml_operations() {
        let source = r"
public class MixedDmlService {
    public void mixedOperations() {
        Account acc = new Account(Name = 'Test');
        insert acc;

        acc.Name = 'Updated';
        update acc;

        delete acc;
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MixedDmlService.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(
            result.is_ok(),
            "Should build graph for mixed DML operations"
        );
    }

    #[test]
    fn test_inner_class_soql() {
        let source = r"
public class OuterClass {
    public class InnerClass {
        public void innerQuery() {
            List<Case> cases = [SELECT Id FROM Case];
        }
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("OuterClass.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for inner class SOQL");
    }

    #[test]
    fn test_visibility_filters_exports() {
        use sqry_core::graph::unified::edge::EdgeKind;

        let source = r"
public class PublicService {
    public void publicMethod() {}
    private void privateMethod() {}
    void defaultMethod() {}
}

private class PrivateHelper {
    public void helperMethod() {}
}

global class GlobalApi {
    global void apiMethod() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Services.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok());

        // Count export edges
        let export_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Exports { .. }))
            .collect();

        // Collect exported node names
        let exported_names: Vec<String> = export_edges
            .iter()
            .filter_map(|e| {
                staging
                    .nodes()
                    .find(|n| n.expected_id == Some(e.target))
                    .map(|n| {
                        staging
                            .resolve_local_string(n.entry.name)
                            .unwrap_or("?")
                            .to_string()
                    })
            })
            .collect();

        // Public class + public method + global class + global method = 4 exports
        // Private class, private method, default method should NOT be exported
        assert!(
            exported_names.contains(&"PublicService".to_string()),
            "Public class should be exported, got: {exported_names:?}"
        );
        assert!(
            exported_names.contains(&"GlobalApi".to_string()),
            "Global class should be exported, got: {exported_names:?}"
        );
        assert!(
            !exported_names.contains(&"PrivateHelper".to_string()),
            "Private class should NOT be exported, got: {exported_names:?}"
        );

        // publicMethod should be exported (it has public visibility in a public class)
        assert!(
            exported_names.contains(&"publicMethod".to_string()),
            "Public method in public class should be exported, got: {exported_names:?}"
        );
        // privateMethod should NOT be exported
        assert!(
            !exported_names.contains(&"privateMethod".to_string()),
            "Private method should NOT be exported, got: {exported_names:?}"
        );
        // defaultMethod should NOT be exported (no visibility = private)
        assert!(
            !exported_names.contains(&"defaultMethod".to_string()),
            "Default-visibility method should NOT be exported, got: {exported_names:?}"
        );
        // helperMethod is public but inside a private class — should NOT be exported
        assert!(
            !exported_names.contains(&"helperMethod".to_string()),
            "Public method in private class should NOT be exported, got: {exported_names:?}"
        );
    }

    #[test]
    fn test_malformed_apex() {
        let source = "public class Broken { public void method() {";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Broken.cls");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle malformed Apex gracefully");
    }

    // =========================================================================
    // OOP Edge Tests (Inherits, Implements)
    // =========================================================================

    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_implements_edge, assert_has_inherits_edge, assert_has_node_with_kind,
    };

    #[test]
    fn test_class_inheritance() {
        let source = r"
public class Invoice {
    public void process() {}
}
public class PremiumInvoice extends Invoice {
    public void process() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Inheritance.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_inherits_edge(&staging, "PremiumInvoice", "Invoice");
    }

    #[test]
    fn test_interface_implementation() {
        let source = r"
public interface Payable {
    void processPayment(Decimal amount);
}
public interface Comparable {
    Integer compareTo(Object other);
}
public class Invoice implements Payable, Comparable {
    public void processPayment(Decimal amount) {}
    public Integer compareTo(Object other) { return 0; }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Implements.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_implements_edge(&staging, "Invoice", "Payable");
        assert_has_implements_edge(&staging, "Invoice", "Comparable");
    }

    #[test]
    fn test_combined_extends_implements() {
        let source = r"
public interface Payable {
    void processPayment(Decimal amount);
}
public class Invoice {
    public void process() {}
}
public class AdvancedInvoice extends Invoice implements Payable {
    public void processPayment(Decimal amount) {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Combined.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_inherits_edge(&staging, "AdvancedInvoice", "Invoice");
        assert_has_implements_edge(&staging, "AdvancedInvoice", "Payable");
    }

    #[test]
    fn test_interface_extends_interface() {
        let source = r"
public interface Payable {
    void processPayment(Decimal amount);
}
public interface ExtendedPayable extends Payable {
    void refundPayment(Decimal amount);
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("InterfaceExtends.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_inherits_edge(&staging, "ExtendedPayable", "Payable");
    }

    #[test]
    fn test_interface_node_kind() {
        let source = r"
public interface Payable {
    void processPayment(Decimal amount);
}
public interface Comparable {
    Integer compareTo(Object other);
}
public class Invoice implements Payable {}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("InterfaceKind.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_node_with_kind(&staging, "Payable", NodeKind::Interface);
        assert_has_node_with_kind(&staging, "Comparable", NodeKind::Interface);
        assert_has_node_with_kind(&staging, "Invoice", NodeKind::Class);
    }

    // =========================================================================
    // Call Extraction Tests
    // =========================================================================

    #[test]
    fn test_unqualified_local_call() {
        let source = r"
public class MyService {
    public void process() {
        doWork();
    }
    private void doWork() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "MyService::doWork");
        assert_has_display_call_edge(&staging, "process", "MyService.doWork");
    }

    #[test]
    fn test_instance_method_call() {
        let source = r"
public class AccountService {
    public void process() {
        Account acc = new Account();
        String name = acc.getName();
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("AccountService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "acc::getName");
        assert_has_display_call_edge(&staging, "process", "acc.getName");
    }

    #[test]
    fn test_this_method_call() {
        let source = r"
public class MyService {
    public void process() {
        this.doWork();
    }
    private void doWork() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "MyService::doWork");
        assert_has_display_call_edge(&staging, "process", "MyService.doWork");
    }

    #[test]
    fn test_super_method_call() {
        let source = r"
public class BaseService {
    public void process() {}
}
public class MyService extends BaseService {
    public void run() {
        super.process();
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // super.foo() uses "super" prefix as fallback since superclass resolution
        // is not available at graph build time
        assert_has_call_edge(&staging, "run", "super::process");
        assert_has_display_call_edge(&staging, "run", "super.process");
    }

    #[test]
    fn test_static_method_call() {
        let source = r"
public class MyService {
    public void process() {
        AccountUtils.validate(new Account());
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "AccountUtils::validate");
        assert_has_display_call_edge(&staging, "process", "AccountUtils.validate");
    }

    #[test]
    fn test_chained_invocation() {
        let source = r"
public class MyService {
    public void process() {
        obj.foo().bar();
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Chained calls: obj.foo() and then .bar() on the result
        assert_has_call_edge(&staging, "process", "obj::foo");
        assert_has_display_call_edge(&staging, "process", "obj.foo");
    }

    #[test]
    fn test_constructor_call_init() {
        let source = r"
public class MyService {
    public void process() {
        Account acc = new Account();
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "Account::<init>");
        assert_has_display_call_edge(&staging, "process", "Account.<init>");
    }

    #[test]
    fn test_generic_constructor() {
        let source = r"
public class MyService {
    public void process() {
        List<Contact> contacts = new List<Contact>();
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert_has_call_edge(&staging, "process", "List::<init>");
        assert_has_display_call_edge(&staging, "process", "List.<init>");
    }

    #[test]
    fn test_database_calls_alongside_table_edges() {
        let source = r"
public class QueryService {
    public void runQuery() {
        List<Account> accounts = Database.query('SELECT Id FROM Account');
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("QueryService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Database.query should produce BOTH a Call edge AND a TableRead edge
        assert_has_call_edge(&staging, "runQuery", "Database::query");
        assert_has_display_call_edge(&staging, "runQuery", "Database.query");

        // Also verify a table read edge exists
        let has_table_read = staging
            .edges()
            .any(|e| matches!(e.kind, EdgeKind::TableRead { .. }));
        assert!(
            has_table_read,
            "Database.query should also produce a TableRead edge"
        );
    }

    #[test]
    fn test_call_edge_dedup() {
        let source = r"
public class MyService {
    public void process() {
        doWork();
        doWork();
    }
    private void doWork() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("MyService.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Two calls at different locations should produce two Call edges
        let call_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Calls { .. }))
            .collect();

        let node_names = build_node_name_lookup(&staging);
        let display_names = build_apex_display_name_lookup(&staging);
        let dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                node_names.get(&e.source).is_some_and(|n| n == "process")
                    && node_names
                        .get(&e.target)
                        .is_some_and(|n| n == "MyService::doWork")
            })
            .collect();

        assert_eq!(
            dowork_calls.len(),
            2,
            "Two doWork() calls at different locations should produce two edges, got: {}",
            dowork_calls.len()
        );

        let display_dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                display_names.get(&e.source).is_some_and(|n| n == "process")
                    && display_names
                        .get(&e.target)
                        .is_some_and(|n| n == "MyService.doWork")
            })
            .collect();

        assert_eq!(
            display_dowork_calls.len(),
            2,
            "Two Apex display doWork() calls should resolve to MyService.doWork"
        );
    }

    // =========================================================================
    // Codex Review Regression Tests: Nested class + Scoped types
    // =========================================================================

    #[test]
    fn test_nested_class_call_qualification() {
        // Regression test for Codex Finding 1 (HIGH):
        // Inner class methods should be qualified with Inner, not Outer.
        let source = r"
public class Outer {
    public class Inner {
        public void doWork() {}
        public void run() {
            this.doWork();
            doWork();
        }
    }
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("Outer.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let node_names = build_node_name_lookup(&staging);
        let display_names = build_apex_display_name_lookup(&staging);
        let call_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Calls { .. }))
            .collect();

        // Graph identity uses canonical separators.
        let inner_dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                node_names.get(&e.source).is_some_and(|n| n == "run")
                    && node_names
                        .get(&e.target)
                        .is_some_and(|n| n == "Inner::doWork")
            })
            .collect();

        assert_eq!(
            inner_dowork_calls.len(),
            2,
            "Both this.doWork() and doWork() should resolve to Inner::doWork, got targets: {:?}",
            call_edges
                .iter()
                .filter(|e| node_names.get(&e.source).is_some_and(|n| n == "run"))
                .map(|e| node_names.get(&e.target).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
        );

        let display_inner_dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                display_names.get(&e.source).is_some_and(|n| n == "run")
                    && display_names
                        .get(&e.target)
                        .is_some_and(|n| n == "Inner.doWork")
            })
            .collect();

        assert_eq!(
            display_inner_dowork_calls.len(),
            2,
            "Apex display names should resolve both calls to Inner.doWork"
        );

        // Verify no calls to Outer::doWork exist.
        let outer_dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                node_names.get(&e.source).is_some_and(|n| n == "run")
                    && node_names
                        .get(&e.target)
                        .is_some_and(|n| n == "Outer::doWork")
            })
            .collect();

        assert!(
            outer_dowork_calls.is_empty(),
            "No calls should resolve to Outer::doWork"
        );

        let display_outer_dowork_calls: Vec<_> = call_edges
            .iter()
            .filter(|e| {
                display_names.get(&e.source).is_some_and(|n| n == "run")
                    && display_names
                        .get(&e.target)
                        .is_some_and(|n| n == "Outer.doWork")
            })
            .collect();

        assert!(
            display_outer_dowork_calls.is_empty(),
            "No Apex display calls should resolve to Outer.doWork"
        );
    }

    #[test]
    fn test_scoped_superclass_name() {
        // Regression test for Codex Finding 2 (MEDIUM):
        // Scoped superclass names should not include "extends" prefix.
        let source = r"
public class Base {}
public class Child extends Base {}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ScopedTest.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let node_names = build_node_name_lookup(&staging);
        let inherits_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Inherits))
            .collect();

        // Child -> Base should exist with clean name
        let child_inherits: Vec<_> = inherits_edges
            .iter()
            .filter(|e| node_names.get(&e.source).is_some_and(|n| n == "Child"))
            .collect();

        assert_eq!(
            child_inherits.len(),
            1,
            "Child should have one Inherits edge"
        );

        let target_name = node_names
            .get(&child_inherits[0].target)
            .expect("Target should have a name");
        assert_eq!(
            target_name, "Base",
            "Inherits target should be 'Base', not '{target_name}'"
        );
        assert!(
            !target_name.contains("extends"),
            "Inherits target should not contain 'extends' keyword"
        );
    }

    #[test]
    fn test_scoped_interface_type_not_truncated() {
        // Regression test for Codex Finding 3 (MEDIUM):
        // Scoped interface types should use full qualified name, not just first segment.
        // Note: This requires tree-sitter-sfapex to parse Outer.Payable as scoped_type_identifier.
        // If the grammar doesn't support it, this test verifies the simple case at minimum.
        let source = r"
public interface Payable {
    void pay();
}
public class Invoice implements Payable {
    public void pay() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ScopedInterface.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let node_names = build_node_name_lookup(&staging);
        let implements_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Implements))
            .collect();

        let invoice_implements: Vec<_> = implements_edges
            .iter()
            .filter(|e| node_names.get(&e.source).is_some_and(|n| n == "Invoice"))
            .collect();

        assert_eq!(
            invoice_implements.len(),
            1,
            "Invoice should have one Implements edge"
        );

        let target_name = node_names
            .get(&invoice_implements[0].target)
            .expect("Target should have a name");
        assert_eq!(
            target_name, "Payable",
            "Implements target should be 'Payable', not '{target_name}'"
        );
    }

    #[test]
    fn test_scoped_superclass_qualified_name() {
        // Tests scoped superclass: `extends Outer.Base` should resolve to `Outer.Base`,
        // not `extends Outer.Base` or just `Outer`.
        let source = r"
public class Outer {
    public class Base {}
}
public class Child extends Outer.Base {}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ScopedSuperclass.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let node_names = build_node_name_lookup(&staging);
        let display_names = build_apex_display_name_lookup(&staging);
        let inherits_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Inherits))
            .collect();

        let child_inherits: Vec<_> = inherits_edges
            .iter()
            .filter(|e| node_names.get(&e.source).is_some_and(|n| n == "Child"))
            .collect();

        assert_eq!(
            child_inherits.len(),
            1,
            "Child should have one Inherits edge"
        );

        let target_name = node_names
            .get(&child_inherits[0].target)
            .expect("Target should have a name");
        assert!(
            !target_name.contains("extends"),
            "Scoped superclass name should not contain 'extends', got: '{target_name}'"
        );
        assert_eq!(
            target_name, "Outer::Base",
            "Scoped superclass should be 'Outer::Base', got: '{target_name}'"
        );

        let display_target_name = display_names
            .get(&child_inherits[0].target)
            .expect("Display target should have a name");
        assert_eq!(
            display_target_name, "Outer.Base",
            "Apex display superclass should be 'Outer.Base', got: '{display_target_name}'"
        );
    }

    #[test]
    fn test_scoped_interface_qualified_name() {
        // Tests scoped interface: `implements Outer.Payable` should resolve to `Outer.Payable`,
        // not just `Outer`.
        let source = r"
public class Outer {
    public interface Payable {
        void pay();
    }
}
public class Invoice implements Outer.Payable {
    public void pay() {}
}
";
        let tree = parse_apex(source);
        let mut staging = StagingGraph::new();
        let builder = ApexGraphBuilder;
        let file = PathBuf::from("ScopedQualifiedInterface.cls");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let node_names = build_node_name_lookup(&staging);
        let display_names = build_apex_display_name_lookup(&staging);
        let implements_edges: Vec<_> = staging
            .edges()
            .filter(|e| matches!(e.kind, EdgeKind::Implements))
            .collect();

        let invoice_implements: Vec<_> = implements_edges
            .iter()
            .filter(|e| node_names.get(&e.source).is_some_and(|n| n == "Invoice"))
            .collect();

        assert_eq!(
            invoice_implements.len(),
            1,
            "Invoice should have one Implements edge"
        );

        let target_name = node_names
            .get(&invoice_implements[0].target)
            .expect("Target should have a name");
        assert_eq!(
            target_name, "Outer::Payable",
            "Scoped interface should be 'Outer::Payable', got: '{target_name}'"
        );

        let display_target_name = display_names
            .get(&invoice_implements[0].target)
            .expect("Display target should have a name");
        assert_eq!(
            display_target_name, "Outer.Payable",
            "Apex display interface should be 'Outer.Payable', got: '{display_target_name}'"
        );
    }
}
