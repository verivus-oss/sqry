// Nested conditionals kept for readability in PL/SQL traversal

//! Oracle PL/SQL `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts relationships from PL/SQL code:
//! - Package modules and procedure/function nodes
//! - Cross-package calls (`other_pkg.procedure()`)
//! - Table access via DML statements
//!
//! ## Grammar Limitations
//!
//! The tree-sitter-plsql grammar is designed primarily for PACKAGE and PACKAGE BODY
//! constructs. This implementation extracts what the grammar supports and provides
//! a foundation for enhanced extraction when the grammar is improved.

use std::collections::HashSet;
use std::path::Path;

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::GraphBuildHelper;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::TableWriteOp;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use std::sync::OnceLock;
use tree_sitter::{Node, Tree};

use super::type_extractor;

/// `GraphBuilder` for Oracle PL/SQL files.
#[derive(Debug, Default)]
pub struct OraclePlsqlGraphBuilder;

impl OraclePlsqlGraphBuilder {
    /// Create a new Oracle PL/SQL graph builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for OraclePlsqlGraphBuilder {
    #[allow(clippy::similar_names)]
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Plsql);
        let module_id = helper.add_module("<module>", None);
        let mut package_stack: Vec<PackageContext> = Vec::new();
        let mut table_edges_seen: HashSet<TableEdgeKey> = HashSet::new();

        // Create recursion guard
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
        let mut recursion_guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to create recursion guard: {e}"),
        })?;

        let mut callables: Vec<(NodeId, usize, usize)> = Vec::new();

        let mut context = WalkContext {
            content,
            helper: &mut helper,
            file_module: module_id,
            package_stack: &mut package_stack,
            table_edges_seen: &mut table_edges_seen,
            guard: &mut recursion_guard,
            callables: &mut callables,
        };

        walk_node(tree.root_node(), None, &mut context).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;
        extract_table_edges_from_text(content, &mut helper, module_id, &mut table_edges_seen);
        extract_typeof_edges_from_text(content, &mut helper, module_id, &callables);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Plsql
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(plsql_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Oracle PL/SQL (tree-sitter-plsql-sqry).
///
/// PL/SQL procedures and functions carry real procedural control flow that the
/// grammar parses fully (`if_statement`, the loop family, `case_statement`,
/// exception handling, `raise_statement`, cursor fetch/open/close), so the
/// body-shape descriptor counts genuine buckets. The mapping is built once from
/// the grammar and shared process-wide.
pub struct PlsqlShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl PlsqlShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_plsql_sqry::language();
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
                *slot = cf_bucket_for_plsql_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for PlsqlShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // The argument list is a `parameter_declaration` child holding
        // `parameter_declaration_element` entries; a `default_expression` child
        // on an element marks a default value.
        let mut cursor = fn_node.walk();
        for child in fn_node.named_children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                let mut elem_cursor = child.walk();
                for elem in child.named_children(&mut elem_cursor) {
                    if elem.kind() == "parameter_declaration_element" {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                        let mut def_cursor = elem.walk();
                        for piece in elem.named_children(&mut def_cursor) {
                            if piece.kind() == "default_expression" {
                                shape.has_defaults = true;
                            }
                        }
                    }
                }
            }
        }
        // A `function_definition`/`function_declaration` carries a
        // `return_declaration`; its presence is the return-type signal.
        let mut ret_cursor = fn_node.walk();
        for child in fn_node.named_children(&mut ret_cursor) {
            if child.kind() == "return_declaration" {
                shape.has_return_annotation = true;
            }
        }
        shape
    }
}

/// Map one tree-sitter-plsql-sqry grammar node-kind name to its canonical
/// control-flow bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_plsql_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" => CfBucket::Branch,
        "case_statement" => CfBucket::Match,
        "basic_loop_statement" | "for_loop_statement" | "while_loop_statement" => CfBucket::Loop,
        "exit_statement" | "continue_statement" => CfBucket::BreakContinue,
        // PL/SQL has no separate `try`; the exception block is the catch surface.
        "exception_block" | "exception_handler" => CfBucket::Catch,
        "raise_statement" => CfBucket::Throw,
        "return_statement" => CfBucket::Return,
        "pipe_row_statement" => CfBucket::Yield,
        "assignment_statement" => CfBucket::Assign,
        // Cursor and dynamic-statement surfaces map onto Call.
        "ref_call" | "execute_immediate" | "open_statement" | "open_for_statement"
        | "fetch_statement" | "close_statement" => CfBucket::Call,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide PL/SQL shape mapping, built once on first use.
#[must_use]
pub fn plsql_shape_mapping() -> &'static PlsqlShapeMapping {
    static MAPPING: OnceLock<PlsqlShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(PlsqlShapeMapping::build)
}

#[derive(Debug, Clone)]
struct PackageContext {
    name: String,
    module_id: NodeId,
}

struct WalkContext<'a, 'b> {
    content: &'a [u8],
    helper: &'a mut GraphBuildHelper<'b>,
    file_module: NodeId,
    package_stack: &'a mut Vec<PackageContext>,
    table_edges_seen: &'a mut HashSet<TableEdgeKey>,
    guard: &'a mut sqry_core::query::security::RecursionGuard,
    callables: &'a mut Vec<(NodeId, usize, usize)>,
}

/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
fn walk_node(
    node: Node<'_>,
    current_callable: Option<NodeId>,
    context: &mut WalkContext<'_, '_>,
) -> Result<(), String> {
    context
        .guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;
    let mut next_callable = current_callable;
    let mut pushed_package = false;

    if is_package_node(node.kind())
        && let Some(package_name) = extract_name_from_children(node, context.content)
    {
        let package_module = context
            .helper
            .add_module(&package_name, Some(span_from_node(&node)));
        // issue #394: real declaration; opt dual-use bare helper into is_definition
        context.helper.mark_definition(package_module);
        context
            .helper
            .add_export_edge(context.file_module, package_module);
        context.package_stack.push(PackageContext {
            name: package_name,
            module_id: package_module,
        });
        pushed_package = true;
    }

    if let Some(callable_name) = extract_callable_name(node, context.content) {
        let span = span_from_node(&node);
        let (qualified_name, export_module) = if let Some(package) = context.package_stack.last() {
            (
                format!("{}.{}", package.name, callable_name),
                Some(package.module_id),
            )
        } else {
            (callable_name, Some(context.file_module))
        };

        let node_id = context
            .helper
            .add_function(&qualified_name, Some(span), false, false);
        // issue #394: real declaration; opt dual-use bare helper into is_definition
        context.helper.mark_definition(node_id);
        if let Some(module_id) = export_module {
            context.helper.add_export_edge(module_id, node_id);
        }
        next_callable = Some(node_id);
        context
            .callables
            .push((node_id, node.start_byte(), node.end_byte()));
    }

    if is_qualified_call_node(node.kind())
        && let Some((package_name, proc_name)) = extract_qualified_parts(node, context.content)
        && !is_builtin_package(&package_name)
        && is_valid_identifier(&package_name)
        && is_valid_identifier(&proc_name)
    {
        let qualified_name = format!("{package_name}.{proc_name}");
        let callee_id = context
            .helper
            .add_function(&qualified_name, None, false, false);
        let caller = next_callable.unwrap_or(context.file_module);
        context.helper.add_call_edge_full_with_span(
            caller,
            callee_id,
            0,
            false,
            vec![span_from_node(&node)],
        );
    }

    if is_dml_node(node.kind()) {
        let caller = next_callable.unwrap_or(context.file_module);
        extract_table_edges(
            node,
            context.content,
            context.helper,
            caller,
            context.table_edges_seen,
        );
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, next_callable, context)?;
    }

    if pushed_package {
        context.package_stack.pop();
    }

    context.guard.exit();
    Ok(())
}

fn extract_table_edges(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper<'_>,
    caller: NodeId,
    table_edges_seen: &mut HashSet<TableEdgeKey>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !is_table_name_node(child.kind()) {
            continue;
        }

        let Ok(raw_name) = child.utf8_text(content) else {
            continue;
        };
        let table_name = clean_identifier(raw_name);
        if table_name.is_empty() || is_sql_keyword(&table_name) {
            continue;
        }

        let (schema, table_only) = split_schema_table(&table_name);
        if !is_valid_identifier(table_only) {
            continue;
        }
        let table_only = table_only.to_ascii_lowercase();
        let schema = schema.map(str::to_ascii_lowercase);
        let table_node = helper.add_variable(&table_only, Some(span_from_node(&child)));
        let spans = vec![span_from_node(&node)];

        match node.kind() {
            "insert_statement" => helper.add_table_write_edge_with_span(
                caller,
                table_node,
                &table_only,
                schema.as_deref(),
                TableWriteOp::Insert,
                spans,
            ),
            "update_statement" => helper.add_table_write_edge_with_span(
                caller,
                table_node,
                &table_only,
                schema.as_deref(),
                TableWriteOp::Update,
                spans,
            ),
            "delete_statement" => helper.add_table_write_edge_with_span(
                caller,
                table_node,
                &table_only,
                schema.as_deref(),
                TableWriteOp::Delete,
                spans,
            ),
            _ => helper.add_table_read_edge_with_span(
                caller,
                table_node,
                &table_only,
                schema.as_deref(),
                spans,
            ),
        }

        let op_key = match node.kind() {
            "insert_statement" => TableOp::Insert,
            "update_statement" => TableOp::Update,
            "delete_statement" => TableOp::Delete,
            _ => TableOp::Read,
        };
        table_edges_seen.insert(TableEdgeKey::new(op_key, &table_only));
    }
}

fn extract_table_edges_from_text(
    content: &[u8],
    helper: &mut GraphBuildHelper<'_>,
    module_id: NodeId,
    table_edges_seen: &mut HashSet<TableEdgeKey>,
) {
    let mut offset = 0usize;
    for statement_bytes in content.split(|b| *b == b';') {
        let statement = String::from_utf8_lossy(statement_bytes);
        let end = offset + statement_bytes.len();
        let span = Span::from_bytes(offset, end);

        let ops = parse_table_ops_from_statement(&statement);
        for (op, table_name) in ops {
            let clean_name = clean_identifier(&table_name);
            if clean_name.is_empty() || is_sql_keyword(&clean_name) {
                continue;
            }

            let (schema, table_only) = split_schema_table(&clean_name);
            if !is_valid_identifier(table_only) {
                continue;
            }
            let table_only = table_only.to_ascii_lowercase();
            let schema = schema.map(str::to_ascii_lowercase);
            let edge_key = TableEdgeKey::new(op, &table_only);
            if table_edges_seen.contains(&edge_key) {
                continue;
            }
            let table_node = helper.add_variable(&table_only, Some(span));
            let spans = vec![span];
            match op {
                TableOp::Read => helper.add_table_read_edge_with_span(
                    module_id,
                    table_node,
                    &table_only,
                    schema.as_deref(),
                    spans,
                ),
                TableOp::Insert => helper.add_table_write_edge_with_span(
                    module_id,
                    table_node,
                    &table_only,
                    schema.as_deref(),
                    TableWriteOp::Insert,
                    spans,
                ),
                TableOp::Update => helper.add_table_write_edge_with_span(
                    module_id,
                    table_node,
                    &table_only,
                    schema.as_deref(),
                    TableWriteOp::Update,
                    spans,
                ),
                TableOp::Delete => helper.add_table_write_edge_with_span(
                    module_id,
                    table_node,
                    &table_only,
                    schema.as_deref(),
                    TableWriteOp::Delete,
                    spans,
                ),
            }
            table_edges_seen.insert(edge_key);
        }

        offset = end + 1;
    }
}

/// Extract `TypeOf` and References edges from PL/SQL text content.
///
/// Scans for:
/// - `p_name IN/OUT/IN OUT TYPE` parameter patterns inside `CREATE PROCEDURE`/`FUNCTION`
/// - `v_name TYPE;` or `v_name TYPE := value;` variable declaration patterns
/// - `FUNCTION name(...) RETURN TYPE IS` return type patterns
#[allow(clippy::too_many_lines)]
fn extract_typeof_edges_from_text(
    content: &[u8],
    helper: &mut GraphBuildHelper<'_>,
    module_id: NodeId,
    callables_byte_ranges: &[(NodeId, usize, usize)],
) {
    let Ok(content_str) = std::str::from_utf8(content) else {
        return;
    };

    let lines: Vec<&str> = content_str.lines().collect();
    let line_offsets: Vec<usize> = {
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        offsets.push(0);
        let mut offset = 0;
        for line in &lines {
            offset += line.len() + 1;
            offsets.push(offset);
        }
        offsets
    };

    // Track current callable context for parameter/return type extraction
    let mut current_callable: Option<(NodeId, String)> = None;
    let mut in_param_list = false;
    let mut param_index: u16 = 0;
    // Per-callable dedup of References edges
    let mut ref_seen = std::collections::HashSet::new();

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();
        let byte_offset = line_offsets[line_idx];

        // Detect procedure/function declarations (non-CREATE)
        if (upper.starts_with("PROCEDURE ") || upper.starts_with("FUNCTION "))
            && let Some(result) = parse_callable_declaration(
                trimmed,
                &upper,
                byte_offset,
                callables_byte_ranges,
                module_id,
            )
        {
            current_callable = Some((result.callable_id, result.name));
            in_param_list = result.in_param_list;
            param_index = 0;
            ref_seen.clear();

            if let Some(paren_content) = result.paren_content {
                extract_params_from_text(
                    &paren_content,
                    result.callable_id,
                    &mut param_index,
                    helper,
                    byte_offset,
                    &mut ref_seen,
                );
            }

            if result.is_function {
                extract_return_type_from_line(
                    &upper,
                    result.callable_id,
                    helper,
                    byte_offset,
                    &mut ref_seen,
                );
            }

            continue;
        }

        // Detect CREATE PROCEDURE/FUNCTION
        if upper.starts_with("CREATE ")
            && (upper.contains("PROCEDURE") || upper.contains("FUNCTION"))
            && let Some(result) = parse_create_callable(
                trimmed,
                &upper,
                byte_offset,
                callables_byte_ranges,
                module_id,
            )
        {
            current_callable = Some((result.callable_id, result.name));
            in_param_list = result.in_param_list;
            param_index = 0;
            ref_seen.clear();

            if let Some(paren_content) = result.paren_content {
                extract_params_from_text(
                    &paren_content,
                    result.callable_id,
                    &mut param_index,
                    helper,
                    byte_offset,
                    &mut ref_seen,
                );
            }

            if result.is_function {
                extract_return_type_from_line(
                    &upper,
                    result.callable_id,
                    helper,
                    byte_offset,
                    &mut ref_seen,
                );
            }

            continue;
        }

        // Continue parameter list from previous lines
        if in_param_list {
            if let Some((callable_id, _)) = &current_callable {
                let callable_id = *callable_id;
                if let Some(close_pos) = find_depth_zero_close_paren(trimmed) {
                    let paren_content = &trimmed[..close_pos];
                    extract_params_from_text(
                        paren_content,
                        callable_id,
                        &mut param_index,
                        helper,
                        byte_offset,
                        &mut ref_seen,
                    );
                    in_param_list = false;
                    // Check for RETURN after closing paren
                    let after_paren = &trimmed[close_pos + 1..];
                    let after_upper = after_paren.to_uppercase();
                    if let Some(ret_idx) = after_upper.find("RETURN") {
                        let type_text = after_paren.get(ret_idx + 6..).unwrap_or("").trim();
                        let type_text = type_text
                            .split(|c: char| c.is_whitespace() || c == ';')
                            .next()
                            .unwrap_or("")
                            .trim();
                        if !type_text.is_empty() {
                            emit_typeof_edge(
                                callable_id,
                                type_text,
                                TypeOfContext::Return,
                                None,
                                None,
                                helper,
                                &mut ref_seen,
                            );
                        }
                    }
                } else {
                    extract_params_from_text(
                        trimmed,
                        callable_id,
                        &mut param_index,
                        helper,
                        byte_offset,
                        &mut ref_seen,
                    );
                }
            }
            continue;
        }

        // Detect variable declarations: v_name TYPE; or v_name TYPE := value;
        if is_variable_declaration_candidate(&upper)
            && let Some((var_name, type_text)) = parse_variable_declaration(trimmed)
        {
            let caller = current_callable.as_ref().map_or(module_id, |(id, _)| *id);
            emit_typeof_edge(
                caller,
                &type_text,
                TypeOfContext::Variable,
                None,
                Some(&var_name),
                helper,
                &mut ref_seen,
            );
        }
    }
}

/// Result of parsing a callable declaration line.
struct CallableParseResult {
    callable_id: NodeId,
    name: String,
    is_function: bool,
    in_param_list: bool,
    paren_content: Option<String>,
}

/// Find the position of the closing `)` that matches the open paren at depth 0.
///
/// `text` should start AFTER the opening `(`. Returns the byte index of the
/// matching `)` relative to `text`, or `None` if unmatched (i.e. multiline).
fn find_matching_close_paren(text: &str) -> Option<usize> {
    let mut depth: u32 = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Check if a line contains a closing `)` at depth 0, accounting for nested
/// parens like `NUMBER(10,2)`. Returns the byte index if found.
fn find_depth_zero_close_paren(text: &str) -> Option<usize> {
    let mut depth: u32 = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Parse a `PROCEDURE`/`FUNCTION` declaration (not preceded by `CREATE`).
fn parse_callable_declaration(
    trimmed: &str,
    upper: &str,
    byte_offset: usize,
    callables_byte_ranges: &[(NodeId, usize, usize)],
    module_id: NodeId,
) -> Option<CallableParseResult> {
    let is_function = upper.starts_with("FUNCTION ");
    let keyword_len = if is_function { 9 } else { 10 };
    let rest = trimmed.get(keyword_len..)?.trim();

    let name = rest
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim();

    if name.is_empty() || !is_valid_identifier(name) {
        return None;
    }

    let callable_id =
        find_callable_by_offset(callables_byte_ranges, byte_offset).unwrap_or(module_id);

    let (paren_content, has_close) = if let Some(paren_start) = trimmed.find('(') {
        let after_open = &trimmed[paren_start + 1..];
        if let Some(close_pos) = find_matching_close_paren(after_open) {
            (Some(after_open[..close_pos].to_string()), true)
        } else {
            (Some(after_open.to_string()), false)
        }
    } else {
        (None, false)
    };

    let in_param_list = trimmed.contains('(') && !has_close;

    Some(CallableParseResult {
        callable_id,
        name: name.to_string(),
        is_function,
        in_param_list,
        paren_content,
    })
}

/// Parse a `CREATE [OR REPLACE] PROCEDURE`/`FUNCTION` declaration.
fn parse_create_callable(
    trimmed: &str,
    upper: &str,
    byte_offset: usize,
    callables_byte_ranges: &[(NodeId, usize, usize)],
    module_id: NodeId,
) -> Option<CallableParseResult> {
    let is_function = upper.contains("FUNCTION");
    let keyword = if is_function { "FUNCTION" } else { "PROCEDURE" };
    let kw_idx = upper.find(keyword)?;
    let after_kw = trimmed.get(kw_idx + keyword.len()..)?.trim();

    let name = after_kw
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim();

    if name.is_empty() || !is_valid_identifier(name) {
        return None;
    }

    let callable_id =
        find_callable_by_offset(callables_byte_ranges, byte_offset).unwrap_or(module_id);

    let (paren_content, has_close) = if let Some(paren_start) = trimmed.find('(') {
        let after_open = &trimmed[paren_start + 1..];
        if let Some(close_pos) = find_matching_close_paren(after_open) {
            (Some(after_open[..close_pos].to_string()), true)
        } else {
            (Some(after_open.to_string()), false)
        }
    } else {
        (None, false)
    };

    let in_param_list = trimmed.contains('(') && !has_close;

    Some(CallableParseResult {
        callable_id,
        name: name.to_string(),
        is_function,
        in_param_list,
        paren_content,
    })
}

/// Check whether a line is a candidate for variable declaration parsing.
///
/// Filters out lines that start with PL/SQL keywords or comments.
fn is_variable_declaration_candidate(upper: &str) -> bool {
    !upper.starts_with("--")
        && !upper.starts_with("/*")
        && !upper.starts_with("PROCEDURE")
        && !upper.starts_with("FUNCTION")
        && !upper.starts_with("CREATE")
        && !upper.starts_with("BEGIN")
        && !upper.starts_with("END")
        && !upper.starts_with("IF ")
        && !upper.starts_with("EXCEPTION")
        && !upper.starts_with("WHEN ")
}

/// Parse a variable declaration line: `v_name TYPE;` or `v_name TYPE := value;`
fn parse_variable_declaration(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim().trim_end_matches(';').trim();

    // Must contain a space (name type) pattern
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let var_name = parts[0].trim();
    if !is_valid_identifier(var_name) {
        return None;
    }

    // Skip if line starts with SQL keywords or PL/SQL keywords
    let upper_name = var_name.to_uppercase();
    if is_variable_decl_keyword(&upper_name) {
        return None;
    }

    // Rest after name = type, possibly with := default
    let rest = parts[1..].join(" ");
    let type_text = rest.split(":=").next().unwrap_or(&rest).trim();

    // Also split on DEFAULT keyword (case-insensitive)
    let type_text = if let Some(default_idx) = type_text.to_uppercase().find("DEFAULT") {
        type_text[..default_idx].trim()
    } else {
        type_text
    };

    if type_text.is_empty() {
        return None;
    }

    // Handle CONSTANT keyword
    let type_text = if type_text.to_uppercase().starts_with("CONSTANT ") {
        type_text.get(9..)?.trim()
    } else {
        type_text
    };

    if type_text.is_empty() {
        return None;
    }

    Some((var_name.to_string(), type_text.to_string()))
}

/// Check if a name is a PL/SQL keyword that should not be treated as a variable name.
fn is_variable_decl_keyword(upper_name: &str) -> bool {
    matches!(
        upper_name,
        "SELECT"
            | "FROM"
            | "WHERE"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "BEGIN"
            | "END"
            | "IF"
            | "THEN"
            | "ELSE"
            | "ELSIF"
            | "LOOP"
            | "FOR"
            | "WHILE"
            | "RETURN"
            | "RAISE"
            | "DECLARE"
            | "SET"
            | "OPEN"
            | "CLOSE"
            | "FETCH"
            | "INTO"
            | "CURSOR"
            | "TYPE"
            | "IS"
            | "AS"
            | "OR"
            | "AND"
            | "NOT"
            | "NULL"
            | "EXCEPTION"
            | "PRAGMA"
            | "EXIT"
            | "CONTINUE"
            | "GOTO"
            | "EXECUTE"
    )
}

/// Split parameter list text by commas, respecting parenthesized groups like `NUMBER(10,2)`.
fn split_params_paren_aware(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth: u32 = 0;
    for ch in text.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Extract parameters from parameter list text.
fn extract_params_from_text(
    text: &str,
    callable_id: NodeId,
    param_index: &mut u16,
    helper: &mut GraphBuildHelper<'_>,
    _byte_offset: usize,
    ref_seen: &mut HashSet<String>,
) {
    for param_text in split_params_paren_aware(text) {
        let param = param_text.trim();
        if param.is_empty() {
            continue;
        }

        let parts: Vec<&str> = param.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let p_name = parts[0].trim();
        if !is_valid_identifier(p_name) {
            *param_index += 1;
            continue;
        }

        // Find type: skip IN/OUT/IN OUT/NOCOPY keywords
        let type_parts = collect_type_parts(&parts);

        if type_parts.is_empty() {
            *param_index += 1;
            continue;
        }

        let type_text_original = type_parts.join(" ");
        let type_text_clean = type_text_original
            .split(":=")
            .next()
            .unwrap_or(&type_text_original)
            .trim();

        // Remove DEFAULT clause (case-insensitive)
        let type_text_clean =
            if let Some(default_idx) = type_text_clean.to_uppercase().find("DEFAULT") {
                type_text_clean[..default_idx].trim()
            } else {
                type_text_clean
            };

        if !type_text_clean.is_empty() {
            emit_typeof_edge(
                callable_id,
                type_text_clean,
                TypeOfContext::Parameter,
                Some(*param_index),
                Some(p_name),
                helper,
                ref_seen,
            );
        }

        *param_index += 1;
    }
}

/// Collect the type parts from a parameter split by whitespace,
/// skipping PL/SQL direction keywords (`IN`, `OUT`, `NOCOPY`).
fn collect_type_parts<'a>(parts: &[&'a str]) -> Vec<&'a str> {
    let mut type_parts = Vec::new();
    let mut i = 1;
    while i < parts.len() {
        let upper = parts[i].to_uppercase();
        if matches!(upper.as_str(), "IN" | "OUT" | "NOCOPY") {
            i += 1;
            continue;
        }
        // Everything remaining is the type
        type_parts = parts[i..].to_vec();
        break;
    }
    type_parts
}

/// Extract return type from a function declaration line.
fn extract_return_type_from_line(
    upper: &str,
    callable_id: NodeId,
    helper: &mut GraphBuildHelper<'_>,
    _byte_offset: usize,
    ref_seen: &mut HashSet<String>,
) {
    // Look for RETURN keyword (not inside parentheses)
    let search_text = if let Some(paren_end) = upper.find(')') {
        &upper[paren_end + 1..]
    } else {
        upper
    };

    if let Some(ret_idx) = search_text.find("RETURN") {
        let after_return = search_text.get(ret_idx + 6..).unwrap_or("").trim();
        let type_text = after_return
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            .unwrap_or("")
            .trim();

        if !type_text.is_empty()
            && type_text.to_uppercase() != "IS"
            && type_text.to_uppercase() != "AS"
        {
            emit_typeof_edge(
                callable_id,
                type_text,
                TypeOfContext::Return,
                None,
                None,
                helper,
                ref_seen,
            );
        }
    }
}

/// Emit a `TypeOf` edge and associated References edges.
///
/// `ref_seen` tracks already-emitted References targets per source to avoid duplicates.
fn emit_typeof_edge(
    source_id: NodeId,
    type_text: &str,
    context: TypeOfContext,
    index: Option<u16>,
    name: Option<&str>,
    helper: &mut GraphBuildHelper<'_>,
    ref_seen: &mut HashSet<String>,
) {
    if let Some(type_name) = type_extractor::extract_type_name(type_text) {
        let target_id = helper.add_type(&type_name, None);
        helper.add_typeof_edge_with_context(source_id, target_id, Some(context), index, name);
    }

    // References edges for non-builtin types (deduped per source)
    for ref_name in type_extractor::extract_all_type_names(type_text) {
        if ref_seen.insert(ref_name.clone()) {
            let ref_target = helper.add_type(&ref_name, None);
            helper.add_reference_edge(source_id, ref_target);
        }
    }
}

/// Find the callable `NodeId` whose byte range contains the given offset.
fn find_callable_by_offset(
    callables: &[(NodeId, usize, usize)],
    byte_offset: usize,
) -> Option<NodeId> {
    let mut best: Option<&(NodeId, usize, usize)> = None;
    for entry in callables {
        if byte_offset >= entry.1
            && byte_offset <= entry.2
            && best
                .as_ref()
                .is_none_or(|b| (entry.2 - entry.1) < (b.2 - b.1))
        {
            best = Some(entry);
        }
    }
    best.map(|e| e.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TableOp {
    Read,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TableEdgeKey {
    op: TableOp,
    table: String,
}

impl TableEdgeKey {
    fn new(op: TableOp, table: &str) -> Self {
        Self {
            op,
            table: table.to_string(),
        }
    }
}

fn parse_table_ops_from_statement(statement: &str) -> Vec<(TableOp, String)> {
    let mut ops = Vec::new();
    let upper = statement.to_uppercase();
    let tokens: Vec<&str> = upper.split_whitespace().collect();
    if tokens.is_empty() {
        return ops;
    }

    let mut idx = 0usize;
    while idx < tokens.len() {
        match tokens[idx] {
            "SELECT" => {
                if let Some(from_idx) = tokens[idx..].iter().position(|t| *t == "FROM")
                    && let Some(table) = tokens.get(idx + from_idx + 1)
                {
                    ops.push((TableOp::Read, (*table).to_string()));
                    idx = idx + from_idx + 1;
                }
            }
            "INSERT" => {
                let table = if tokens.get(idx + 1) == Some(&"INTO") {
                    tokens.get(idx + 2)
                } else {
                    tokens.get(idx + 1)
                };
                if let Some(name) = table {
                    ops.push((TableOp::Insert, (*name).to_string()));
                }
            }
            "UPDATE" => {
                if let Some(name) = tokens.get(idx + 1) {
                    ops.push((TableOp::Update, (*name).to_string()));
                }
            }
            "DELETE" => {
                let table = if tokens.get(idx + 1) == Some(&"FROM") {
                    tokens.get(idx + 2)
                } else {
                    tokens.get(idx + 1)
                };
                if let Some(name) = table {
                    ops.push((TableOp::Delete, (*name).to_string()));
                }
            }
            "MERGE" => {
                let table = if tokens.get(idx + 1) == Some(&"INTO") {
                    tokens.get(idx + 2)
                } else {
                    tokens.get(idx + 1)
                };
                if let Some(name) = table {
                    ops.push((TableOp::Update, (*name).to_string()));
                }
            }
            _ => {}
        }
        idx += 1;
    }

    ops
}

fn span_from_node(node: &Node<'_>) -> Span {
    Span::from_bytes(node.start_byte(), node.end_byte())
}

fn is_package_node(kind: &str) -> bool {
    matches!(
        kind,
        "create_package"
            | "package_spec"
            | "package_specification"
            | "create_package_body"
            | "package_body"
    )
}

fn is_callable_node(kind: &str) -> bool {
    matches!(
        kind,
        "create_procedure"
            | "procedure_definition"
            | "procedure_declaration"
            | "procedure_body"
            | "procedure_spec"
            | "create_function"
            | "function_definition"
            | "function_declaration"
            | "function_body"
            | "function_spec"
            | "create_trigger"
            | "trigger_definition"
    )
}

fn extract_callable_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    if !is_callable_node(node.kind()) {
        return None;
    }
    extract_name_from_children(node, content)
}

fn extract_name_from_children(node: Node<'_>, content: &[u8]) -> Option<String> {
    for field_name in &["name", "identifier", "object_name", "package_name"] {
        if let Some(name_node) = node.child_by_field_name(field_name)
            && let Ok(text) = name_node.utf8_text(content)
        {
            let name = text.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "name" | "simple_identifier" | "object_name"
        ) && let Ok(text) = child.utf8_text(content)
        {
            let name = text.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    None
}

fn is_qualified_call_node(kind: &str) -> bool {
    matches!(
        kind,
        "qualified_expression" | "qualified_identifier" | "member_expression" | "qualified_name"
    )
}

fn extract_qualified_parts(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
    if let (Some(object), Some(property)) = (
        node.child_by_field_name("object"),
        node.child_by_field_name("property")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.child_by_field_name("member")),
    ) {
        let package = object.utf8_text(content).ok()?.trim().to_string();
        let proc = property.utf8_text(content).ok()?.trim().to_string();
        if !package.is_empty() && !proc.is_empty() {
            let proc_name = proc.split('(').next().unwrap_or(&proc).trim().to_string();
            return Some((package, proc_name));
        }
    }

    let mut identifiers = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "name" | "simple_identifier" | "qualified_name" | "object_name" => {
                if let Ok(text) = child.utf8_text(content) {
                    let clean = text.trim().to_string();
                    if !clean.is_empty() {
                        identifiers.push(clean);
                    }
                }
            }
            "qualified_expression" | "qualified_identifier" | "member_expression" => {
                if let Some((pkg, proc)) = extract_qualified_parts(child, content) {
                    identifiers.push(pkg);
                    identifiers.push(proc);
                }
            }
            "." | "(" | ")" | "," => {}
            _ => {
                if let Ok(text) = child.utf8_text(content) {
                    let clean = text.trim();
                    if !clean.is_empty()
                        && !clean.contains('(')
                        && !clean.contains(' ')
                        && clean.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        identifiers.push(clean.to_string());
                    }
                }
            }
        }
    }

    if identifiers.len() >= 2 {
        let proc_name = identifiers.pop()?;
        let package_name = identifiers.pop()?;
        return Some((package_name, proc_name));
    }

    if let Ok(text) = node.utf8_text(content) {
        let text = text.trim();
        if text.chars().filter(|&c| c == '.').count() == 1
            && let Some(dot_idx) = text.find('.')
        {
            let package = text[..dot_idx].trim();
            let rest = text[dot_idx + 1..].trim();
            let proc = rest.split('(').next().unwrap_or(rest).trim();
            if is_valid_identifier(package) && is_valid_identifier(proc) {
                return Some((package.to_string(), proc.to_string()));
            }
        }
    }

    None
}

fn is_table_name_node(kind: &str) -> bool {
    matches!(
        kind,
        "table_reference" | "table_name" | "identifier" | "object_name"
    )
}

fn clean_identifier(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim_matches(|c: char| c == ';' || c == ',' || c == ')' || c == '(')
        .to_string()
}

fn split_schema_table(name: &str) -> (Option<&str>, &str) {
    if let Some((schema, table)) = name.split_once('.') {
        (Some(schema), table)
    } else {
        (None, name)
    }
}

fn is_dml_node(kind: &str) -> bool {
    matches!(
        kind,
        "select_statement"
            | "insert_statement"
            | "update_statement"
            | "delete_statement"
            | "merge_statement"
    )
}

/// Check if a package name is a built-in Oracle package.
fn is_builtin_package(name: &str) -> bool {
    let upper = name.to_uppercase();
    matches!(
        upper.as_str(),
        "DBMS_OUTPUT"
            | "DBMS_SQL"
            | "DBMS_LOB"
            | "DBMS_UTILITY"
            | "DBMS_LOCK"
            | "DBMS_JOB"
            | "DBMS_SCHEDULER"
            | "UTL_FILE"
            | "UTL_HTTP"
            | "UTL_SMTP"
            | "UTL_RAW"
            | "SYS"
            | "STANDARD"
    )
}

/// Check if a name is an SQL keyword (not a table name).
fn is_sql_keyword(name: &str) -> bool {
    let upper = name.to_uppercase();
    matches!(
        upper.as_str(),
        "SELECT"
            | "FROM"
            | "WHERE"
            | "INTO"
            | "VALUES"
            | "SET"
            | "AND"
            | "OR"
            | "NOT"
            | "NULL"
            | "TRUE"
            | "FALSE"
            | "AS"
            | "ON"
            | "JOIN"
            | "LEFT"
            | "RIGHT"
            | "INNER"
            | "OUTER"
            | "CROSS"
            | "DUAL"
    )
}

/// Check if a string looks like a valid PL/SQL identifier.
fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '#')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_builder_language() {
        let builder = OraclePlsqlGraphBuilder::new();
        assert_eq!(builder.language(), Language::Plsql);
    }

    #[test]
    fn test_is_builtin_package() {
        assert!(is_builtin_package("DBMS_OUTPUT"));
        assert!(is_builtin_package("dbms_output"));
        assert!(is_builtin_package("UTL_FILE"));
        assert!(!is_builtin_package("my_package"));
        assert!(!is_builtin_package("hr_utils"));
    }

    #[test]
    fn test_is_sql_keyword() {
        assert!(is_sql_keyword("SELECT"));
        assert!(is_sql_keyword("JOIN"));
        assert!(!is_sql_keyword("employees"));
    }

    #[test]
    fn test_is_valid_identifier() {
        assert!(is_valid_identifier("my_table"));
        assert!(is_valid_identifier("EMPLOYEES"));
        assert!(is_valid_identifier("T1"));
        assert!(is_valid_identifier("_private"));
        assert!(is_valid_identifier("pkg$name"));
        assert!(is_valid_identifier("name#hash"));

        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("123abc"));
        assert!(!is_valid_identifier("has space"));
        assert!(!is_valid_identifier("has.dot"));
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{ShapeBudget, compute_shape_descriptor};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/data/sample.pkb"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_plsql_sqry::language())
            .expect("load plsql grammar");
        parser.parse(src, None).expect("parse")
    }

    /// Resolve the first node of one of the given kinds anywhere in the tree.
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
    fn cf_map_is_non_empty_and_covers_real_kinds() {
        let mapping = plsql_shape_mapping();
        let populated = mapping.cf_by_kind_id.iter().filter(|s| s.is_some()).count();
        assert!(
            populated > 0,
            "PL/SQL cf map must map at least one real grammar kind"
        );

        let lang: tree_sitter::Language = tree_sitter_plsql_sqry::language();
        let id = |n: &str| lang.id_for_node_kind(n, true);
        assert_eq!(
            mapping.cf_bucket(id("if_statement")),
            Some(CfBucket::Branch)
        );
        assert_eq!(
            mapping.cf_bucket(id("for_loop_statement")),
            Some(CfBucket::Loop)
        );
        assert_eq!(
            mapping.cf_bucket(id("case_statement")),
            Some(CfBucket::Match)
        );
        assert_eq!(
            mapping.cf_bucket(id("exception_handler")),
            Some(CfBucket::Catch)
        );
        assert_eq!(
            mapping.cf_bucket(id("raise_statement")),
            Some(CfBucket::Throw)
        );
        assert_eq!(
            mapping.cf_bucket(id("return_statement")),
            Some(CfBucket::Return)
        );
    }

    #[test]
    fn descriptor_counts_procedural_control_flow() {
        let tree = parse(SAMPLE);
        let proc = first_of(tree.root_node(), &["procedure_definition"])
            .expect("procedure_definition in fixture");
        let descriptor = compute_shape_descriptor(
            proc,
            SAMPLE.as_bytes(),
            plsql_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(
            !descriptor.is_unhashable(),
            "a procedure body with control flow must be hashable"
        );
        let h = &descriptor.cf_histogram;
        assert!(h[CfBucket::Branch.index()] >= 1, "IF must count Branch");
        assert!(
            h[CfBucket::Loop.index()] >= 2,
            "FOR + WHILE must count Loop twice"
        );
        assert!(h[CfBucket::Match.index()] >= 1, "CASE must count Match");
        assert!(
            h[CfBucket::Catch.index()] >= 1,
            "the exception block must count Catch"
        );
        assert!(h[CfBucket::Throw.index()] >= 1, "RAISE must count Throw");
        assert!(
            h[CfBucket::Call.index()] >= 1,
            "the log_it calls must count Call"
        );
        assert!(
            h[CfBucket::Assign.index()] >= 1,
            "the := assignments must count Assign"
        );
    }

    #[test]
    fn signature_shape_reads_parameters_defaults_and_return() {
        let tree = parse(SAMPLE);
        let proc = first_of(tree.root_node(), &["procedure_definition"])
            .expect("procedure_definition in fixture");
        let shape = plsql_shape_mapping().signature_shape(proc, SAMPLE.as_bytes());
        assert_eq!(
            shape.arity_positional, 3,
            "classify(score, bonus, grade) has three parameters"
        );
        assert!(
            shape.has_defaults,
            "the DEFAULT 0 parameter must set has_defaults"
        );

        let func = first_of(tree.root_node(), &["function_definition"])
            .expect("function_definition in fixture");
        let fshape = plsql_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        assert!(
            fshape.has_return_annotation,
            "a FUNCTION ... RETURN VARCHAR2 must set has_return_annotation"
        );
    }
}
