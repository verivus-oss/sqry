//! SQL `GraphBuilder` implementation for code graph construction.
//!
//! Extracts SQL-specific relationships:
//! - Procedure/function definitions and calls
//! - Trigger definitions and activations
//! - Table read operations (SELECT)
//! - Table write operations (INSERT, UPDATE, DELETE, CREATE/DROP/ALTER TABLE)

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Span,
    unified::{GraphBuildHelper, StagingGraph},
};
use std::path::Path;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

#[derive(Debug, Clone)]
struct SqlCallable {
    node_id: sqry_core::graph::unified::NodeId,
    start_byte: usize,
    end_byte: usize,
}

#[derive(Debug, Clone)]
struct SqlDatabaseObject {
    node_id: sqry_core::graph::unified::NodeId,
}

#[derive(Debug, Clone)]
enum SqlTableOpKind {
    Read,
    Write(sqry_core::graph::unified::TableWriteOp),
}

#[derive(Debug, Clone)]
struct SqlTableOp {
    op_span_bytes: (usize, usize),
    kind: SqlTableOpKind,
    table_name: String,
    schema: Option<String>,
    table_node_id: sqry_core::graph::unified::NodeId,
    span: Span,
}

/// File-level module name for SQL exports.
///
/// Used to represent the SQL file as a module that exports all top-level
/// symbols (functions, procedures, views, tables, triggers).
const FILE_MODULE_NAME: &str = "<file_module>";

/// SQL-specific `GraphBuilder` implementation.
///
/// Performs multi-pass analysis:
/// 1. Extract procedure/function definitions
/// 2. Extract trigger definitions
/// 3. Extract table access patterns (reads and writes)
/// 4. Synthesize edges between entities
/// 5. Emit Export edges from file module to exported symbols
#[derive(Debug, Default, Clone, Copy)]
pub struct SqlGraphBuilder;

impl SqlGraphBuilder {
    /// Create a new SQL `GraphBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for SqlGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Sql);

        // Compile tree-sitter queries
        let language = tree_sitter_sequel::LANGUAGE.into();
        let queries = SqlQueries::new(&language)?;

        // Extract procedure/function definitions
        let mut callables = extract_procedures(tree, content, &queries.procedures, &mut helper);

        // Extract trigger definitions
        callables.extend(extract_triggers(
            tree,
            content,
            &queries.triggers,
            &mut helper,
        ));

        // Extract table read operations
        let table_reads = extract_table_reads(tree, content, &queries.table_reads, &mut helper);

        // Extract table write operations
        let table_writes = extract_table_writes(tree, content, &queries.table_writes, &mut helper);

        // Extract function/procedure calls
        let function_calls = extract_function_calls(tree, content, &queries.function_calls);

        // Extract table definitions (CREATE TABLE)
        let table_definitions =
            extract_table_definitions(tree, content, &queries.table_definitions, &mut helper);

        // Extract view definitions (CREATE VIEW, CREATE MATERIALIZED VIEW)
        let view_definitions =
            extract_view_definitions(tree, content, &queries.view_definitions, &mut helper);

        // Synthesize edges from callables to table operations based on lexical containment.
        for op in table_reads.into_iter().chain(table_writes) {
            let Some(caller) = find_enclosing_callable(&callables, op.op_span_bytes) else {
                continue;
            };

            match op.kind {
                SqlTableOpKind::Read => helper.add_table_read_edge_with_span(
                    caller.node_id,
                    op.table_node_id,
                    &op.table_name,
                    op.schema.as_deref(),
                    vec![op.span],
                ),
                SqlTableOpKind::Write(operation) => helper.add_table_write_edge_with_span(
                    caller.node_id,
                    op.table_node_id,
                    &op.table_name,
                    op.schema.as_deref(),
                    operation,
                    vec![op.span],
                ),
            }
        }

        // Synthesize call edges from callables to called functions based on lexical containment.
        for call in function_calls {
            // Find enclosing callable (procedure/function/trigger)
            if let Some(caller) = find_enclosing_callable(&callables, call.span_bytes) {
                // Create callee function node and add call edge
                // Call-site extent, not a declaration of the callee (issue #748).
                let callee_id =
                    helper.add_call_site_node(&call.callee_name, call.span, NodeKind::Function);
                helper.add_call_edge_full_with_span(
                    caller.node_id,
                    callee_id,
                    255,
                    false,
                    vec![call.span],
                );
            }
            // Note: Top-level calls outside procedures are skipped to avoid
            // creating a synthetic module-level caller (which could cause node kind collisions)
        }

        // Extract and create call edges for trigger EXECUTE FUNCTION
        // This captures the relationship: trigger -> executed function
        extract_trigger_execute_function_calls(
            tree,
            content,
            &queries.trigger_execute_function,
            &callables,
            &mut helper,
        );

        // Emit Export edges for all database objects (callables, tables, views)
        emit_exports(
            &mut helper,
            &callables,
            &table_definitions,
            &view_definitions,
        );

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Sql
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(sql_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for SQL (tree-sitter-sequel).
///
/// SQL functions and procedures are real graph callables (`create_function`),
/// so the body-shape descriptor applies to them. The control flow that the
/// grammar parses lives inside `LANGUAGE sql` bodies (CASE expressions, set
/// operations, subqueries); `LANGUAGE plpgsql` and other dollar-quoted bodies
/// are opaque text to this grammar, so a procedural body with no parsed CF
/// kinds yields a near-empty histogram, which is the honest result. The
/// mapping is built once from the grammar and shared process-wide.
pub struct SqlShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl SqlShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
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
                *slot = cf_bucket_for_sql_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for SqlShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // `create_function` exposes the argument list as a `function_arguments`
        // node (no field name), so locate it among the named children.
        let mut cursor = fn_node.walk();
        for child in fn_node.named_children(&mut cursor) {
            if child.kind() == "function_arguments" {
                let mut arg_cursor = child.walk();
                for arg in child.named_children(&mut arg_cursor) {
                    if arg.kind() == "function_argument" {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                        // A `keyword_default` token inside the argument marks a
                        // default value.
                        let mut def_cursor = arg.walk();
                        for piece in arg.children(&mut def_cursor) {
                            if piece.kind() == "keyword_default" {
                                shape.has_defaults = true;
                            }
                        }
                    }
                }
            }
        }
        shape
    }
}

/// Map one tree-sitter-sequel grammar node-kind name to its canonical
/// control-flow bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_sql_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        // `CASE ... WHEN ... THEN ... END` is the SQL conditional construct.
        "case" => CfBucket::Match,
        "when_clause" => CfBucket::Branch,
        // Procedural assignment (`x := y`) inside parsed routine bodies.
        "assignment" => CfBucket::Assign,
        // Function/aggregate invocations carried by the query body.
        "invocation" => CfBucket::Call,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide SQL shape mapping, built once on first use.
#[must_use]
pub fn sql_shape_mapping() -> &'static SqlShapeMapping {
    static MAPPING: OnceLock<SqlShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(SqlShapeMapping::build)
}

/// Tree-sitter queries for SQL relationship extraction.
struct SqlQueries {
    procedures: Query,
    triggers: Query,
    trigger_execute_function: Query,
    table_reads: Query,
    table_writes: Query,
    function_calls: Query,
    table_definitions: Query,
    view_definitions: Query,
}

impl SqlQueries {
    // Query construction is verbose but kept together for clarity.
    #[allow(clippy::too_many_lines)]
    fn new(language: &tree_sitter::Language) -> GraphResult<Self> {
        // Query for procedure/function definitions (covers both)
        let procedures = Query::new(
            language,
            r"
            (create_function
              (object_reference
                name: (identifier) @func.name)) @func
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile procedure query: {e}"),
        })?;

        // Query for trigger definitions
        // Note: Both trigger name and table name are in object_reference nodes
        let triggers = Query::new(
            language,
            r"
            (create_trigger
              (object_reference
                name: (identifier) @trigger.name)
              (keyword_on)
              (object_reference
                name: (identifier) @trigger.table)) @trigger
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile trigger query: {e}"),
        })?;

        // Query for trigger EXECUTE FUNCTION (to create call edges)
        // Captures trigger name and the function being executed
        let trigger_execute_function = Query::new(
            language,
            r"
            (create_trigger
              (object_reference
                name: (identifier) @trigger.name)
              (keyword_execute)
              (keyword_function)
              (object_reference
                name: (identifier) @func.name)) @trigger_exec
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile trigger_execute_function query: {e}"),
        })?;

        // Query for table reads (SELECT statements)
        // AST structure: (statement (select ...) (from (keyword_from) (relation (object_reference name: (identifier)))))
        let table_reads = Query::new(
            language,
            r"
            (statement
              (select) @select
              (from
                (keyword_from)
                (relation
                  (object_reference
                    name: (identifier) @table.name))))
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile table_reads query: {e}"),
        })?;

        // Query for table writes (INSERT, UPDATE, DELETE statements)
        // INSERT: (insert ... (object_reference name: (identifier)))
        // UPDATE: (update (relation (object_reference name: (identifier))))
        // DELETE: (statement (delete) (from (keyword_from) (object_reference name: (identifier))))
        let table_writes = Query::new(
            language,
            r"
            [
              (insert
                (object_reference
                  name: (identifier) @table.name)) @write

              (update
                (relation
                  (object_reference
                    name: (identifier) @table.name))) @write

              (statement
                (delete) @write
                (from
                  (keyword_from)
                  (object_reference
                    name: (identifier) @table.name)))
            ]
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile table_writes query: {e}"),
        })?;

        // Query for function/procedure calls.
        // Includes:
        // - normal invocations like function_name(args) or schema.function_name(args)
        // - PL/pgSQL assignment forms that tree-sitter-sequel currently surfaces as
        //   ERROR nodes, e.g. `sum_val := add(x, y);`
        let function_calls = Query::new(
            language,
            r#"
            [
              (invocation
                (object_reference
                  name: (identifier) @call.name)) @call

              (ERROR
                ":="
                (_) @call.name
                "(") @call

              (ERROR) @call.error
            ]
            "#,
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile function_calls query: {e}"),
        })?;

        // Query for table definitions (CREATE TABLE)
        let table_definitions = Query::new(
            language,
            r"
            (create_table
              (object_reference
                name: (identifier) @table.name)) @table
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile table_definitions query: {e}"),
        })?;

        // Query for view definitions (CREATE VIEW, CREATE MATERIALIZED VIEW)
        let view_definitions = Query::new(
            language,
            r"
            [
              (create_view
                (object_reference
                  name: (identifier) @view.name)) @view
              (create_materialized_view
                (object_reference
                  name: (identifier) @view.name)) @view
            ]
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile view_definitions query: {e}"),
        })?;

        Ok(Self {
            procedures,
            triggers,
            trigger_execute_function,
            table_reads,
            table_writes,
            function_calls,
            table_definitions,
            view_definitions,
        })
    }
}

/// Extract procedure/function definitions from the AST.
fn extract_procedures(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlCallable> {
    let mut callables = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut func_name = None;
        let mut func_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            if name == "func.name"
                && let Ok(text) = capture.node.utf8_text(content)
            {
                func_name = Some(text.to_string());
            }
            if name == "func" {
                func_node = Some(capture.node);
            }
        }

        if let (Some(name), Some(node)) = (func_name, func_node) {
            let span = Span::from_node(&node);
            let node_id = helper.add_function(&name, Some(span), false, false);
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(node_id);
            callables.push(SqlCallable {
                node_id,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    }

    callables
}

/// Extract trigger definitions from the AST.
fn extract_triggers(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlCallable> {
    let mut callables = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut trigger_name = None;
        let mut table_name = None;
        let mut trigger_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "trigger.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        trigger_name = Some(text.to_string());
                    }
                }
                "trigger.table" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        table_name = Some(text.to_string());
                    }
                }
                "trigger" => {
                    trigger_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(trigger), Some(table), Some(node)) = (trigger_name, table_name, trigger_node) {
            let (schema, table_only) = split_schema_table(&table);
            let span = Span::from_node(&node);

            let trigger_id = helper.add_function(&trigger, Some(span), false, false);
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(trigger_id);
            callables.push(SqlCallable {
                node_id: trigger_id,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });

            let table_id = helper.add_variable(table_only, Some(span));
            helper.add_triggered_by_edge_with_span(
                trigger_id,
                table_id,
                &trigger,
                schema,
                vec![span],
            );
        }
    }

    callables
}

/// Extract trigger EXECUTE FUNCTION call edges.
///
/// Creates call edges from triggers to the functions they execute via
/// the `EXECUTE FUNCTION function_name()` clause.
fn extract_trigger_execute_function_calls(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    callables: &[SqlCallable],
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut trigger_name = None;
        let mut func_name = None;
        let mut trigger_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "trigger.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        trigger_name = Some(text.to_string());
                    }
                }
                "func.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        func_name = Some(text.to_string());
                    }
                }
                "trigger_exec" => {
                    trigger_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(_trigger), Some(func), Some(node)) = (trigger_name, func_name, trigger_node) {
            let span = Span::from_node(&node);

            // Find the trigger's node ID from callables
            if let Some(trigger_callable) = callables.iter().find(|c| {
                // Match by byte range overlap (trigger definition contains this execute)
                c.start_byte <= node.start_byte() && node.end_byte() <= c.end_byte
            }) {
                // Create callee function node (or reuse existing) and add call edge
                // EXECUTE FUNCTION extent, not a declaration (issue #748).
                let callee_id = helper.add_call_site_node(&func, span, NodeKind::Function);
                helper.add_call_edge_full_with_span(
                    trigger_callable.node_id,
                    callee_id,
                    255,
                    false,
                    vec![span],
                );
            }
        }
    }
}

/// Extract table read operations (SELECT statements).
fn extract_table_reads(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlTableOp> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut table_name = None;
        let mut op_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "table.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        table_name = Some(text.to_string());
                    }
                }
                "select" => op_node = Some(capture.node),
                _ => {}
            }
        }

        if let (Some(table_name), Some(node)) = (table_name, op_node) {
            let (schema, table_only) = split_schema_table(&table_name);
            let span = Span::from_node(&node);
            let table_node_id = helper.add_variable(table_only, Some(span));
            ops.push(SqlTableOp {
                op_span_bytes: (node.start_byte(), node.end_byte()),
                kind: SqlTableOpKind::Read,
                table_name: table_only.to_string(),
                schema: schema.map(str::to_string),
                table_node_id,
                span,
            });
        }
    }

    ops
}

/// Extract table write operations (INSERT, UPDATE, DELETE statements).
fn extract_table_writes(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlTableOp> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut table_name = None;
        let mut write_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "table.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        table_name = Some(text.to_string());
                    }
                }
                "write" => write_node = Some(capture.node),
                _ => {}
            }
        }

        let Some(table_name) = table_name else {
            continue;
        };
        let Some(node) = write_node else {
            continue;
        };

        let operation = match node.kind() {
            "insert" => sqry_core::graph::unified::TableWriteOp::Insert,
            "delete" => sqry_core::graph::unified::TableWriteOp::Delete,
            _ => sqry_core::graph::unified::TableWriteOp::Update,
        };

        let (schema, table_only) = split_schema_table(&table_name);
        let span = Span::from_node(&node);
        let table_node_id = helper.add_variable(table_only, Some(span));
        ops.push(SqlTableOp {
            op_span_bytes: (node.start_byte(), node.end_byte()),
            kind: SqlTableOpKind::Write(operation),
            table_name: table_only.to_string(),
            schema: schema.map(str::to_string),
            table_node_id,
            span,
        });
    }

    ops
}

/// SQL function call information
#[derive(Debug)]
struct SqlFunctionCall {
    callee_name: String,
    span_bytes: (usize, usize),
    span: Span,
}

/// Extract function/procedure calls from the AST.
fn extract_function_calls(tree: &Tree, content: &[u8], query: &Query) -> Vec<SqlFunctionCall> {
    let mut calls = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut call_name = None;
        let mut call_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "call.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        call_name = Some(normalize_callee_name(text));
                    }
                }
                "call" | "call.error" => call_node = Some(capture.node),
                _ => {}
            }
        }

        let Some(node) = call_node else {
            continue;
        };

        let span_bytes = (node.start_byte(), node.end_byte());
        let span = Span::from_node(&node);

        if node.kind() == "ERROR" {
            if let Ok(text) = node.utf8_text(content) {
                for name in extract_error_call_names(text) {
                    calls.push(SqlFunctionCall {
                        callee_name: name,
                        span_bytes,
                        span,
                    });
                }
            }
            continue;
        }

        if let Some(name) = call_name
            && !name.is_empty()
        {
            calls.push(SqlFunctionCall {
                callee_name: name,
                span_bytes,
                span,
            });
        }
    }

    calls
}

fn normalize_callee_name(name: &str) -> String {
    name.trim()
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn extract_error_call_names(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut offset = 0;
    let mut call_names = Vec::new();

    while offset < bytes.len() {
        if !is_sql_identifier_start(bytes[offset]) {
            offset += 1;
            continue;
        }

        let start = offset;
        offset += 1;
        while offset < bytes.len() && is_sql_identifier_continue(bytes[offset]) {
            offset += 1;
        }

        let token = &text[start..offset];
        let mut lookahead = offset;
        while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
            lookahead += 1;
        }

        if lookahead < bytes.len() && bytes[lookahead] == b'(' {
            let normalized = normalize_callee_name(token);
            if !normalized.is_empty() && !call_names.iter().any(|name| name == &normalized) {
                call_names.push(normalized);
            }
        }
    }

    call_names
}

const fn is_sql_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_sql_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')
}

/// Extract table definitions from the AST (CREATE TABLE statements).
fn extract_table_definitions(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlDatabaseObject> {
    let mut objects = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut table_name = None;
        let mut table_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "table.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        table_name = Some(text.to_string());
                    }
                }
                "table" => table_node = Some(capture.node),
                _ => {}
            }
        }

        if let (Some(name), Some(node)) = (table_name, table_node) {
            // Strip schema prefix if present (e.g., "public.users" -> "users")
            let (_, table_only) = split_schema_table(&name);
            let span = Span::from_node(&node);
            let node_id = helper.add_variable(table_only, Some(span));
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(node_id);
            objects.push(SqlDatabaseObject { node_id });
        }
    }

    objects
}

/// Extract view definitions from the AST (CREATE VIEW and CREATE MATERIALIZED VIEW statements).
fn extract_view_definitions(
    tree: &Tree,
    content: &[u8],
    query: &Query,
    helper: &mut GraphBuildHelper,
) -> Vec<SqlDatabaseObject> {
    let mut objects = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut view_name = None;
        let mut view_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "view.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        view_name = Some(text.to_string());
                    }
                }
                "view" => view_node = Some(capture.node),
                _ => {}
            }
        }

        if let (Some(name), Some(node)) = (view_name, view_node) {
            // Strip schema prefix if present
            let (_, view_only) = split_schema_table(&name);
            let span = Span::from_node(&node);
            let node_id = helper.add_variable(view_only, Some(span));
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(node_id);
            objects.push(SqlDatabaseObject { node_id });
        }
    }

    objects
}

fn find_enclosing_callable(
    callables: &[SqlCallable],
    op_span_bytes: (usize, usize),
) -> Option<&SqlCallable> {
    let (start_byte, end_byte) = op_span_bytes;
    callables
        .iter()
        .filter(|c| c.start_byte <= start_byte && end_byte <= c.end_byte)
        .min_by_key(|c| c.end_byte.saturating_sub(c.start_byte))
}

fn split_schema_table(name: &str) -> (Option<&str>, &str) {
    let mut parts = name.splitn(2, '.');
    let first = parts.next().unwrap_or(name).trim();
    let second = parts.next().map(str::trim);
    match second {
        Some(table) if !table.is_empty() => (Some(first), table),
        _ => (None, first),
    }
}

/// Emit Export edges for all SQL database objects in the file.
///
/// Creates a file-level module and establishes Export edges from it to all
/// top-level SQL symbols (callables, tables, views). This allows queries to
/// discover exported symbols.
fn emit_exports(
    helper: &mut GraphBuildHelper,
    callables: &[SqlCallable],
    tables: &[SqlDatabaseObject],
    views: &[SqlDatabaseObject],
) {
    // Only create module if there are objects to export
    if callables.is_empty() && tables.is_empty() && views.is_empty() {
        return;
    }

    // Create the file-level module node
    let module_id = helper.add_module(FILE_MODULE_NAME, None);

    // Emit Export edges for callables (procedures, functions, triggers)
    for callable in callables {
        helper.add_export_edge(module_id, callable.node_id);
    }

    // Emit Export edges for tables
    for table in tables {
        helper.add_export_edge(module_id, table.node_id);
    }

    // Emit Export edges for views
    for view in views {
        helper.add_export_edge(module_id, view.node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::StagingOp;
    use sqry_core::graph::unified::TableWriteOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use std::path::PathBuf;

    fn parse_sql(sql: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("Failed to set SQL language");
        parser
            .parse(sql.as_bytes(), None)
            .expect("Failed to parse SQL")
    }

    /// Helper to extract table read edges from staging operations
    #[allow(dead_code)]
    fn get_table_read_edges(staging: &StagingGraph) -> Vec<String> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    kind: EdgeKind::TableRead { table_name, .. },
                    ..
                } = op
                {
                    // table_name is a StringId, we need to look it up
                    // For testing, we just check that the edge was created
                    Some(format!("TableRead({table_name:?})"))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Helper to extract table write edges from staging operations
    #[allow(dead_code)]
    fn get_table_write_edges(staging: &StagingGraph) -> Vec<(String, TableWriteOp)> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    kind:
                        EdgeKind::TableWrite {
                            table_name,
                            operation,
                            ..
                        },
                    ..
                } = op
                {
                    Some((format!("TableWrite({table_name:?})"), *operation))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Helper to count edges of a specific kind
    fn count_table_read_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TableRead { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn count_table_write_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::TableWrite { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn count_table_write_edges_by_op(staging: &StagingGraph, expected_op: TableWriteOp) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge { kind: EdgeKind::TableWrite { operation, .. }, .. }
                    if *operation == expected_op
                )
            })
            .count()
    }

    fn count_call_edges(staging: &StagingGraph) -> usize {
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

    /// Helper to count Export edges from staging operations
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
    fn test_sql_graph_builder_new() {
        let builder = SqlGraphBuilder::new();
        assert_eq!(builder.language(), Language::Sql);
    }

    #[test]
    fn test_select_creates_table_read_edge() {
        let sql = r"
            CREATE FUNCTION get_users()
            RETURNS TABLE (id INT, name TEXT) AS $$
                SELECT * FROM users;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let read_count = count_table_read_edges(&staging);
        assert!(
            read_count >= 1,
            "Expected at least 1 TableRead edge, got {read_count}"
        );
    }

    #[test]
    fn test_insert_creates_table_write_edge() {
        let sql = r"
            CREATE FUNCTION create_user(user_name TEXT)
            RETURNS VOID AS $$
                INSERT INTO users (name) VALUES (user_name);
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let insert_count = count_table_write_edges_by_op(&staging, TableWriteOp::Insert);
        assert!(
            insert_count >= 1,
            "Expected at least 1 TableWrite(Insert) edge, got {insert_count}"
        );
    }

    #[test]
    fn test_update_creates_table_write_edge() {
        let sql = r"
            CREATE FUNCTION update_user(user_id INT, new_name TEXT)
            RETURNS VOID AS $$
                UPDATE users SET name = new_name WHERE id = user_id;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let update_count = count_table_write_edges_by_op(&staging, TableWriteOp::Update);
        assert!(
            update_count >= 1,
            "Expected at least 1 TableWrite(Update) edge, got {update_count}"
        );
    }

    #[test]
    fn test_delete_creates_table_write_edge() {
        let sql = r"
            CREATE FUNCTION delete_user(user_id INT)
            RETURNS VOID AS $$
                DELETE FROM users WHERE id = user_id;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let delete_count = count_table_write_edges_by_op(&staging, TableWriteOp::Delete);
        assert!(
            delete_count >= 1,
            "Expected at least 1 TableWrite(Delete) edge, got {delete_count}"
        );
    }

    #[test]
    fn test_join_creates_table_read_edge_for_primary_table() {
        // NOTE: tree-sitter-sequel only captures the primary FROM table, not JOINed tables
        // This is a grammar limitation, not a bug in our query
        let sql = r"
            CREATE FUNCTION get_user_orders()
            RETURNS TABLE (user_name TEXT, order_id INT) AS $$
                SELECT u.name, o.id FROM users u JOIN orders o ON u.id = o.user_id;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        // Should have at least 1 TableRead edge (users - the primary FROM table)
        let read_count = count_table_read_edges(&staging);
        assert!(
            read_count >= 1,
            "Expected at least 1 TableRead edge for FROM clause, got {read_count}"
        );
    }

    #[test]
    fn test_multiple_joins_creates_table_read_edge_for_primary_table() {
        // NOTE: tree-sitter-sequel only captures the primary FROM table, not JOINed tables
        let sql = r"
            CREATE FUNCTION get_order_details()
            RETURNS TABLE (user_name TEXT, product_name TEXT, quantity INT) AS $$
                SELECT u.name, p.name, o.quantity
                FROM users u
                JOIN orders o ON u.id = o.user_id
                LEFT JOIN products p ON o.product_id = p.id;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        // Should have at least 1 TableRead edge (users - the primary FROM table)
        let read_count = count_table_read_edges(&staging);
        assert!(
            read_count >= 1,
            "Expected at least 1 TableRead edge for FROM clause, got {read_count}"
        );
    }

    #[test]
    fn test_mixed_read_write_operations() {
        // NOTE: tree-sitter-sequel requires BEGIN...END; for multiple statements
        let sql = r"
            CREATE FUNCTION transfer_funds(from_id INT, to_id INT, amount DECIMAL)
            RETURNS VOID AS $$
            BEGIN
                SELECT balance FROM accounts WHERE id = from_id;
                UPDATE accounts SET balance = balance - amount WHERE id = from_id;
                UPDATE accounts SET balance = balance + amount WHERE id = to_id;
                INSERT INTO transactions (from_account, to_account, amount) VALUES (from_id, to_id, amount);
            END;
            $$ LANGUAGE plpgsql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let read_count = count_table_read_edges(&staging);
        let write_count = count_table_write_edges(&staging);

        // We expect at least 1 read (accounts) and multiple writes
        assert!(
            read_count >= 1,
            "Expected at least 1 TableRead edge, got {read_count}"
        );
        assert!(
            write_count >= 1,
            "Expected at least 1 TableWrite edge, got {write_count}"
        );
    }

    #[test]
    fn test_plpgsql_assignment_function_calls_create_call_edges() {
        let sql = r"
            CREATE FUNCTION add(a INT, b INT) RETURNS INT AS $$
            BEGIN
                RETURN a + b;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION multiply(a INT, b INT) RETURNS INT AS $$
            BEGIN
                RETURN a * b;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION compute(x INT, y INT, z INT) RETURNS INT AS $$
            DECLARE
                sum_val INT;
            BEGIN
                sum_val := add(x, y);
                RETURN multiply(sum_val, z);
            END;
            $$ LANGUAGE plpgsql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("nested_calls.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let call_count = count_call_edges(&staging);
        assert!(
            call_count >= 2,
            "Expected at least 2 call edges for add() and multiply(), got {call_count}"
        );
    }

    #[test]
    fn test_plpgsql_multiple_assignment_calls_create_call_edges() {
        let sql = r"
            CREATE FUNCTION helper_one() RETURNS INT AS $$
            BEGIN
                RETURN 42;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION helper_two() RETURNS INT AS $$
            BEGIN
                RETURN 100;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION orchestrator() RETURNS INT AS $$
            DECLARE
                val1 INT;
                val2 INT;
            BEGIN
                val1 := helper_one();
                val2 := helper_two();
                RETURN val1 + val2;
            END;
            $$ LANGUAGE plpgsql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("multiple_assignment_calls.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let call_count = count_call_edges(&staging);
        assert!(
            call_count >= 2,
            "Expected at least 2 call edges for helper_one() and helper_two(), got {call_count}"
        );
    }

    #[test]
    fn test_schema_qualified_table_name() {
        let sql = r"
            CREATE FUNCTION get_public_users()
            RETURNS TABLE (id INT, name TEXT) AS $$
                SELECT * FROM public.users;
            $$ LANGUAGE sql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("test.sql");

        // Should not fail on schema-qualified names
        let result = builder.build_graph(&tree, sql.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle schema-qualified table names");
    }

    #[test]
    fn test_split_schema_table_with_schema() {
        let (schema, table) = split_schema_table("public.users");
        assert_eq!(schema, Some("public"));
        assert_eq!(table, "users");
    }

    #[test]
    fn test_split_schema_table_without_schema() {
        let (schema, table) = split_schema_table("users");
        assert_eq!(schema, None);
        assert_eq!(table, "users");
    }

    #[test]
    fn test_split_schema_table_with_whitespace() {
        let (schema, table) = split_schema_table(" public . users ");
        assert_eq!(schema, Some("public"));
        assert_eq!(table, "users");
    }

    #[test]
    fn test_empty_sql_file() {
        let sql = "";
        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("empty.sql");

        let result = builder.build_graph(&tree, sql.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle empty SQL files");
    }

    #[test]
    fn test_standalone_select_without_function() {
        // Standalone SELECT should not create edges (no enclosing callable)
        let sql = "SELECT * FROM users;";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("query.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        // Table reads outside functions are NOT synthesized into edges
        // because there's no enclosing callable to serve as the edge source
        let read_count = count_table_read_edges(&staging);
        assert_eq!(
            read_count, 0,
            "Standalone SELECT should not create edges without enclosing function"
        );
    }

    #[test]
    fn test_export_edges_for_table_definitions() {
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL
            );

            CREATE TABLE orders (
                id SERIAL PRIMARY KEY,
                user_id INTEGER REFERENCES users(id)
            );
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("schema.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 2,
            "Expected 2 Export edges (users and orders), got {export_count}"
        );
    }

    #[test]
    fn test_export_edges_for_view_definitions() {
        let sql = r"
            CREATE TABLE users (id INT, created_at TIMESTAMP);

            CREATE VIEW active_users AS
            SELECT * FROM users WHERE created_at > NOW() - INTERVAL '30 days';

            CREATE MATERIALIZED VIEW user_stats AS
            SELECT COUNT(*) as total FROM users;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("views.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        // Expect 3 exports: 1 table (users) + 2 views (active_users, user_stats)
        assert_eq!(
            export_count, 3,
            "Expected 3 Export edges (1 table + 2 views), got {export_count}"
        );
    }

    #[test]
    fn test_export_edges_for_functions_and_triggers() {
        let sql = r"
            CREATE FUNCTION get_balance(account_id INT) RETURNS BIGINT AS $$
            BEGIN
                RETURN 42;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION update_balance() RETURNS TRIGGER AS $$
            BEGIN
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;

            CREATE TRIGGER balance_updated
            BEFORE INSERT ON accounts
            FOR EACH ROW
            EXECUTE FUNCTION update_balance();
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("banking.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        // Note: The trigger "BEFORE INSERT ON accounts" creates a variable node for "accounts" table.
        // Tree-sitter might also capture the INSERT keyword, creating an additional table node.
        // We expect at least 3 exports (2 functions + 1 trigger), but may get 4 due to the table reference.
        assert!(
            export_count >= 3,
            "Expected at least 3 Export edges (2 functions + 1 trigger), got {export_count}"
        );
    }

    #[test]
    fn test_export_edges_with_schema_qualified_names() {
        let sql = r"
            CREATE TABLE public.customers (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL
            );

            CREATE FUNCTION public.get_customer_name(cust_id INT) RETURNS TEXT AS $$
            BEGIN
                RETURN 'test';
            END;
            $$ LANGUAGE plpgsql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("public_schema.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        // Expect 2 exports: 1 table (customers, schema stripped) + 1 function (get_customer_name)
        assert_eq!(
            export_count, 2,
            "Expected 2 Export edges (table + function), got {export_count}"
        );
    }

    #[test]
    fn test_mixed_database_objects_exports() {
        let sql = r"
            CREATE TABLE accounts (
                id SERIAL PRIMARY KEY,
                balance_cents BIGINT NOT NULL
            );

            CREATE VIEW positive_balances AS
            SELECT * FROM accounts WHERE balance_cents > 0;

            CREATE FUNCTION get_balance(account_id INT) RETURNS BIGINT AS $$
            BEGIN
                RETURN (SELECT balance_cents FROM accounts WHERE id = account_id);
            END;
            $$ LANGUAGE plpgsql;
        ";

        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("mixed.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        // Expect 3 exports: 1 table (accounts) + 1 view (positive_balances) + 1 function (get_balance)
        assert_eq!(
            export_count, 3,
            "Expected 3 Export edges (table + view + function), got {export_count}"
        );
    }

    #[test]
    fn test_no_exports_for_empty_file() {
        let sql = "";
        let tree = parse_sql(sql);
        let mut staging = StagingGraph::new();
        let builder = SqlGraphBuilder::new();
        let file = PathBuf::from("empty.sql");

        builder
            .build_graph(&tree, sql.as_bytes(), &file, &mut staging)
            .expect("Graph building should succeed");

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for empty file, got {export_count}"
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{ShapeBudget, compute_shape_descriptor};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/data/sample.sql"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("load sql grammar");
        parser.parse(src, None).expect("parse")
    }

    /// Resolve the first `create_function` node anywhere in the tree.
    fn first_create_function(node: Node<'_>) -> Option<Node<'_>> {
        if node.kind() == "create_function" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_create_function(child) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn cf_map_is_non_empty_and_covers_real_kinds() {
        let mapping = sql_shape_mapping();
        let populated = mapping.cf_by_kind_id.iter().filter(|s| s.is_some()).count();
        assert!(
            populated > 0,
            "SQL cf map must map at least one real grammar kind"
        );

        // The grammar exposes `case`/`when_clause` as named kinds; confirm the
        // mapping resolves them to the canonical buckets by id, not by guesswork.
        let lang: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
        let case_id = lang.id_for_node_kind("case", true);
        let when_id = lang.id_for_node_kind("when_clause", true);
        assert_eq!(mapping.cf_bucket(case_id), Some(CfBucket::Match));
        assert_eq!(mapping.cf_bucket(when_id), Some(CfBucket::Branch));
    }

    #[test]
    fn descriptor_counts_case_control_flow_in_sql_body() {
        let tree = parse(SAMPLE);
        let func = first_create_function(tree.root_node()).expect("create_function in fixture");
        let descriptor = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            sql_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(
            !descriptor.is_unhashable(),
            "a function with a parsed CASE body must be hashable"
        );
        assert!(
            descriptor.cf_histogram[CfBucket::Match.index()] >= 1,
            "the CASE expression must be counted in the Match bucket"
        );
    }

    #[test]
    fn signature_shape_reads_arguments_and_defaults() {
        let tree = parse(SAMPLE);
        let func = first_create_function(tree.root_node()).expect("create_function in fixture");
        let shape = sql_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        assert_eq!(
            shape.arity_positional, 2,
            "grade(score, bonus) has two arguments"
        );
        assert!(
            shape.has_defaults,
            "the DEFAULT 0 argument must set has_defaults"
        );
    }
}
