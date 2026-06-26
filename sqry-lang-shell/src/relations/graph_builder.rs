//! `GraphBuilder` for Shell scripts using manual tree walking approach.
//!
//! Extracts function definitions, call edges, and import edges from Shell/Bash scripts.
//! Handles both POSIX (`foo() { ... }`) and Bash (`function foo { ... }`) syntax.
//! Filters out built-in commands to avoid synthetic nodes for shell builtins.
//! Detects `source` and `.` commands as import edges for cross-file module inclusion.

use std::sync::OnceLock;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::ExportKind;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, GraphResult, Language, Span,
    unified::{GraphBuildHelper, StagingGraph},
};
use tree_sitter::{Node, StreamingIterator, Tree};

/// `GraphBuilder` for Shell scripts
pub struct ShellGraphBuilder {
    max_scope_depth: usize,
}

impl Default for ShellGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 2, // Shell typically has flat structure (script -> function)
        }
    }
}

impl GraphBuilder for ShellGraphBuilder {
    fn language(&self) -> Language {
        Language::Shell
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(shell_shape_mapping())
    }

    // Shell graph extraction is linear and benefits from a single pass.
    #[allow(clippy::too_many_lines)]
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Shell);

        // Build AST metadata to track function contexts
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Phase 0: ALWAYS create script-level module node (entry point)
        let script_name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("script");
        let module_qualified = format!("{script_name}::module");
        let module_id =
            helper.add_module(&module_qualified, Some(Span::from_bytes(0, content.len())));

        // Phase 1: Insert function contexts as nodes and emit Export edges
        // All shell functions are exported from the script module
        // DESIGN: Only user-defined functions (from function_definition AST nodes) are exported.
        // Shell builtins are never defined as function_definition nodes, so they're automatically excluded.
        for context in ast_graph.contexts() {
            let qualified = context.qualified_name();
            let span = Span::from_bytes(context.span.0, context.span.1);
            let visibility = extract_visibility(&qualified);
            let function_id = helper.add_function_with_visibility(
                &qualified,
                Some(span),
                false,
                false,
                Some(visibility),
            );
            // Export all user-defined functions using ExportKind::Direct
            helper.add_export_edge_full(module_id, function_id, ExportKind::Direct, None);
        }

        // Phase 1b: Export variables referenced in explicit `export` commands.
        let mut exported_variables = HashSet::new();
        let root = tree.root_node();
        let mut root_cursor = root.walk();
        for command in root.children(&mut root_cursor) {
            match command.kind() {
                "command" | "declaration_command" => {}
                _ => continue,
            }

            let mut cmd_cursor = command.walk();
            let mut command_name: Option<String> = None;
            let mut arg_nodes: Vec<Node> = Vec::new();

            for child in command.children(&mut cmd_cursor) {
                match child.kind() {
                    "export" | "word" | "command_name" | "variable_name" => {
                        if command_name.is_none() {
                            command_name = Some(get_node_text(child, content)?);
                        } else {
                            arg_nodes.push(child);
                        }
                    }
                    "variable_assignment" => arg_nodes.push(child),
                    _ => {}
                }
            }

            let Some(command_name) = command_name else {
                continue;
            };
            if command_name != "export" {
                continue;
            }

            let mut mark_next_as_function = false;
            for arg_node in arg_nodes {
                match arg_node.kind() {
                    "word" | "command_name" | "variable_name" => {
                        let text = get_node_text(arg_node, content)?;
                        if text == "-f" {
                            mark_next_as_function = true;
                            continue;
                        }
                        if text.starts_with('-') {
                            continue;
                        }
                        if mark_next_as_function {
                            mark_next_as_function = false;
                            continue;
                        }

                        if exported_variables.insert(text.clone()) {
                            let var_id = helper.add_variable(&text, Some(span_from_node(arg_node)));
                            helper.add_export_edge_full(
                                module_id,
                                var_id,
                                ExportKind::Direct,
                                None,
                            );
                        }
                    }
                    "variable_assignment" => {
                        if let Some(name_node) = arg_node.child_by_field_name("name") {
                            let name = get_node_text(name_node, content)?;
                            if exported_variables.insert(name.clone()) {
                                let var_id =
                                    helper.add_variable(&name, Some(span_from_node(name_node)));
                                helper.add_export_edge_full(
                                    module_id,
                                    var_id,
                                    ExportKind::Direct,
                                    None,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Phase 2: Traverse tree to collect call edges
        let mut stack = vec![tree.root_node()];
        let mut visited = HashSet::new();

        while let Some(node) = stack.pop() {
            let node_id = node.id();

            // Skip if already visited (prevents infinite loops)
            if !visited.insert(node_id) {
                continue;
            }

            // Skip non-code nodes
            match node.kind() {
                "comment" | "string" | "raw_string" | "ansi_c_string" => {
                    continue;
                }
                _ => {}
            }

            // Detect command invocations
            if node.kind() == "command" {
                // Check for import commands (source/.) first
                if let Some((importer_qname, imported_path, span)) =
                    build_import_edge_for_staging(&ast_graph, node, content, &module_qualified)?
                {
                    let from_id = helper.add_import(&importer_qname, None);
                    let to_id = helper.add_import(&imported_path, Some(span));
                    helper.add_import_edge(from_id, to_id);
                }
                // Then check for call edges (user-defined function calls)
                else if let Some((caller_qname, callee_qname, argument_count, span)) =
                    build_call_edge_for_staging(&ast_graph, node, content, &module_qualified)?
                {
                    let source_id =
                        helper.ensure_callee(&caller_qname, span, CalleeKindHint::Function);
                    let target_id =
                        helper.ensure_callee(&callee_qname, span, CalleeKindHint::Function);

                    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                    helper.add_call_edge_full_with_span(
                        source_id,
                        target_id,
                        argument_count,
                        false,
                        vec![span],
                    );
                }
            }

            // Traverse children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }

        Ok(())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Build call edge information for the staging graph.
/// Returns (`caller_qname`, `callee_qname`, `argument_count`, span) tuple.
fn build_call_edge_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node,
    content: &[u8],
    module_name: &str,
) -> GraphResult<Option<(String, String, usize, Span)>> {
    // Find the calling context (which function is this call in?)
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Script-level call - use module-qualified name as context
        module_context = CallContext {
            qualified_name: module_name.to_string(),
            span: (0, content.len()),
        };
        &module_context
    };

    // Extract the command name
    let Some(name_node) = call_node.child_by_field_name("name") else {
        return Ok(None);
    };

    let callee_text = get_node_text(name_node, content)?;

    if callee_text.is_empty() {
        return Ok(None);
    }

    // CRITICAL: Filter out shell built-in commands
    if is_builtin_command(&callee_text) {
        return Ok(None);
    }

    // DESIGN REQUIREMENT: Only create call edges for user-defined functions
    let is_user_defined = ast_graph
        .contexts()
        .iter()
        .any(|ctx| ctx.qualified_name() == callee_text);

    if !is_user_defined {
        return Ok(None);
    }

    let target_qname = callee_text.clone();
    let source_qname = call_context.qualified_name();

    let span = span_from_node(call_node);
    let argument_count = count_arguments(call_node);

    Ok(Some((source_qname, target_qname, argument_count, span)))
}

/// Check if a command is a shell built-in
fn is_builtin_command(cmd: &str) -> bool {
    // Common POSIX and Bash built-ins
    matches!(
        cmd,
        "echo"
            | "cd"
            | "pwd"
            | "ls"
            | "cat"
            | "grep"
            | "sed"
            | "awk"
            | "test"
            | "["
            | "[["
            | "printf"
            | "read"
            | "set"
            | "unset"
            | "export"
            | "alias"
            | "unalias"
            | "bg"
            | "fg"
            | "jobs"
            | "kill"
            | "wait"
            | "eval"
            | "exec"
            | "exit"
            | "return"
            | "shift"
            | "trap"
            | "umask"
            | "readonly"
            | "local"
            | "declare"
            | "typeset"
            | "enable"
            | "help"
            | "let"
            | "break"
            | "continue"
            | "true"
            | "false"
            | ":"
            | "getopts"
            | "hash"
            | "type"
            | "times"
            | "ulimit"
            | "shopt"
            | "complete"
            | "compgen"
            | "fc"
            | "history"
            | "pushd"
            | "popd"
            | "dirs"
            | "bind"
            | "builtin"
            | "command"
            | "mapfile"
            | "readarray"
            | "caller"
            | "disown"
            | "suspend"
            | "compopt"
    )
}

/// Check if a command is a `source` or `.` (dot) import command
fn is_source_command(cmd: &str) -> bool {
    matches!(cmd, "source" | ".")
}

/// Build import edge information for `source` and `.` commands.
///
/// Returns `(importer_qname, imported_path, span)` if the command is an import,
/// or `None` if it's not a source/dot command or has no argument.
fn build_import_edge_for_staging(
    ast_graph: &ASTGraph,
    command_node: Node,
    content: &[u8],
    module_name: &str,
) -> GraphResult<Option<(String, String, Span)>> {
    // Extract the command name
    let Some(name_node) = command_node.child_by_field_name("name") else {
        return Ok(None);
    };

    let cmd_text = get_node_text(name_node, content)?;
    if !is_source_command(&cmd_text) {
        return Ok(None);
    }

    // Find the first argument (the file path) — it's the first child after the command name
    let mut arg_node = None;
    let mut cursor = command_node.walk();
    let mut past_name = false;
    for child in command_node.children(&mut cursor) {
        if child.id() == name_node.id() {
            past_name = true;
            continue;
        }
        if past_name {
            match child.kind() {
                "word" | "string" | "raw_string" | "simple_expansion" | "expansion"
                | "concatenation" => {
                    arg_node = Some(child);
                    break;
                }
                _ => {}
            }
        }
    }

    let Some(arg) = arg_node else {
        return Ok(None);
    };

    // Extract the imported path, stripping quotes for string/raw_string
    let imported_path = extract_source_path(arg, content)?;
    if imported_path.is_empty() {
        return Ok(None);
    }

    // Determine the importer context (function or script-level module)
    let importer_qname = if let Some(ctx) = ast_graph.get_callable_context(command_node.id()) {
        ctx.qualified_name()
    } else {
        module_name.to_string()
    };

    let span = span_from_node(command_node);
    Ok(Some((importer_qname, imported_path, span)))
}

/// Extract the file path from a source/dot command argument node.
///
/// Handles various node types:
/// - `word`: bare path (e.g., `./config.sh`)
/// - `string`/`raw_string`: quoted path — strips surrounding quotes
/// - `simple_expansion`/`expansion`: variable expansion (e.g., `$HOME/.bashrc`)
/// - `concatenation`: mixed literals and expansions
fn extract_source_path(node: Node, content: &[u8]) -> GraphResult<String> {
    match node.kind() {
        "string" | "raw_string" => {
            let text = get_node_text(node, content)?;
            // Strip surrounding quotes (", ', $')
            let stripped = text
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .or_else(|| text.strip_prefix("$'").and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(&text);
            Ok(stripped.to_string())
        }
        // word, simple_expansion, expansion, concatenation — use raw text
        _ => get_node_text(node, content),
    }
}

/// Count arguments in a command invocation
fn count_arguments(call_node: Node) -> usize {
    let mut count: usize = 0;
    let mut cursor = call_node.walk();

    for child in call_node.children(&mut cursor) {
        match child.kind() {
            "word"
            | "string"
            | "raw_string"
            | "ansi_c_string"
            | "simple_expansion"
            | "expansion"
            | "command_substitution" => {
                count += 1;
            }
            _ => {}
        }
    }

    // Subtract 1 for the command name itself
    count.saturating_sub(1)
}

/// Create Span from tree-sitter Node
fn span_from_node(node: Node) -> Span {
    Span::from_bytes(node.start_byte(), node.end_byte())
}

/// Extract text from a node
fn get_node_text(node: Node, content: &[u8]) -> GraphResult<String> {
    node.utf8_text(content)
        .map(|s| s.trim().to_string())
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(node),
            reason: "invalid UTF-8".to_string(),
        })
}

// ============================================================================
// AST Graph - tracks callable contexts (functions)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: (usize, usize),
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
    fn from_tree(tree: &Tree, content: &[u8], _max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();

        // Extract function definitions using tree-sitter query
        let query = tree_sitter::Query::new(
            &tree_sitter_bash::LANGUAGE.into(),
            r"(function_definition name: (word) @function_name) @function_node",
        )
        .map_err(|e| format!("Failed to create query: {e}"))?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let root = tree.root_node();
        let capture_names = query.capture_names();
        let mut matches = cursor.matches(&query, root, content);

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut func_node = None;

            for capture in m.captures {
                let capture_name = capture_names[capture.index as usize];
                match capture_name {
                    "function_name" => name_node = Some(capture.node),
                    "function_node" => func_node = Some(capture.node),
                    _ => {}
                }
            }

            let (Some(name_node), Some(func_node)) = (name_node, func_node) else {
                continue;
            };

            let function_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read function name".to_string())?
                .to_string();

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name: function_name,
                span: (func_node.start_byte(), func_node.end_byte()),
            });

            // Map all descendant nodes to this context
            map_descendants_to_context(func_node, &mut node_to_context, context_idx);
        }

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    fn contexts(&self) -> &[CallContext] {
        &self.contexts
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

/// Extract visibility for a Shell function.
///
/// In Shell/Bash scripts, all user-defined functions are considered public
/// as they can be called from anywhere within the script or sourced by
/// other scripts. Shell doesn't have formal visibility modifiers.
fn extract_visibility(_name: &str) -> &'static str {
    "public"
}

/// Map all descendant nodes to a context index
fn map_descendants_to_context(node: Node, map: &mut HashMap<usize, usize>, context_idx: usize) {
    map.insert(node.id(), context_idx);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        map_descendants_to_context(child, map, context_idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::{StagingOp, test_helpers::*};
    use sqry_core::graph::unified::edge::{EdgeKind, ExportKind};
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::PathBuf;

    fn parse_shell(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .expect("failed to set language");
        parser.parse(source, None).expect("failed to parse")
    }

    #[test]
    fn test_extracts_posix_functions() {
        let source = r#"
foo() {
    echo "foo"
}

bar() {
    echo "bar"
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify script-level module is created
        assert_has_node_with_kind(&staging, "test::module", NodeKind::Module);

        // Verify both functions are extracted
        assert_has_node_with_kind(&staging, "foo", NodeKind::Function);
        assert_has_node_with_kind(&staging, "bar", NodeKind::Function);

        // Verify both functions are exported from the module
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 2, "Expected 2 function exports");
        assert_has_export_edge(&staging, "test::module", "foo");
        assert_has_export_edge(&staging, "test::module", "bar");
    }

    #[test]
    fn test_extracts_bash_functions() {
        let source = r#"
function foo {
    echo "foo"
}

function bar() {
    echo "bar"
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify script-level module is created
        assert_has_node_with_kind(&staging, "test::module", NodeKind::Module);

        // Verify both Bash-style functions are extracted
        assert_has_node_with_kind(&staging, "foo", NodeKind::Function);
        assert_has_node_with_kind(&staging, "bar", NodeKind::Function);

        // Verify both functions are exported from the module
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 2, "Expected 2 function exports");
        assert_has_export_edge(&staging, "test::module", "foo");
        assert_has_export_edge(&staging, "test::module", "bar");
    }

    #[test]
    fn test_creates_call_edges() {
        let source = r#"
caller() {
    callee
}

callee() {
    echo "callee"
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify both functions are extracted
        assert_has_node_with_kind(&staging, "caller", NodeKind::Function);
        assert_has_node_with_kind(&staging, "callee", NodeKind::Function);

        // Verify call edge from caller to callee
        let call_edges = collect_call_edges(&staging);
        assert_eq!(call_edges.len(), 1, "Expected 1 call edge");
        assert_has_call_edge(&staging, "caller", "callee");
    }

    #[test]
    fn test_script_module_node_always_present() {
        // Test that script-level module node is ALWAYS created, even for empty scripts
        let source = r"
#!/bin/bash
# Empty script with no functions
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify script-level module is always created, even for empty scripts
        assert_has_node_with_kind(&staging, "test::module", NodeKind::Module);

        // Verify no functions or exports for empty script
        assert_eq!(count_nodes_by_kind(&staging, NodeKind::Function), 0);
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 0, "Expected no exports for empty script");
    }

    #[test]
    fn test_script_name_function_collision() {
        // Regression: script-level module should not mask functions sharing the script name
        let source = r#"
#!/bin/bash

deploy() {
    helper
}

helper() {
    echo "hi"
}

deploy
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("deploy.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify module and both functions exist
        assert_has_node_with_kind(&staging, "deploy::module", NodeKind::Module);
        assert_has_node_with_kind(&staging, "deploy", NodeKind::Function);
        assert_has_node_with_kind(&staging, "helper", NodeKind::Function);

        // Verify call edge from deploy function to helper
        assert_has_call_edge(&staging, "deploy", "helper");

        // Verify script-level call to deploy function
        assert_has_call_edge(&staging, "deploy::module", "deploy");
    }

    #[test]
    fn test_filters_external_tools() {
        // Test that external tools (git, kubectl, docker, etc.) do NOT create call edges
        let source = r#"
deploy() {
    git status
    kubectl apply -f deployment.yaml
    docker build -t myimage .
    my_helper
}

my_helper() {
    echo "ok"
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify both functions are extracted
        assert_has_node_with_kind(&staging, "deploy", NodeKind::Function);
        assert_has_node_with_kind(&staging, "my_helper", NodeKind::Function);

        // Verify only user-defined function call is recorded (not external tools)
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Expected 1 call edge (only to user function)"
        );
        assert_has_call_edge(&staging, "deploy", "my_helper");

        // Verify no nodes for external tools (git, kubectl, docker)
        assert!(
            !staging.nodes().any(|n| staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("git")
                    || name.contains("kubectl")
                    || name.contains("docker"))),
            "External tools should not create nodes"
        );
    }

    #[test]
    fn test_filters_builtin_commands() {
        let source = r#"
my_function() {
    echo "test"
    cd /tmp
    ls -la
    my_helper
}

my_helper() {
    pwd
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify both functions are extracted
        assert_has_node_with_kind(&staging, "my_function", NodeKind::Function);
        assert_has_node_with_kind(&staging, "my_helper", NodeKind::Function);

        // Verify only user-defined function call is recorded (not builtins)
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Expected 1 call edge (only to user function)"
        );
        assert_has_call_edge(&staging, "my_function", "my_helper");

        // Verify no nodes for builtin commands (echo, cd, ls, pwd)
        assert!(
            !staging.nodes().any(
                |n| staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name == "echo"
                        || name == "cd"
                        || name == "ls"
                        || name == "pwd")
            ),
            "Builtin commands should not create nodes"
        );
    }

    #[test]
    fn test_exports_user_defined_functions() {
        // Test that user-defined functions are exported from the module
        let source = r#"
#!/bin/bash

my_function() {
    echo "exported function"
}

helper() {
    return 0
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("functions.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify both functions are extracted
        assert_has_node_with_kind(&staging, "my_function", NodeKind::Function);
        assert_has_node_with_kind(&staging, "helper", NodeKind::Function);

        // Verify both functions are exported from the module
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 2, "Expected 2 function exports");
        assert_has_export_edge(&staging, "functions::module", "my_function");
        assert_has_export_edge(&staging, "functions::module", "helper");
    }

    #[test]
    fn test_exports_exclude_builtins() {
        // Test that shell builtins are NOT exported (only user-defined functions)
        let source = r#"
#!/bin/bash

my_script() {
    echo "user function"
    cd /tmp
    ls -la
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("script.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify only user-defined function is extracted
        assert_has_node_with_kind(&staging, "my_script", NodeKind::Function);

        // Verify only user-defined function is exported (not builtins)
        let exports = collect_export_edges(&staging);
        assert_eq!(
            exports.len(),
            1,
            "Expected only 1 export (user function, not builtins)"
        );
        assert_has_export_edge(&staging, "script::module", "my_script");

        // Verify no nodes or exports for builtins
        assert!(
            !staging.nodes().any(|n| staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == "echo" || name == "cd" || name == "ls")),
            "Builtins should not create nodes"
        );
    }

    #[test]
    fn test_export_uses_direct_kind() {
        // Test that exports use ExportKind::Direct
        let source = r#"
user_function() {
    echo "test"
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify function is exported
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 1, "Expected 1 export");

        // Verify export uses ExportKind::Direct
        if let Some(StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, .. },
            ..
        }) = exports.first()
        {
            assert_eq!(
                *kind,
                ExportKind::Direct,
                "Export should use ExportKind::Direct"
            );
        } else {
            panic!("Expected Exports edge");
        }
    }

    // ====================================================================
    // Import edge tests (source/. commands)
    // ====================================================================

    #[test]
    fn test_source_creates_import_edges() {
        let source = r"
#!/bin/bash
source ./config.sh
source /etc/profile.sh
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
        assert_has_import_edge(&staging, "test::module", "./config.sh");
        assert_has_import_edge(&staging, "test::module", "/etc/profile.sh");
    }

    #[test]
    fn test_dot_creates_import_edges() {
        let source = r"
#!/bin/bash
. ./init.sh
. config.sh
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
        assert_has_import_edge(&staging, "test::module", "./init.sh");
        assert_has_import_edge(&staging, "test::module", "config.sh");
    }

    #[test]
    fn test_source_inside_function() {
        let source = r"
load_config() {
    source ./config.sh
}
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge");
        assert_has_import_edge(&staging, "load_config", "./config.sh");
    }

    #[test]
    fn test_source_with_variable_expansion() {
        let source = r"
#!/bin/bash
source $CONFIG_DIR/file.sh
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge");
        // Variable expansion is stored as raw text
        assert_has_import_edge(&staging, "test::module", "$CONFIG_DIR/file.sh");
    }

    #[test]
    fn test_source_with_quoted_path() {
        let source = r#"
#!/bin/bash
source "./path with spaces.sh"
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1, "Expected 1 import edge");
        // Quotes should be stripped
        assert_has_import_edge(&staging, "test::module", "./path with spaces.sh");
    }

    #[test]
    fn test_source_does_not_create_call_edge() {
        let source = r"
#!/bin/bash
source ./config.sh
. ./init.sh
";

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // source/. should create import edges, NOT call edges
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            0,
            "source/. commands should not create call edges"
        );

        // But import edges should exist
        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 2, "Expected 2 import edges");
    }

    #[test]
    fn test_builtin_filter_still_works_without_source() {
        // Regression: removing source/. from builtins must not break other builtin filtering
        let source = r#"
my_func() {
    echo "test"
    cd /tmp
    my_helper
}

my_helper() {
    pwd
}
"#;

        let tree = parse_shell(source);
        let mut staging = StagingGraph::new();
        let builder = ShellGraphBuilder::default();
        let file = PathBuf::from("test.sh");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Only user-defined function call should exist
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Expected 1 call edge (only to user function)"
        );
        assert_has_call_edge(&staging, "my_func", "my_helper");

        // No import edges should exist
        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 0, "Expected no import edges");

        // No nodes for builtins
        assert!(
            !staging.nodes().any(|n| staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == "echo" || name == "cd" || name == "pwd")),
            "Builtin commands should not create nodes"
        );
    }
}

/// Per-language [`ShapeMapping`] for shell/bash (identifier-blind body-shape
/// feature).
///
/// Precomputed `kind_id -> CfBucket` table built once from the tree-sitter-bash
/// grammar. Shell functions take no declared parameters (positional `$1`..`$N` are
/// read at runtime), so [`signature_shape`](ShellShapeMapping::signature_shape)
/// is honestly minimal. `break`/`continue`/`return` are ordinary commands in the
/// grammar, so they fall into the `Call` bucket rather than a dedicated kind.
pub struct ShellShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl ShellShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
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
                *slot = cf_bucket_for_shell_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for ShellShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, _fn_node: Node, _src: &[u8]) -> SignatureShape {
        // Shell functions declare no formal parameters; arity is not structurally
        // available, so the honest signature is the default (all zero/false).
        SignatureShape::default()
    }
}

/// Map one tree-sitter-bash node-kind name to its canonical control-flow bucket.
/// Additive-only against the frozen [`CfBucket`] set. `until` loops parse as
/// `while_statement`, so a single Loop arm covers both.
fn cf_bucket_for_shell_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "elif_clause" | "else_clause" | "ternary_expression" => CfBucket::Branch,
        "while_statement" | "for_statement" | "c_style_for_statement" => CfBucket::Loop,
        "case_statement" | "case_item" => CfBucket::Match,
        "command" => CfBucket::Call,
        "variable_assignment" | "declaration_command" => CfBucket::Assign,
        // A nested `function_definition` is a closure-like inner function.
        "function_definition" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide shell shape mapping, built once on first use.
#[must_use]
pub fn shell_shape_mapping() -> &'static ShellShapeMapping {
    static MAPPING: OnceLock<ShellShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(ShellShapeMapping::build)
}

#[cfg(test)]
mod shape_tests {
    //! Coverage for the shell [`ShapeMapping`]. Consumes the hand-written
    //! control-flow fixture so the test is load-bearing.

    use super::{cf_bucket_for_shell_kind, shell_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };
    use tree_sitter::{Node, Parser, Tree};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/dynamic/script.sh"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .expect("load bash grammar");
        parser.parse(src, None).expect("parse bash")
    }

    fn first_function<'t>(tree: &'t Tree) -> Node<'t> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() == "function_definition" {
                return child;
            }
        }
        panic!("no function_definition in shell fixture");
    }

    #[test]
    fn mapping_is_non_empty_and_covers_real_kinds() {
        assert_eq!(
            cf_bucket_for_shell_kind("if_statement"),
            Some(CfBucket::Branch)
        );
        assert_eq!(
            cf_bucket_for_shell_kind("while_statement"),
            Some(CfBucket::Loop)
        );
        assert_eq!(
            cf_bucket_for_shell_kind("for_statement"),
            Some(CfBucket::Loop)
        );
        assert_eq!(
            cf_bucket_for_shell_kind("case_statement"),
            Some(CfBucket::Match)
        );
        assert_eq!(cf_bucket_for_shell_kind("command"), Some(CfBucket::Call));
        assert_eq!(
            cf_bucket_for_shell_kind("variable_assignment"),
            Some(CfBucket::Assign)
        );
        assert_eq!(cf_bucket_for_shell_kind("nope"), None);

        let lang: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
        let id = (0..lang.node_kind_count())
            .map(|i| i as u16)
            .find(|&i| {
                lang.node_kind_is_named(i) && lang.node_kind_for_id(i) == Some("if_statement")
            })
            .expect("grammar exposes named if_statement");
        assert_eq!(shell_shape_mapping().cf_bucket(id), Some(CfBucket::Branch));
    }

    #[test]
    fn descriptor_covers_fixture_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_function(&tree);
        let descriptor = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            shell_shape_mapping(),
            &ShapeBudget::default(),
        );
        let hist = descriptor.cf_histogram;
        assert!(hist[CfBucket::Branch.index()] >= 1, "branch (if/elif)");
        assert!(hist[CfBucket::Loop.index()] >= 1, "loop (while/for/until)");
        assert!(hist[CfBucket::Match.index()] >= 1, "case");
        assert!(hist[CfBucket::Call.index()] >= 1, "command");
        assert!(hist[CfBucket::Assign.index()] >= 1, "assignment");
    }

    #[test]
    fn signature_shape_is_minimal() {
        let tree = parse(SAMPLE);
        let func = first_function(&tree);
        let shape = shell_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // Shell has no declared formal parameters; the honest signature is empty.
        assert_eq!(shape.arity_positional, 0);
        assert_eq!(shape.arity_keyword_only, 0);
        assert!(!shape.has_varargs);
    }
}
