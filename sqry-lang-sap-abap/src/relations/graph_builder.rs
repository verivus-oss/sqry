// Nested conditionals kept for readability in ABAP AST traversal

//! SAP ABAP `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts relationships from ABAP code:
//! - Class method definitions
//! - CALL FUNCTION references
//! - SELECT statement table references (`TableRead` edges)
//! - INSERT/MODIFY/UPDATE/DELETE table references (`TableWrite` edges)
//! - INCLUDE statement import edges (`Imports` edges)
//! - TYPE-POOLS statement import edges (`Imports` edges)
//!
//! ## Grammar Limitations
//!
//! The tree-sitter-abap grammar has limited SQL statement coverage:
//! - `select_statement_obsolete` is parsed and supports `TableRead` extraction
//! - INSERT, MODIFY, UPDATE, DELETE are NOT parsed by the grammar
//! - TYPE-POOLS is NOT parsed by the grammar
//!
//! For write operations and TYPE-POOLS, this implementation uses text-based
//! pattern matching as a fallback since the grammar doesn't support these statements.

use std::collections::HashSet;
use std::path::Path;

use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span,
    unified::edge::kind::TypeOfContext,
    unified::{GraphBuildHelper, NodeId, StagingGraph, TableWriteOp},
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

use super::type_extractor;

/// Information about an ABAP callable (method, function, etc.)
#[derive(Debug, Clone)]
struct AbapCallable {
    node_id: NodeId,
    start_byte: usize,
    end_byte: usize,
}

/// Information about an ABAP class
#[derive(Debug, Clone)]
struct AbapClass {
    node_id: NodeId,
    start_byte: usize,
    end_byte: usize,
}

/// Table operation kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableOpKind {
    Read,
    Insert,
    Modify,
    Update,
    Delete,
}

/// Table operation information
#[derive(Debug, Clone)]
struct TableOp {
    kind: TableOpKind,
    table_name: String,
    span: Span,
    start_byte: usize,
    end_byte: usize,
}

/// `GraphBuilder` for SAP ABAP files
#[derive(Debug, Default)]
pub struct AbapGraphBuilder;

impl AbapGraphBuilder {
    /// Create a new ABAP graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for AbapGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for this file
        let mut helper = GraphBuildHelper::new(staging, file, Language::Abap);

        // Create module node from file path
        let module_name = extract_module_name_from_path(file);
        let module_id = helper.add_module(&module_name, None);

        // Compile tree-sitter queries
        let language = tree_sitter_abap_sqry::language();
        let queries = AbapQueries::new(&language)?;

        // Extract classes first
        let classes = extract_classes(tree, content, &queries, &mut helper);

        // Extract callable definitions (methods, functions) for enclosing context
        let callables = extract_callables(tree, content, &queries, &mut helper, &classes);

        // Create export edges for all callables (function modules and methods)
        for callable in &callables {
            helper.add_export_edge(module_id, callable.node_id);
        }

        // Extract program calls (SUBMIT and CALL TRANSACTION) and create edges
        extract_program_calls(content, &callables, &mut helper);

        // Extract import edges (INCLUDE and TYPE-POOLS statements)
        extract_imports(tree, content, &queries, &callables, &mut helper, module_id);

        // Extract table read operations (SELECT statements via grammar)
        let table_reads = extract_table_reads(tree, content, &queries);

        // Extract table write operations (INSERT/MODIFY/UPDATE/DELETE via text matching)
        let table_writes = extract_table_writes(content);

        // Combine all table operations
        let all_ops: Vec<TableOp> = table_reads.into_iter().chain(table_writes).collect();

        // Create edges from callables to table operations based on lexical containment
        for op in all_ops {
            // Find the enclosing callable for this operation
            let caller = find_enclosing_callable(&callables, op.start_byte, op.end_byte);

            // Create table node
            let table_node_id = helper.add_variable(&op.table_name, Some(op.span));

            match op.kind {
                TableOpKind::Read => {
                    if let Some(caller) = caller {
                        helper.add_table_read_edge_with_span(
                            caller.node_id,
                            table_node_id,
                            &op.table_name,
                            None, // ABAP typically doesn't use schema prefix in SELECT
                            vec![op.span],
                        );
                    }
                }
                TableOpKind::Insert => {
                    if let Some(caller) = caller {
                        helper.add_table_write_edge_with_span(
                            caller.node_id,
                            table_node_id,
                            &op.table_name,
                            None,
                            TableWriteOp::Insert,
                            vec![op.span],
                        );
                    }
                }
                TableOpKind::Modify => {
                    // MODIFY in ABAP is INSERT/UPDATE (upsert), use Update as closest match
                    if let Some(caller) = caller {
                        helper.add_table_write_edge_with_span(
                            caller.node_id,
                            table_node_id,
                            &op.table_name,
                            None,
                            TableWriteOp::Update,
                            vec![op.span],
                        );
                    }
                }
                TableOpKind::Update => {
                    if let Some(caller) = caller {
                        helper.add_table_write_edge_with_span(
                            caller.node_id,
                            table_node_id,
                            &op.table_name,
                            None,
                            TableWriteOp::Update,
                            vec![op.span],
                        );
                    }
                }
                TableOpKind::Delete => {
                    if let Some(caller) = caller {
                        helper.add_table_write_edge_with_span(
                            caller.node_id,
                            table_node_id,
                            &op.table_name,
                            None,
                            TableWriteOp::Delete,
                            vec![op.span],
                        );
                    }
                }
            }
        }

        // Extract TypeOf and References edges from type declarations
        extract_typeof_and_reference_edges(content, &callables, &mut helper);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Abap
    }
}

/// Tree-sitter queries for ABAP relationship extraction
struct AbapQueries {
    /// Query for class implementations
    classes: Query,
    /// Query for method implementations
    methods: Query,
    /// Query for function implementations
    functions: Query,
    /// Query for SELECT statements
    selects: Query,
    /// Query for INCLUDE statements
    includes: Query,
}

impl AbapQueries {
    fn new(language: &tree_sitter::Language) -> GraphResult<Self> {
        // Query for class implementations
        let classes = Query::new(
            language,
            r"
            (class_implementation
              name: (name) @class.name) @class
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile class query: {e}"),
        })?;

        // Query for method implementations
        let methods = Query::new(
            language,
            r"
            (method_implementation
              name: (name) @method.name) @method
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile method query: {e}"),
        })?;

        // Query for function implementations
        let functions = Query::new(
            language,
            r"
            (function_implementation
              name: (name) @func.name) @func
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile function query: {e}"),
        })?;

        // Query for SELECT statements (using select_statement_obsolete from grammar)
        // The grammar structure is:
        // (select_statement_obsolete
        //   (select_list)
        //   (from keyword)
        //   (data_source alias for name))
        let selects = Query::new(
            language,
            r"
            (select_statement_obsolete
              (data_source) @table.name) @select
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile select query: {e}"),
        })?;

        // Query for INCLUDE statements
        let includes = Query::new(
            language,
            r"
            (include_statement (name) @include.name) @include.node
            ",
        )
        .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to compile include query: {e}"),
        })?;

        Ok(Self {
            classes,
            methods,
            functions,
            selects,
            includes,
        })
    }
}

/// Extract module name from file path.
/// For ABAP files, use the file stem (filename without extension) as the module name.
fn extract_module_name_from_path(file: &Path) -> String {
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string()
}

/// Extract class definitions from the AST
fn extract_classes(
    tree: &Tree,
    content: &[u8],
    queries: &AbapQueries,
    helper: &mut GraphBuildHelper,
) -> Vec<AbapClass> {
    let mut classes = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = queries.classes.capture_names();
    let mut matches = cursor.matches(&queries.classes, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut class_name = None;
        let mut class_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            if name == "class.name"
                && let Ok(text) = capture.node.utf8_text(content)
            {
                class_name = Some(text.trim().to_string());
            }
            if name == "class" {
                class_node = Some(capture.node);
            }
        }

        if let (Some(name), Some(node)) = (class_name, class_node) {
            let span = span_from_node(&node);
            let visibility = extract_visibility(&name);
            let node_id = helper.add_class_with_visibility(&name, Some(span), Some(visibility));
            classes.push(AbapClass {
                node_id,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    }

    classes
}

/// Extract callable definitions (methods, functions) from the AST
#[allow(clippy::too_many_lines)]
fn extract_callables(
    tree: &Tree,
    content: &[u8],
    queries: &AbapQueries,
    helper: &mut GraphBuildHelper,
    classes: &[AbapClass],
) -> Vec<AbapCallable> {
    let mut callables = Vec::new();
    let mut seen_methods = HashSet::new();

    // Extract method implementations
    {
        let mut cursor = QueryCursor::new();
        let capture_names = queries.methods.capture_names();
        let mut matches = cursor.matches(&queries.methods, tree.root_node(), content);

        while let Some(m) = matches.next() {
            let mut method_name = None;
            let mut method_node = None;

            for capture in m.captures {
                let name = capture_names[capture.index as usize];
                if name == "method.name"
                    && let Ok(text) = capture.node.utf8_text(content)
                {
                    method_name = Some(text.trim().to_string());
                }
                if name == "method" {
                    method_node = Some(capture.node);
                }
            }

            if let (Some(name), Some(node)) = (method_name, method_node) {
                let span = span_from_node(&node);
                let visibility = extract_visibility(&name);
                let node_id = helper.add_method_with_visibility(
                    &name,
                    Some(span),
                    false,
                    false,
                    Some(visibility),
                );

                // Find enclosing class and create Contains edge
                let enclosing_class =
                    find_enclosing_class(classes, node.start_byte(), node.end_byte());
                if let Some(class) = enclosing_class {
                    helper.add_contains_edge(class.node_id, node_id);
                }

                callables.push(AbapCallable {
                    node_id,
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                });
                seen_methods.insert(name);
            }
        }
    }

    if let Ok(content_str) = std::str::from_utf8(content) {
        let lines: Vec<&str> = content_str.lines().collect();
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        offsets.push(0);
        let mut offset = 0;
        for line in &lines {
            offset += line.len() + 1;
            offsets.push(offset);
        }

        let mut pending: Option<(String, usize, usize)> = None;
        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let upper = trimmed.to_uppercase();
            if let Some(name) = parse_method_declaration_line(trimmed) {
                pending = Some((name, line_idx, offsets[line_idx]));
                continue;
            }
            if upper.starts_with("ENDMETHOD")
                && let Some((name, start_line, start_byte)) = pending.take()
            {
                if seen_methods.contains(&name) {
                    continue;
                }
                let span = span_from_line(start_line, lines[start_line].len());
                let end_byte = offsets[line_idx] + line.len();
                let visibility = extract_visibility(&name);
                let node_id = helper.add_method_with_visibility(
                    &name,
                    Some(span),
                    false,
                    false,
                    Some(visibility),
                );

                // Find enclosing class and create Contains edge
                let enclosing_class = find_enclosing_class(classes, start_byte, end_byte);
                if let Some(class) = enclosing_class {
                    helper.add_contains_edge(class.node_id, node_id);
                }

                callables.push(AbapCallable {
                    node_id,
                    start_byte,
                    end_byte,
                });
                seen_methods.insert(name);
            }
        }
    }

    // Extract function implementations
    {
        let mut cursor = QueryCursor::new();
        let capture_names = queries.functions.capture_names();
        let mut matches = cursor.matches(&queries.functions, tree.root_node(), content);

        while let Some(m) = matches.next() {
            let mut func_name = None;
            let mut func_node = None;

            for capture in m.captures {
                let name = capture_names[capture.index as usize];
                if name == "func.name"
                    && let Ok(text) = capture.node.utf8_text(content)
                {
                    func_name = Some(text.trim().to_string());
                }
                if name == "func" {
                    func_node = Some(capture.node);
                }
            }

            if let (Some(name), Some(node)) = (func_name, func_node) {
                let span = span_from_node(&node);
                let visibility = extract_visibility(&name);
                let node_id = helper.add_function_with_visibility(
                    &name,
                    Some(span),
                    false,
                    false,
                    Some(visibility),
                );
                callables.push(AbapCallable {
                    node_id,
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                });
            }
        }
    }

    callables
}

/// Extract table read operations (SELECT statements) from the AST
fn extract_table_reads(tree: &Tree, content: &[u8], queries: &AbapQueries) -> Vec<TableOp> {
    let mut ops = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names = queries.selects.capture_names();
    let mut matches = cursor.matches(&queries.selects, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut table_name = None;
        let mut select_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "table.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        let text = text.trim();
                        // Skip ABAP keywords that might be captured
                        if !is_abap_keyword(text) && is_valid_abap_identifier(text) {
                            table_name = Some(text.to_string());
                        }
                    }
                }
                "select" => {
                    select_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(table), Some(node)) = (table_name, select_node) {
            ops.push(TableOp {
                kind: TableOpKind::Read,
                table_name: table,
                span: span_from_node(&node),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    }

    ops
}

/// Extract table write operations from content using text-based pattern matching.
///
/// This is necessary because the tree-sitter-abap grammar doesn't parse
/// INSERT, MODIFY, UPDATE, DELETE statements.
///
/// Also scans for internal table declarations to build a per-file set of
/// internal table identifiers for more accurate filtering.
fn extract_table_writes(content: &[u8]) -> Vec<TableOp> {
    let Ok(content_str) = std::str::from_utf8(content) else {
        return Vec::new();
    };

    let mut ops = Vec::new();
    let lines: Vec<&str> = content_str.lines().collect();

    // Precompute line start byte offsets to avoid O(n²) recomputation
    let line_offsets: Vec<usize> = {
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        offsets.push(0);
        let mut offset = 0;
        for line in &lines {
            offset += line.len() + 1; // +1 for newline
            offsets.push(offset);
        }
        offsets
    };

    // Pre-scan for internal table declarations to build a per-file exclusion set
    // Patterns: DATA <name> TYPE TABLE OF ..., DATA: <name> TYPE STANDARD TABLE OF ...
    // FIELD-SYMBOLS <name> TYPE TABLE OF ..., TYPES <name> TYPE TABLE OF ...
    let declared_internal_tables = extract_declared_internal_tables(content_str);

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();
        let byte_offset = line_offsets[line_idx];

        // Parse INSERT statement: INSERT <table> FROM ...
        if let Some(table) = parse_insert_statement(&upper, trimmed)
            && is_valid_abap_identifier(&table)
            && !is_abap_keyword(&table)
            && !declared_internal_tables.contains(&table.to_lowercase())
        {
            let span = span_from_line(line_idx, trimmed.len());
            ops.push(TableOp {
                kind: TableOpKind::Insert,
                table_name: table,
                span,
                start_byte: byte_offset,
                end_byte: byte_offset + trimmed.len(),
            });
        }

        // Parse MODIFY statement: MODIFY <table> FROM ...
        if let Some(table) = parse_modify_statement(&upper, trimmed)
            && is_valid_abap_identifier(&table)
            && !is_abap_keyword(&table)
            && !declared_internal_tables.contains(&table.to_lowercase())
        {
            let span = span_from_line(line_idx, trimmed.len());
            ops.push(TableOp {
                kind: TableOpKind::Modify,
                table_name: table,
                span,
                start_byte: byte_offset,
                end_byte: byte_offset + trimmed.len(),
            });
        }

        // Parse UPDATE statement: UPDATE <table> SET ...
        if let Some(table) = parse_update_statement(&upper, trimmed)
            && is_valid_abap_identifier(&table)
            && !is_abap_keyword(&table)
            && !declared_internal_tables.contains(&table.to_lowercase())
        {
            let span = span_from_line(line_idx, trimmed.len());
            ops.push(TableOp {
                kind: TableOpKind::Update,
                table_name: table,
                span,
                start_byte: byte_offset,
                end_byte: byte_offset + trimmed.len(),
            });
        }

        // Parse DELETE statement: DELETE FROM <table> WHERE ... or DELETE <table> WHERE ...
        if let Some(table) = parse_delete_statement(&upper, trimmed)
            && is_valid_abap_identifier(&table)
            && !is_abap_keyword(&table)
            && !declared_internal_tables.contains(&table.to_lowercase())
        {
            let span = span_from_line(line_idx, trimmed.len());
            ops.push(TableOp {
                kind: TableOpKind::Delete,
                table_name: table,
                span,
                start_byte: byte_offset,
                end_byte: byte_offset + trimmed.len(),
            });
        }
    }

    ops
}

/// Extract TypeOf and References edges from ABAP type declarations.
///
/// Uses `type_extractor::extract_type_declarations()` to find DATA/TYPES/FIELD-SYMBOLS
/// declarations, then creates TypeOf edges for each variable and References edges
/// for non-builtin types.
fn extract_typeof_and_reference_edges(
    content: &[u8],
    _callables: &[AbapCallable],
    helper: &mut GraphBuildHelper,
) {
    let Ok(content_str) = std::str::from_utf8(content) else {
        return;
    };

    let decls = type_extractor::extract_type_declarations(content_str);

    for decl in &decls {
        // Create a variable node for this declaration
        let source_id = helper.add_variable(&decl.var_name, None);

        // TypeOf edge: source variable -> type node
        let target_id = helper.add_type(&decl.type_name, None);
        helper.add_typeof_edge_with_context(
            source_id,
            target_id,
            Some(TypeOfContext::Variable),
            None,
            Some(&decl.var_name),
        );

        // References edges for non-builtin types
        let mut seen = std::collections::HashSet::new();

        // Reference to main type (if not builtin).
        // For TABLE OF and REF TO patterns, use the base_type (the concrete class/type).
        let ref_type = if decl.base_type.is_some() {
            decl.base_type.as_deref()
        } else {
            Some(decl.type_name.as_str())
        };

        if let Some(type_name) = ref_type
            && !type_extractor::is_abap_builtin_type(type_name)
            && seen.insert(type_name.to_string())
        {
            let ref_target = helper.add_type(type_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }

        // For LIKE declarations, reference the other variable
        if decl.is_like
            && !type_extractor::is_abap_builtin_type(&decl.type_name)
            && seen.insert(decl.type_name.clone())
        {
            let ref_target = helper.add_variable(&decl.type_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }
    }
}

/// Extract identifiers declared as internal tables from the source.
/// Scans for patterns like:
///
/// - DATA <name> TYPE TABLE OF ...
/// - DATA <name> TYPE STANDARD TABLE OF ...
/// - DATA <name> TYPE SORTED TABLE OF ...
/// - DATA <name> TYPE HASHED TABLE OF ...
/// - DATA: <name> TYPE TABLE OF ...
/// - FIELD-SYMBOLS <name> TYPE TABLE OF ...
/// - TYPES <name> TYPE TABLE OF ...
///
/// Returns a set of lowercase identifiers that are known internal tables.
fn extract_declared_internal_tables(content: &str) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let mut internal_tables = HashSet::new();

    // Regex-like pattern matching using simple string operations
    // We look for patterns: DATA/TYPES/FIELD-SYMBOLS <name> TYPE [STANDARD/SORTED/HASHED] TABLE OF
    for line in content.lines() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();

        if upper.contains("FIELD-SYMBOL(")
            && upper.contains("TYPE")
            && (upper.contains("TABLE OF")
                || upper.contains("STANDARD TABLE")
                || upper.contains("SORTED TABLE")
                || upper.contains("HASHED TABLE"))
            && let Some(name) = extract_inline_field_symbol_name(trimmed)
        {
            internal_tables.insert(name.to_lowercase());
        }

        // Skip if line doesn't contain TABLE OF or TABLE keyword with table type
        if !upper.contains("TABLE OF")
            && !upper.contains("STANDARD TABLE")
            && !upper.contains("SORTED TABLE")
            && !upper.contains("HASHED TABLE")
        {
            continue;
        }

        // Check for DATA, TYPES, or FIELD-SYMBOLS declarations
        let decl_start = if upper.starts_with("DATA ") || upper.starts_with("DATA:") {
            Some(4)
        } else if upper.starts_with("TYPES ") || upper.starts_with("TYPES:") {
            Some(5)
        } else if upper.starts_with("FIELD-SYMBOLS ") || upper.starts_with("FIELD-SYMBOLS:") {
            Some(13)
        } else {
            None
        };

        if let Some(start) = decl_start {
            // Extract the rest after the declaration keyword
            if let Some(rest) = trimmed.get(start..) {
                let rest = rest.trim();

                // Handle colon notation for all declaration types:
                // - "DATA: var1 TYPE ..., var2 TYPE ..."
                // - "TYPES: type1 TYPE ..., type2 TYPE ..."
                // - "FIELD-SYMBOLS: <fs1> TYPE ..., <fs2> TYPE ..."
                // Check if we have colon notation (rest starts with : or after a space)
                let rest = rest.trim_start_matches(':').trim();

                // For each comma-separated declaration
                for decl in rest.split(',') {
                    let decl = decl.trim();
                    if let Some(name) = extract_table_declaration_name(decl) {
                        internal_tables.insert(name.to_lowercase());
                    }
                }
            }
        } else {
            // Check for continuation lines (indented lines that are part of DATA: declaration)
            // These typically start with the variable name directly
            if upper.contains("TYPE")
                && upper.contains("TABLE")
                && let Some(name) = extract_table_declaration_name(trimmed)
            {
                internal_tables.insert(name.to_lowercase());
            }
        }
    }

    internal_tables
}

/// Extract the variable name from a table declaration line.
/// Input examples:
/// - "`lt_data` TYPE TABLE OF zstructure"
/// - "`lt_data` TYPE STANDARD TABLE OF zstructure"
/// - "`<fs_data>` TYPE TABLE OF zstructure" (field symbols)
fn extract_table_declaration_name(decl: &str) -> Option<&str> {
    let upper = decl.to_uppercase();

    // Check if this declares a TABLE type
    if !upper.contains("TYPE")
        || (!upper.contains("TABLE OF")
            && !upper.contains("STANDARD TABLE")
            && !upper.contains("SORTED TABLE")
            && !upper.contains("HASHED TABLE"))
    {
        return None;
    }

    // Get the first token (the variable name)
    let parts: Vec<&str> = decl.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let name = parts[0];
    // Handle field symbols: <fs_name> -> fs_name
    let name = name.trim_start_matches('<').trim_end_matches('>');
    // Handle trailing punctuation
    let name = name.trim_end_matches([',', '.', ':']);

    if name.is_empty() {
        return None;
    }

    Some(name)
}

fn extract_inline_field_symbol_name(line: &str) -> Option<&str> {
    let start = line.find('<')?;
    let end = line[start + 1..].find('>')?;
    let name = &line[start + 1..start + 1 + end];
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Parse INSERT statement and extract table name.
/// Database table patterns (accepted):
///
/// - INSERT <dbtab> FROM <wa>.
/// - INSERT <dbtab> FROM TABLE <itab>.
/// - INSERT <dbtab> VALUES ...
///
/// Internal table patterns (rejected):
///
/// - INSERT <wa> INTO TABLE <itab>.
/// - INSERT <wa> INTO <itab> INDEX <n>.
/// - INSERT LINES OF `<itab_src>` INTO TABLE `<itab_dst>`.
/// - INSERT INITIAL LINE INTO TABLE <itab>.
///
/// Also rejects: INSERT REPORT, INSERT TEXTPOOL, identifiers matching internal table naming.
fn parse_insert_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("INSERT ") {
        return None;
    }

    let rest = original.get(7..)?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();

    if parts.is_empty() {
        return None;
    }

    let first = parts[0].trim_end_matches('.');
    let first_upper = first.to_uppercase();

    // Reject internal table patterns that start with special keywords
    // "INSERT LINES OF ...", "INSERT INITIAL LINE ...", "INSERT REPORT ...", "INSERT TEXTPOOL ..."
    if matches!(
        first_upper.as_str(),
        "LINES" | "INITIAL" | "REPORT" | "TEXTPOOL"
    ) {
        return None;
    }

    // Check for "INTO" keyword anywhere - indicates internal table operation
    // Pattern: INSERT <wa> INTO TABLE <itab> or INSERT <wa> INTO <itab> INDEX ...
    for (i, part) in parts.iter().enumerate() {
        if part.to_uppercase() == "INTO" {
            // "INTO TABLE" or "INTO <itab>" patterns are internal table operations
            return None;
        }
        // Stop checking after reasonable distance (avoid false positives from unrelated INTO)
        if i > 3 {
            break;
        }
    }

    // Database INSERT requires FROM or VALUES after table name
    // Pattern: INSERT <dbtab> FROM ... or INSERT <dbtab> VALUES ...
    if parts.len() > 1 {
        let second_upper = parts[1].to_uppercase();
        if matches!(second_upper.as_str(), "FROM" | "VALUES") {
            // Reject if the identifier looks like an internal table
            if is_likely_internal_table(first) {
                return None;
            }
            return Some(first.to_string());
        }
    }

    // No FROM/VALUES clause found - reject to avoid false positives
    // (could be internal table operation or malformed statement)
    None
}

/// Parse MODIFY statement and extract table name.
/// Patterns for database table operations:
///
/// - MODIFY `<table>` FROM `<work_area>`
/// - MODIFY <table> FROM TABLE <itab>
///
/// Internal table operations (rejected):
///
/// - MODIFY <itab> FROM <wa> INDEX <n>
/// - MODIFY <itab> FROM <wa> TRANSPORTING <fields>
/// - MODIFY TABLE <itab> FROM <wa>
///
/// Also rejects identifiers matching internal table naming conventions.
fn parse_modify_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("MODIFY ") {
        return None;
    }

    let rest = original.get(7..)?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();

    if parts.is_empty() {
        return None;
    }

    // "MODIFY TABLE <itab>" is internal table operation
    if parts[0].to_uppercase() == "TABLE" {
        return None;
    }

    // First token after MODIFY is the table name
    let table = parts[0].trim_end_matches('.');

    // Reject if the identifier looks like an internal table
    if is_likely_internal_table(table) {
        return None;
    }

    // Database table MODIFY requires FROM clause: MODIFY <dbtab> FROM ...
    // Internal table MODIFY uses INDEX, TRANSPORTING, WHERE, etc.
    if parts.len() > 1 {
        let next = parts[1].to_uppercase();
        // FROM indicates database table operation
        if next == "FROM" {
            return Some(table.to_string());
        }
        // INDEX, TRANSPORTING, WHERE indicate internal table operation
        if matches!(next.as_str(), "INDEX" | "TRANSPORTING" | "WHERE") {
            return None;
        }
    }

    // No FROM clause - likely internal table, reject to avoid false positives
    None
}

/// Parse UPDATE statement and extract table name.
/// Pattern: UPDATE <table> SET ...
/// UPDATE is always a database operation in ABAP (internal tables use MODIFY).
/// Still rejects identifiers matching internal table naming conventions as a safety check.
fn parse_update_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("UPDATE ") {
        return None;
    }

    let rest = original.get(7..)?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();

    if parts.is_empty() {
        return None;
    }

    // First token after UPDATE is the table name
    let table = parts[0].trim_end_matches('.');

    // Reject if the identifier looks like an internal table (safety check)
    if is_likely_internal_table(table) {
        return None;
    }

    Some(table.to_string())
}

/// Parse DELETE statement and extract table name.
/// Patterns for database table operations:
///
/// - DELETE FROM <table> WHERE ...
/// - DELETE <table> WHERE ... (database table with WHERE clause)
///
/// Internal table operations (rejected):
///
/// - DELETE <itab> INDEX <n>
/// - DELETE <itab> (without WHERE clause or FROM keyword)
/// - DELETE <itab>. (even with period - too ambiguous)
/// - DELETE TABLE <itab> FROM <wa>
/// - DELETE ADJACENT DUPLICATES FROM <itab>
///
/// Also rejects identifiers matching internal table naming conventions.
fn parse_delete_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("DELETE ") {
        return None;
    }

    let rest = original.get(7..)?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();

    if parts.is_empty() {
        return None;
    }

    let first_upper = parts[0].to_uppercase();

    // "DELETE TABLE <itab>" and "DELETE ADJACENT" are internal table operations
    if matches!(first_upper.as_str(), "TABLE" | "ADJACENT") {
        return None;
    }

    // Check for "DELETE FROM <table>" pattern - this is database table
    if first_upper == "FROM" && parts.len() > 1 {
        let table = parts[1].trim_end_matches('.');
        // Reject if the identifier looks like an internal table
        if is_likely_internal_table(table) {
            return None;
        }
        return Some(table.to_string());
    }

    // "DELETE <table> WHERE" is database table operation
    // "DELETE <itab> INDEX" is internal table operation
    if parts.len() > 1 {
        let next = parts[1].to_uppercase();
        if next == "WHERE" {
            let table = parts[0].trim_end_matches('.');
            // Reject if the identifier looks like an internal table
            if is_likely_internal_table(table) {
                return None;
            }
            return Some(table.to_string());
        }
        // INDEX indicates internal table operation
        if next == "INDEX" {
            return None;
        }
    }

    // REMOVED: The dangerous "DELETE <table>." heuristic
    // Single "DELETE <ident>." is too ambiguous - could be internal table deletion
    // which is very common in ABAP. Reject to avoid false positives.

    // Ambiguous case without FROM/WHERE - reject to avoid false positives
    None
}

/// Find the enclosing class for a byte range
fn find_enclosing_class(
    classes: &[AbapClass],
    start_byte: usize,
    end_byte: usize,
) -> Option<&AbapClass> {
    classes
        .iter()
        .filter(|c| c.start_byte <= start_byte && end_byte <= c.end_byte)
        .min_by_key(|c| c.end_byte.saturating_sub(c.start_byte))
}

/// Find the enclosing callable for a byte range
fn find_enclosing_callable(
    callables: &[AbapCallable],
    start_byte: usize,
    end_byte: usize,
) -> Option<&AbapCallable> {
    callables
        .iter()
        .filter(|c| c.start_byte <= start_byte && end_byte <= c.end_byte)
        .min_by_key(|c| c.end_byte.saturating_sub(c.start_byte))
}

/// Extract SUBMIT and CALL TRANSACTION statements and create Module nodes and Call edges
fn extract_program_calls(
    content: &[u8],
    callables: &[AbapCallable],
    helper: &mut GraphBuildHelper,
) {
    let Ok(content_str) = std::str::from_utf8(content) else {
        return;
    };

    let lines: Vec<&str> = content_str.lines().collect();

    // Precompute line start byte offsets
    let line_offsets: Vec<usize> = {
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        offsets.push(0);
        let mut offset = 0;
        for line in &lines {
            offset += line.len() + 1; // +1 for newline
            offsets.push(offset);
        }
        offsets
    };

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();
        let byte_offset = line_offsets[line_idx];
        let line_end = byte_offset + trimmed.len();

        // Parse SUBMIT statement: SUBMIT <program_name> ...
        if let Some(program_name) = parse_submit_statement(&upper, trimmed) {
            let span = span_from_line(line_idx, trimmed.len());
            let program_node = helper.add_module(&program_name, Some(span));

            // Find the enclosing callable and create call edge
            if let Some(caller) = find_enclosing_callable(callables, byte_offset, line_end) {
                helper.add_call_edge(caller.node_id, program_node);
            }
        }

        // Parse CALL TRANSACTION statement: CALL TRANSACTION '<tcode>' ...
        if let Some(tcode) = parse_call_transaction_statement(&upper, trimmed) {
            let span = span_from_line(line_idx, trimmed.len());
            // Prefix transaction code with TCODE_ to distinguish from regular programs
            let tcode_name = format!("TCODE_{tcode}");
            let tcode_node = helper.add_module(&tcode_name, Some(span));

            // Find the enclosing callable and create call edge
            if let Some(caller) = find_enclosing_callable(callables, byte_offset, line_end) {
                helper.add_call_edge(caller.node_id, tcode_node);
            }
        }
    }
}

/// Parse SUBMIT statement and extract program name
/// Examples: SUBMIT `z_program` AND RETURN.
///           SUBMIT `z_program` VIA SELECTION-SCREEN.
fn parse_submit_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("SUBMIT ") {
        return None;
    }

    let rest = original.get(7..)?.trim();
    let program_name = rest.split_whitespace().next()?.trim_end_matches('.');

    if program_name.is_empty() || is_abap_keyword(program_name) {
        return None;
    }

    Some(program_name.to_string())
}

/// Parse CALL TRANSACTION statement and extract transaction code
/// Examples: CALL TRANSACTION 'VA01' USING bdcdata.
///           CALL TRANSACTION 'SE38'.
fn parse_call_transaction_statement(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("CALL TRANSACTION ") {
        return None;
    }

    let rest = original.get(17..)?.trim();

    // Extract transaction code from quotes
    if let Some(start) = rest.find('\'')
        && let Some(end) = rest[start + 1..].find('\'')
    {
        let tcode = &rest[start + 1..start + 1 + end];
        if !tcode.is_empty() {
            return Some(tcode.to_string());
        }
    }

    None
}

/// Extract import edges from INCLUDE statements (grammar-based) and TYPE-POOLS
/// statements (text-based fallback).
///
/// REPORT statements are intentionally excluded: they declare the current program
/// header rather than importing another module, and would create noisy self-edges.
fn extract_imports(
    tree: &Tree,
    content: &[u8],
    queries: &AbapQueries,
    callables: &[AbapCallable],
    helper: &mut GraphBuildHelper,
    module_id: NodeId,
) {
    // Part A: Grammar-based INCLUDE extraction
    let mut cursor = QueryCursor::new();
    let capture_names = queries.includes.capture_names();
    let mut matches = cursor.matches(&queries.includes, tree.root_node(), content);

    while let Some(m) = matches.next() {
        let mut include_name = None;
        let mut include_node = None;

        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            match name {
                "include.name" => {
                    if let Ok(text) = capture.node.utf8_text(content) {
                        let text = text.trim();
                        if !text.is_empty() && is_valid_abap_identifier(text) {
                            include_name = Some(text.to_string());
                        }
                    }
                }
                "include.node" => {
                    include_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(name), Some(node)) = (include_name, include_node) {
            let span = span_from_node(&node);
            let start_byte = node.start_byte();
            let end_byte = node.end_byte();

            // Determine importer: enclosing callable or module
            // Use enclosing callable as importer, or module for file-level
            let importer_id =
                if let Some(callable) = find_enclosing_callable(callables, start_byte, end_byte) {
                    callable.node_id
                } else {
                    module_id
                };

            let to_id = helper.add_import(&name, Some(span));
            helper.add_import_edge(importer_id, to_id);
        }
    }

    // Part B: Text-based TYPE-POOLS extraction
    let Ok(content_str) = std::str::from_utf8(content) else {
        return;
    };

    let lines: Vec<&str> = content_str.lines().collect();

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();

        let pool_names = parse_type_pools_statement(&upper, trimmed);
        for pool_name in pool_names {
            let span = span_from_line(line_idx, trimmed.len());
            // TYPE-POOLS is always file-level, so importer is module_id
            let to_id = helper.add_import(&pool_name, Some(span));
            helper.add_import_edge(module_id, to_id);
        }
    }
}

/// Parse TYPE-POOLS statement and extract pool names.
///
/// Handles both simple and colon notation:
/// - `TYPE-POOLS slis.` → `vec!["slis"]`
/// - `TYPE-POOLS: slis, abap.` → `vec!["slis", "abap"]`
///
/// Source spelling is preserved (no case conversion).
fn parse_type_pools_statement(upper: &str, original: &str) -> Vec<String> {
    // TYPE-POOLS with optional colon
    if !upper.starts_with("TYPE-POOLS") {
        return Vec::new();
    }

    let Some(rest) = original.get(10..) else {
        return Vec::new();
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Vec::new();
    }

    // Check for colon notation: TYPE-POOLS: name1, name2.
    let rest = if let Some(stripped) = rest.strip_prefix(':') {
        stripped.trim()
    } else if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '/') {
        // Simple form: TYPE-POOLS name.
        rest
    } else {
        return Vec::new();
    };

    // Split by commas for colon notation, or take single name
    let mut names = Vec::new();
    for part in rest.split(',') {
        let name = part.trim().trim_end_matches('.');
        if !name.is_empty() && is_valid_abap_identifier(name) && !is_abap_keyword(name) {
            names.push(name.to_string());
        }
    }

    names
}

/// Create a Span from a tree-sitter node
fn span_from_node(node: &Node) -> Span {
    Span::new(
        Position::new(node.start_position().row, node.start_position().column),
        Position::new(node.end_position().row, node.end_position().column),
    )
}

/// Create a Span from line index and length
fn span_from_line(line_idx: usize, len: usize) -> Span {
    Span::new(Position::new(line_idx, 0), Position::new(line_idx, len))
}

fn parse_method_declaration_line(line: &str) -> Option<String> {
    let upper = line.to_uppercase();
    if !upper.starts_with("METHOD ") {
        return None;
    }
    let rest = line.get(6..)?.trim();
    let name = rest.split_whitespace().next()?.trim_end_matches('.');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Check if a string is an ABAP keyword (not a table name)
fn is_abap_keyword(name: &str) -> bool {
    let upper = name.to_uppercase();
    matches!(
        upper.as_str(),
        "SELECT"
            | "FROM"
            | "WHERE"
            | "INTO"
            | "TABLE"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "MODIFY"
            | "SET"
            | "AND"
            | "OR"
            | "NOT"
            | "FOR"
            | "ALL"
            | "ENTRIES"
            | "IN"
            | "DATA"
            | "TYPE"
            | "VALUE"
            | "CORRESPONDING"
            | "FIELDS"
            | "OF"
            | "APPENDING"
            | "UP"
            | "TO"
            | "ROWS"
            | "SINGLE"
            | "DISTINCT"
            | "ORDER"
            | "BY"
            | "GROUP"
            | "HAVING"
            | "JOIN"
            | "ON"
            | "LEFT"
            | "RIGHT"
            | "OUTER"
            | "INNER"
            | "CONNECTION"
    )
}

/// Check if a string is a valid ABAP identifier
fn is_valid_abap_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() > 30 {
        return false;
    }

    let first = s.chars().next().unwrap();
    if !first.is_alphabetic() && first != '_' && first != '/' {
        return false;
    }

    s.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '/')
}

/// Check if identifier looks like an internal table based on ABAP naming conventions.
/// Common prefixes for internal tables:
/// - lt_ (local table)
/// - gt_ (global table)
/// - it_ (importing table)
/// - et_ (exporting table)
/// - ct_ (changing table)
/// - mt_ (member/instance table)
/// - pt_ (parameter table)
/// - rt_ (returning table)
/// - t_ (generic table prefix)
fn is_likely_internal_table(name: &str) -> bool {
    let lower = name.to_lowercase();

    // Common internal table prefixes (case-insensitive)
    let internal_table_prefixes = [
        "lt_", "gt_", "it_", "et_", "ct_", "mt_", "pt_", "rt_", "t_", "lit_",
        "git_", // Less common variants
    ];

    for prefix in &internal_table_prefixes {
        if lower.starts_with(prefix) {
            return true;
        }
    }

    // Also check for common suffixes that indicate internal tables
    // E.g., "_tab", "_itab", "_table"
    if lower.ends_with("_tab") || lower.ends_with("_itab") || lower.ends_with("_table") {
        return true;
    }

    false
}

/// Extract visibility for an ABAP method or function.
///
/// In ABAP, methods can be in PUBLIC, PRIVATE, or PROTECTED sections,
/// but since the current implementation doesn't parse class structure,
/// we treat all methods/functions as public by default.
fn extract_visibility(_name: &str) -> &'static str {
    "public"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse_abap(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_abap_sqry::language())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    #[test]
    fn test_creates_module_node() {
        let source = r"
CLASS zcl_test DEFINITION PUBLIC.
  PUBLIC SECTION.
    METHODS test_method.
ENDCLASS.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("zcl_test.abap");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph without errors");
    }

    #[test]
    fn test_graph_builder_language() {
        let builder = AbapGraphBuilder::new();
        assert_eq!(builder.language(), Language::Abap);
    }

    #[test]
    fn test_empty_file() {
        let source = "";
        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("empty.abap");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should handle empty file");
    }

    #[test]
    fn test_is_abap_keyword() {
        assert!(is_abap_keyword("SELECT"));
        assert!(is_abap_keyword("select"));
        assert!(is_abap_keyword("FROM"));
        assert!(is_abap_keyword("INSERT"));
        assert!(is_abap_keyword("MODIFY"));
        assert!(is_abap_keyword("UPDATE"));
        assert!(is_abap_keyword("DELETE"));
        assert!(!is_abap_keyword("ztable"));
        assert!(!is_abap_keyword("customers"));
    }

    #[test]
    fn test_is_valid_abap_identifier() {
        assert!(is_valid_abap_identifier("ztable"));
        assert!(is_valid_abap_identifier("CUSTOMERS"));
        assert!(is_valid_abap_identifier("Z_MY_TABLE"));
        assert!(is_valid_abap_identifier("/namespace/table"));
        assert!(!is_valid_abap_identifier("")); // Empty
        assert!(!is_valid_abap_identifier("123abc")); // Starts with digit
    }

    #[test]
    fn test_parse_insert_statement() {
        // Basic INSERT
        let upper = "INSERT ZTABLE FROM TABLE @LT_DATA.";
        let orig = "INSERT ztable FROM TABLE @lt_data.";
        let result = parse_insert_statement(upper, orig);
        assert_eq!(result, Some("ztable".to_string()));

        // Not an INSERT
        let upper = "SELECT * FROM ZTABLE.";
        let orig = "SELECT * FROM ztable.";
        assert!(parse_insert_statement(upper, orig).is_none());
    }

    #[test]
    fn test_parse_modify_statement() {
        // Basic MODIFY
        let upper = "MODIFY ZTABLE FROM TABLE @LT_DATA.";
        let orig = "MODIFY ztable FROM TABLE @lt_data.";
        let result = parse_modify_statement(upper, orig);
        assert_eq!(result, Some("ztable".to_string()));

        // Not a MODIFY
        let upper = "SELECT * FROM ZTABLE.";
        let orig = "SELECT * FROM ztable.";
        assert!(parse_modify_statement(upper, orig).is_none());
    }

    #[test]
    fn test_parse_update_statement() {
        // Basic UPDATE
        let upper = "UPDATE ZTABLE SET FIELD = VALUE WHERE ID = 1.";
        let orig = "UPDATE ztable SET field = value WHERE id = 1.";
        let result = parse_update_statement(upper, orig);
        assert_eq!(result, Some("ztable".to_string()));

        // Not an UPDATE
        let upper = "SELECT * FROM ZTABLE.";
        let orig = "SELECT * FROM ztable.";
        assert!(parse_update_statement(upper, orig).is_none());
    }

    #[test]
    fn test_parse_delete_statement() {
        // DELETE FROM pattern
        let upper = "DELETE FROM ZTABLE WHERE ID = 1.";
        let orig = "DELETE FROM ztable WHERE id = 1.";
        let result = parse_delete_statement(upper, orig);
        assert_eq!(result, Some("ztable".to_string()));

        // DELETE without FROM
        let upper = "DELETE ZTABLE WHERE ID = 1.";
        let orig = "DELETE ztable WHERE id = 1.";
        let result = parse_delete_statement(upper, orig);
        assert_eq!(result, Some("ztable".to_string()));

        // Not a DELETE
        let upper = "SELECT * FROM ZTABLE.";
        let orig = "SELECT * FROM ztable.";
        assert!(parse_delete_statement(upper, orig).is_none());
    }

    #[test]
    fn test_extract_table_writes() {
        let content = br"
CLASS zcl_test IMPLEMENTATION.
  METHOD test_method.
    INSERT ztable FROM TABLE @lt_data.
    MODIFY customers FROM @ls_customer.
    UPDATE orders SET status = 'COMPLETED' WHERE id = 1.
    DELETE FROM partners WHERE inactive = 'X'.
  ENDMETHOD.
ENDCLASS.
";

        let ops = extract_table_writes(content);

        assert_eq!(ops.len(), 4, "Should extract 4 write operations");

        // Verify INSERT
        let insert = ops.iter().find(|o| o.kind == TableOpKind::Insert);
        assert!(insert.is_some(), "Should find INSERT");
        assert_eq!(insert.unwrap().table_name, "ztable");

        // Verify MODIFY
        let modify = ops.iter().find(|o| o.kind == TableOpKind::Modify);
        assert!(modify.is_some(), "Should find MODIFY");
        assert_eq!(modify.unwrap().table_name, "customers");

        // Verify UPDATE
        let update = ops.iter().find(|o| o.kind == TableOpKind::Update);
        assert!(update.is_some(), "Should find UPDATE");
        assert_eq!(update.unwrap().table_name, "orders");

        // Verify DELETE
        let delete = ops.iter().find(|o| o.kind == TableOpKind::Delete);
        assert!(delete.is_some(), "Should find DELETE");
        assert_eq!(delete.unwrap().table_name, "partners");
    }

    #[test]
    fn test_select_statement_with_method() {
        let source = r"
CLASS zcl_data IMPLEMENTATION.
  METHOD get_customers.
    SELECT * FROM zcustomers INTO TABLE @DATA(lt_result).
  ENDMETHOD.
ENDCLASS.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("zcl_data.abap");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(result.is_ok(), "Should build graph for SELECT statement");

        // The staging graph should have nodes and potentially edges
        let stats = staging.stats();
        // At minimum we should have method nodes
        assert!(stats.nodes_staged > 0, "Should have staged some nodes");
    }

    #[test]
    fn test_multiple_table_operations() {
        let source = r"
CLASS zcl_crud IMPLEMENTATION.
  METHOD process_data.
    SELECT * FROM zmaster INTO TABLE @DATA(lt_master).
    INSERT zdetail FROM TABLE @lt_detail.
    MODIFY zstatus FROM @ls_status.
    UPDATE zlog SET processed = 'X' WHERE batch = @lv_batch.
    DELETE FROM ztemp WHERE created < @lv_date.
  ENDMETHOD.
ENDCLASS.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("zcl_crud.abap");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(
            result.is_ok(),
            "Should handle multiple table operations: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_function_with_table_access() {
        let source = r"
FUNCTION z_get_data.
  SELECT * FROM zconfig INTO TABLE @DATA(lt_config).
  INSERT zresult FROM TABLE @lt_result.
ENDFUNCTION.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("z_get_data.abap");

        let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
        assert!(
            result.is_ok(),
            "Should handle function with table access: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_namespaced_table() {
        let content = br"
METHOD test.
  SELECT * FROM /namespace/table INTO TABLE @lt_data.
  INSERT /custom/orders FROM @ls_order.
ENDMETHOD.
";

        let ops = extract_table_writes(content);

        // Namespaced INSERT
        let insert = ops.iter().find(|o| o.kind == TableOpKind::Insert);
        assert!(insert.is_some(), "Should find namespaced INSERT");
        assert_eq!(insert.unwrap().table_name, "/custom/orders");
    }

    #[test]
    fn test_is_likely_internal_table() {
        // Common internal table prefixes
        assert!(is_likely_internal_table("lt_data"));
        assert!(is_likely_internal_table("LT_CUSTOMERS"));
        assert!(is_likely_internal_table("gt_global"));
        assert!(is_likely_internal_table("it_import"));
        assert!(is_likely_internal_table("et_export"));
        assert!(is_likely_internal_table("ct_changing"));
        assert!(is_likely_internal_table("mt_member"));
        assert!(is_likely_internal_table("pt_params"));
        assert!(is_likely_internal_table("rt_result"));
        assert!(is_likely_internal_table("t_data"));

        // Common suffixes
        assert!(is_likely_internal_table("customers_tab"));
        assert!(is_likely_internal_table("orders_itab"));
        assert!(is_likely_internal_table("data_table"));

        // Database tables should NOT match
        assert!(!is_likely_internal_table("ztable"));
        assert!(!is_likely_internal_table("ZCUSTOMERS"));
        assert!(!is_likely_internal_table("mara"));
        assert!(!is_likely_internal_table("vbak"));
        assert!(!is_likely_internal_table("/namespace/table"));
    }

    #[test]
    fn test_internal_table_operations_rejected() {
        // Internal table DELETE operations should be rejected
        let upper = "DELETE LT_ITEMS.";
        let orig = "DELETE lt_items.";
        assert!(
            parse_delete_statement(upper, orig).is_none(),
            "DELETE lt_items. should be rejected (internal table)"
        );

        let upper = "DELETE LT_ITEMS WHERE AMOUNT > 100.";
        let orig = "DELETE lt_items WHERE amount > 100.";
        assert!(
            parse_delete_statement(upper, orig).is_none(),
            "DELETE lt_items WHERE ... should be rejected (internal table prefix)"
        );

        // Internal table MODIFY operations should be rejected
        let upper = "MODIFY LT_ITEMS FROM LS_ITEM.";
        let orig = "MODIFY lt_items FROM ls_item.";
        assert!(
            parse_modify_statement(upper, orig).is_none(),
            "MODIFY lt_items FROM ... should be rejected (internal table prefix)"
        );

        let upper = "MODIFY TABLE LT_ITEMS FROM LS_ITEM.";
        let orig = "MODIFY TABLE lt_items FROM ls_item.";
        assert!(
            parse_modify_statement(upper, orig).is_none(),
            "MODIFY TABLE lt_items ... should be rejected"
        );

        // Internal table INSERT operations should be rejected
        let upper = "INSERT LT_ITEMS FROM LS_ITEM.";
        let orig = "INSERT lt_items FROM ls_item.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT lt_items ... should be rejected (internal table prefix)"
        );

        // Database table operations should still be accepted
        let upper = "DELETE FROM ZTABLE WHERE ID = 1.";
        let orig = "DELETE FROM ztable WHERE id = 1.";
        assert_eq!(
            parse_delete_statement(upper, orig),
            Some("ztable".to_string()),
            "DELETE FROM ztable WHERE ... should be accepted (database table)"
        );

        let upper = "MODIFY ZCUSTOMERS FROM @LS_CUSTOMER.";
        let orig = "MODIFY zcustomers FROM @ls_customer.";
        assert_eq!(
            parse_modify_statement(upper, orig),
            Some("zcustomers".to_string()),
            "MODIFY zcustomers FROM ... should be accepted (database table)"
        );
    }

    #[test]
    fn test_ambiguous_delete_rejected() {
        // Single DELETE <ident>. without FROM or WHERE should be rejected
        // as it's too ambiguous (could be internal table)
        let upper = "DELETE ZTABLE.";
        let orig = "DELETE ztable.";
        assert!(
            parse_delete_statement(upper, orig).is_none(),
            "DELETE ztable. (without FROM/WHERE) should be rejected as ambiguous"
        );

        // DELETE <ident> INDEX <n> is internal table operation
        let upper = "DELETE LT_ITEMS INDEX 5.";
        let orig = "DELETE lt_items INDEX 5.";
        assert!(
            parse_delete_statement(upper, orig).is_none(),
            "DELETE lt_items INDEX ... should be rejected (internal table)"
        );
    }

    #[test]
    fn test_extract_table_writes_filters_internal_tables() {
        let content = br#"
METHOD process.
  " Database operations - should be extracted
  INSERT zcustomers FROM @ls_customer.
  MODIFY zorders FROM @ls_order.
  UPDATE zlog SET processed = 'X' WHERE id = 1.
  DELETE FROM ztemp WHERE created < @lv_date.

  " Internal table operations - should be filtered out
  INSERT lt_local FROM ls_item.
  MODIFY gt_global FROM ls_entry.
  DELETE lt_items WHERE amount > 100.
ENDMETHOD.
"#;

        let ops = extract_table_writes(content);

        // Should only have database table operations
        let table_names: Vec<&str> = ops.iter().map(|o| o.table_name.as_str()).collect();

        assert!(
            table_names.contains(&"zcustomers"),
            "Should extract zcustomers"
        );
        assert!(table_names.contains(&"zorders"), "Should extract zorders");
        assert!(table_names.contains(&"zlog"), "Should extract zlog");
        assert!(table_names.contains(&"ztemp"), "Should extract ztemp");

        // Should NOT have internal table operations
        assert!(
            !table_names.contains(&"lt_local"),
            "Should NOT extract lt_local (internal table)"
        );
        assert!(
            !table_names.contains(&"gt_global"),
            "Should NOT extract gt_global (internal table)"
        );
        assert!(
            !table_names.contains(&"lt_items"),
            "Should NOT extract lt_items (internal table)"
        );

        assert_eq!(
            ops.len(),
            4,
            "Should have exactly 4 database table operations"
        );
    }

    #[test]
    fn test_insert_internal_table_patterns_rejected() {
        // Internal table insert patterns should be rejected

        // Pattern: INSERT <wa> INTO TABLE <itab>
        let upper = "INSERT LS_ITEM INTO TABLE LT_ITEMS.";
        let orig = "INSERT ls_item INTO TABLE lt_items.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT <wa> INTO TABLE <itab> should be rejected"
        );

        // Pattern: INSERT INITIAL LINE INTO TABLE <itab>
        let upper = "INSERT INITIAL LINE INTO TABLE LT_ITEMS.";
        let orig = "INSERT INITIAL LINE INTO TABLE lt_items.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT INITIAL LINE INTO TABLE should be rejected"
        );

        // Pattern: INSERT LINES OF <itab_src> INTO TABLE <itab_dst>
        let upper = "INSERT LINES OF LT_SRC INTO TABLE LT_DST.";
        let orig = "INSERT LINES OF lt_src INTO TABLE lt_dst.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT LINES OF ... INTO TABLE should be rejected"
        );

        // Pattern: INSERT <wa> INTO <itab> INDEX <n>
        let upper = "INSERT LS_ITEM INTO LT_ITEMS INDEX 5.";
        let orig = "INSERT ls_item INTO lt_items INDEX 5.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT <wa> INTO <itab> INDEX should be rejected"
        );

        // Valid database inserts should still work
        let upper = "INSERT ZTABLE FROM @LS_ROW.";
        let orig = "INSERT ztable FROM @ls_row.";
        assert_eq!(
            parse_insert_statement(upper, orig),
            Some("ztable".to_string()),
            "INSERT <dbtab> FROM ... should be accepted"
        );

        let upper = "INSERT ZTABLE FROM TABLE @LT_ROWS.";
        let orig = "INSERT ztable FROM TABLE @lt_rows.";
        assert_eq!(
            parse_insert_statement(upper, orig),
            Some("ztable".to_string()),
            "INSERT <dbtab> FROM TABLE ... should be accepted"
        );
    }

    #[test]
    fn test_insert_requires_from_or_values() {
        // INSERT without FROM or VALUES should be rejected
        let upper = "INSERT ZTABLE.";
        let orig = "INSERT ztable.";
        assert!(
            parse_insert_statement(upper, orig).is_none(),
            "INSERT without FROM/VALUES should be rejected"
        );

        // INSERT with FROM should be accepted
        let upper = "INSERT ZTABLE FROM @LS_DATA.";
        let orig = "INSERT ztable FROM @ls_data.";
        assert!(
            parse_insert_statement(upper, orig).is_some(),
            "INSERT with FROM should be accepted"
        );

        // INSERT with VALUES should be accepted (if ABAP ever supports it)
        let upper = "INSERT ZTABLE VALUES @LS_DATA.";
        let orig = "INSERT ztable VALUES @ls_data.";
        assert!(
            parse_insert_statement(upper, orig).is_some(),
            "INSERT with VALUES should be accepted"
        );
    }

    #[test]
    fn test_extract_declared_internal_tables() {
        let content = r#"
DATA lt_customers TYPE TABLE OF zcustomer.
DATA gt_orders TYPE STANDARD TABLE OF zorder.
DATA: lt_items TYPE SORTED TABLE OF zitem,
      lt_logs TYPE HASHED TABLE OF zlog.
FIELD-SYMBOLS <fs_data> TYPE TABLE OF zdata.
TYPES: ty_items TYPE TABLE OF zitem.

" Not internal tables
DATA lv_count TYPE i.
DATA ls_customer TYPE zcustomer.
"#;

        let internal_tables = extract_declared_internal_tables(content);

        assert!(
            internal_tables.contains("lt_customers"),
            "Should find lt_customers"
        );
        assert!(
            internal_tables.contains("gt_orders"),
            "Should find gt_orders"
        );
        assert!(internal_tables.contains("lt_items"), "Should find lt_items");
        assert!(internal_tables.contains("lt_logs"), "Should find lt_logs");
        assert!(
            internal_tables.contains("fs_data"),
            "Should find fs_data (field symbol)"
        );
        assert!(
            internal_tables.contains("ty_items"),
            "Should find ty_items (type)"
        );

        // Should NOT contain non-table declarations
        assert!(
            !internal_tables.contains("lv_count"),
            "Should NOT contain lv_count"
        );
        assert!(
            !internal_tables.contains("ls_customer"),
            "Should NOT contain ls_customer"
        );
    }

    #[test]
    fn test_declaration_based_filtering() {
        // Test that declared internal tables are filtered even without naming prefixes
        let content = br#"
METHOD process.
  DATA customers TYPE TABLE OF zcustomer.
  DATA orders TYPE STANDARD TABLE OF zorder.

  " This should be filtered because 'customers' is declared as internal table
  INSERT customers FROM @ls_cust.
  MODIFY orders FROM @ls_ord.

  " This should be accepted - real database table
  INSERT zcustomers FROM @ls_cust.
  MODIFY zorders FROM @ls_ord.
ENDMETHOD.
"#;

        let ops = extract_table_writes(content);
        let table_names: Vec<&str> = ops.iter().map(|o| o.table_name.as_str()).collect();

        // Database tables should be found
        assert!(
            table_names.contains(&"zcustomers"),
            "Should extract zcustomers (database table)"
        );
        assert!(
            table_names.contains(&"zorders"),
            "Should extract zorders (database table)"
        );

        // Declared internal tables should NOT be found
        assert!(
            !table_names.contains(&"customers"),
            "Should NOT extract customers (declared as internal table)"
        );
        assert!(
            !table_names.contains(&"orders"),
            "Should NOT extract orders (declared as internal table)"
        );

        assert_eq!(
            ops.len(),
            2,
            "Should have exactly 2 database table operations"
        );
    }

    #[test]
    fn test_field_symbols_colon_notation() {
        // Test that FIELD-SYMBOLS: (colon notation) is handled correctly
        // This addresses the non-blocking recommendation from Codex iter7 review
        let content = r"
FIELD-SYMBOLS: <fs_data> TYPE TABLE OF zdata,
               <fs_items> TYPE STANDARD TABLE OF zitem.
FIELD-SYMBOLS <fs_single> TYPE SORTED TABLE OF zlog.
FIELD-SYMBOLS: <fs_orders> TYPE HASHED TABLE OF zorder.
";

        let internal_tables = extract_declared_internal_tables(content);

        // All field symbols should be found regardless of colon notation
        assert!(
            internal_tables.contains("fs_data"),
            "Should find fs_data from FIELD-SYMBOLS: colon notation"
        );
        assert!(
            internal_tables.contains("fs_items"),
            "Should find fs_items from FIELD-SYMBOLS: colon notation (second item)"
        );
        assert!(
            internal_tables.contains("fs_single"),
            "Should find fs_single from FIELD-SYMBOLS without colon"
        );
        assert!(
            internal_tables.contains("fs_orders"),
            "Should find fs_orders from FIELD-SYMBOLS: colon notation (single item)"
        );
        assert_eq!(
            internal_tables.len(),
            4,
            "Should have exactly 4 field symbols"
        );
    }

    #[test]
    fn test_field_symbol_inline_declaration() {
        let content = r"
FIELD-SYMBOL(<fs_inline>) TYPE TABLE OF zdata.
ASSIGN lt_data TO FIELD-SYMBOL(<fs_ref>).
";

        let internal_tables = extract_declared_internal_tables(content);

        assert!(
            internal_tables.contains("fs_inline"),
            "Should find fs_inline from inline FIELD-SYMBOL declaration"
        );
        assert!(
            !internal_tables.contains("fs_ref"),
            "Should not treat ASSIGN field symbol as table declaration"
        );
    }

    // ====================================================================
    // Import edge tests (INCLUDE and TYPE-POOLS)
    // ====================================================================

    // Import tests use shared test helpers from sqry-core
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_import_edge, collect_call_edges, collect_import_edges,
    };

    #[test]
    fn test_include_creates_import_edges() {
        let source = r"
INCLUDE zcl_utils.
INCLUDE zbc_macros.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
        assert_has_import_edge(&staging, "ztest", "zcl_utils");
        assert_has_import_edge(&staging, "ztest", "zbc_macros");
    }

    #[test]
    fn test_include_inside_method() {
        let source = r"
CLASS zcl_test IMPLEMENTATION.
  METHOD setup.
    INCLUDE zcl_setup_macros.
  ENDMETHOD.
ENDCLASS.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("zcl_test.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge");
        // Import should be from the enclosing method, not module
        assert_has_import_edge(&staging, "setup", "zcl_setup_macros");
    }

    #[test]
    fn test_include_if_found() {
        let source = r"
INCLUDE zopt_module IF FOUND.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge for IF FOUND");
        assert_has_import_edge(&staging, "ztest", "zopt_module");
    }

    #[test]
    fn test_type_pools_creates_import_edges() {
        let source = r"
TYPE-POOLS slis.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge");
        assert_has_import_edge(&staging, "ztest", "slis");
    }

    #[test]
    fn test_type_pools_colon_notation() {
        let source = r"
TYPE-POOLS: slis, abap.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
        assert_has_import_edge(&staging, "ztest", "slis");
        assert_has_import_edge(&staging, "ztest", "abap");
    }

    #[test]
    fn test_type_pools_multiline_colon() {
        // TYPE-POOLS with colon notation split across what parses as one line
        let source = "TYPE-POOLS: slis,\n            icon.\n";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // At minimum, slis should be captured from the first line
        let imports = collect_import_edges(&staging);
        assert!(
            !imports.is_empty(),
            "Expected at least 1 import edge from multiline TYPE-POOLS"
        );
        assert_has_import_edge(&staging, "ztest", "slis");
    }

    #[test]
    fn test_import_preserves_source_case() {
        let source = r"
INCLUDE ZCL_UTILS.
TYPE-POOLS SLIS.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Names should preserve source spelling
        assert_has_import_edge(&staging, "ztest", "ZCL_UTILS");
        assert_has_import_edge(&staging, "ztest", "SLIS");
    }

    #[test]
    fn test_report_does_not_create_import_edge() {
        let source = r"
REPORT z_main.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("z_main.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // REPORT should NOT create import edges (it's a program header, not an import)
        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 0, "REPORT should not create import edges");
    }

    #[test]
    fn test_import_does_not_create_call_edge() {
        let source = r"
INCLUDE zcl_utils.
TYPE-POOLS slis.
";

        let tree = parse_abap(source);
        let mut staging = StagingGraph::new();
        let builder = AbapGraphBuilder;
        let file = PathBuf::from("ztest.abap");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // INCLUDE and TYPE-POOLS should create import edges, NOT call edges
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            0,
            "INCLUDE/TYPE-POOLS should not create call edges"
        );

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
    }
}
