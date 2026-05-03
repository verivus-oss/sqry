use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::{GraphBuildHelper, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Position, Span};
use tree_sitter::{Node, Point, Tree};

use super::type_extractor::{canonical_type_string, extract_type_names};
use super::yard_parser::{extract_yard_comment, parse_yard_tags};

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// File-level module name for exports.
/// In Ruby, public classes, modules, and methods are exported.
const FILE_MODULE_NAME: &str = "<file_module>";

type CallEdgeData = (String, String, usize, Span, bool);

/// Graph builder for Ruby source files.
///
/// This implementation follows the unified `ASTGraph` pattern used by other
/// language plugins. It builds method contexts first, then performs a second
/// traversal to emit call edges, FFI hooks, and call-site metadata.
#[derive(Debug, Clone, Copy)]
pub struct RubyGraphBuilder {
    max_scope_depth: usize,
}

impl Default for RubyGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl RubyGraphBuilder {
    /// Create a builder with custom scope depth.
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for RubyGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Ruby);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Walk tree to find methods, classes, modules, calls, imports, and FFI
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &ast_graph.ffi_enabled_scopes,
        )?;

        apply_controller_dsl_hooks(&ast_graph, &mut helper);

        // Phase: Process YARD annotations for TypeOf and Reference edges
        process_yard_annotations(tree.root_node(), content, &ast_graph, &mut helper)?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Ruby
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Public,
    Protected,
    Private,
}

impl Visibility {
    #[allow(dead_code)] // Reserved for visibility filtering in graph queries
    fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Protected => "protected",
            Visibility::Private => "private",
        }
    }

    fn from_keyword(keyword: &str) -> Option<Self> {
        match keyword {
            "public" => Some(Visibility::Public),
            "protected" => Some(Visibility::Protected),
            "private" => Some(Visibility::Private),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
enum RubyContextKind {
    Method,
    SingletonMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControllerDslKind {
    Before,
    After,
    Around,
}

#[allow(dead_code)] // Scaffolding for Rails controller DSL analysis
#[derive(Debug, Clone)]
struct ControllerDslHook {
    container: String,
    kind: ControllerDslKind,
    callbacks: Vec<String>,
    only: Option<Vec<String>>,   // action filters
    except: Option<Vec<String>>, // action filters
}

#[derive(Debug, Clone)]
struct RubyContext {
    qualified_name: String,
    container: Option<String>,
    kind: RubyContextKind,
    visibility: Visibility,
    start_position: Point,
    end_position: Point,
}

impl RubyContext {
    #[allow(dead_code)] // Reserved for future filtering logic
    fn is_method(&self) -> bool {
        matches!(
            self.kind,
            RubyContextKind::Method | RubyContextKind::SingletonMethod
        )
    }

    fn is_singleton(&self) -> bool {
        matches!(self.kind, RubyContextKind::SingletonMethod)
    }

    fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

struct ASTGraph {
    contexts: Vec<RubyContext>,
    node_to_context: HashMap<usize, usize>,
    attr_visibility: HashMap<usize, Visibility>,
    /// Scopes (namespaces) that have `extend FFI::Library` - used for FFI edge emission
    ffi_enabled_scopes: HashSet<Vec<String>>,
    #[allow(dead_code)] // Reserved for Rails controller DSL analysis
    controller_dsl_hooks: Vec<ControllerDslHook>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Result<Self, String> {
        let mut builder = ContextBuilder::new(content, max_depth)?;
        builder.walk(tree.root_node())?;
        Ok(Self {
            contexts: builder.contexts,
            node_to_context: builder.node_to_context,
            attr_visibility: builder.attr_visibility,
            ffi_enabled_scopes: builder.ffi_enabled_scopes,
            controller_dsl_hooks: builder.controller_dsl_hooks,
        })
    }

    #[allow(dead_code)] // Reserved for future context queries
    fn contexts(&self) -> &[RubyContext] {
        &self.contexts
    }

    fn context_for_node(&self, node: &Node<'_>) -> Option<&RubyContext> {
        self.node_to_context
            .get(&node.id())
            .and_then(|idx| self.contexts.get(*idx))
    }

    fn attr_visibility_for_node(&self, node: &Node<'_>) -> Visibility {
        self.attr_visibility
            .get(&node.id())
            .copied()
            .unwrap_or(Visibility::Public)
    }
}

/// Walk the tree and populate the staging graph.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    ffi_enabled_scopes: &HashSet<Vec<String>>,
) -> GraphResult<()> {
    // Track current namespace for FFI scope detection
    let mut current_namespace: Vec<String> = Vec::new();

    walk_tree_for_graph_impl(
        node,
        content,
        ast_graph,
        helper,
        ffi_enabled_scopes,
        &mut current_namespace,
    )
}

fn apply_controller_dsl_hooks(ast_graph: &ASTGraph, helper: &mut GraphBuildHelper) {
    if ast_graph.controller_dsl_hooks.is_empty() {
        return;
    }

    let mut actions_by_container: HashMap<String, Vec<String>> = HashMap::new();
    for context in &ast_graph.contexts {
        if !matches!(context.kind, RubyContextKind::Method) {
            continue;
        }
        let Some(container) = context.container() else {
            continue;
        };
        let Some(action_name) = context.qualified_name.rsplit('#').next() else {
            continue;
        };
        actions_by_container
            .entry(container.to_string())
            .or_default()
            .push(action_name.to_string());
    }

    let mut emitted: HashSet<(String, String)> = HashSet::new();
    for hook in &ast_graph.controller_dsl_hooks {
        let Some(actions) = actions_by_container.get(&hook.container) else {
            continue;
        };

        for action in actions {
            let included = if let Some(only) = &hook.only {
                only.iter().any(|name| name == action)
            } else if let Some(except) = &hook.except {
                !except.iter().any(|name| name == action)
            } else {
                true
            };

            if !included {
                continue;
            }

            for callback in &hook.callbacks {
                if callback.trim().is_empty() {
                    continue;
                }

                let action_qname = format!("{}#{}", hook.container, action);
                let callback_qname = format!("{}#{}", hook.container, callback);
                if !emitted.insert((action_qname.clone(), callback_qname.clone())) {
                    continue;
                }

                let action_id = helper.ensure_method(&action_qname, None, false, false);
                let callback_id = helper.ensure_method(&callback_qname, None, false, false);
                helper.add_call_edge_full_with_span(action_id, callback_id, 255, false, vec![]);
            }
        }
    }
}

/// Internal implementation that tracks namespace context.
#[allow(
    clippy::too_many_lines,
    reason = "Ruby graph extraction handles DSLs and FFI patterns in one traversal."
)]
fn walk_tree_for_graph_impl(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    ffi_enabled_scopes: &HashSet<Vec<String>>,
    current_namespace: &mut Vec<String>,
) -> GraphResult<()> {
    match node.kind() {
        "class" => {
            // Extract class name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let span = span_from_points(node.start_position(), node.end_position());
                let qualified_name = class_name.to_string();
                let class_id = helper.add_class(&qualified_name, Some(span));

                // Export all classes from the file module
                // In Ruby, all classes are public by default and accessible from outside
                let module_id = helper.add_module(FILE_MODULE_NAME, None);
                helper.add_export_edge(module_id, class_id);

                // Check for superclass (class Foo < Bar)
                if let Some(superclass_node) = node.child_by_field_name("superclass")
                    && let Ok(superclass_name) = superclass_node.utf8_text(content)
                {
                    let superclass_name = superclass_name.trim();
                    if !superclass_name.is_empty() {
                        // Create node for the parent class and add Inherits edge
                        let parent_id = helper.add_class(superclass_name, None);
                        helper.add_inherits_edge(class_id, parent_id);
                    }
                }

                // Push class name to namespace for FFI scope tracking
                current_namespace.push(class_name.trim().to_string());

                // Recurse into children with updated namespace
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_tree_for_graph_impl(
                        child,
                        content,
                        ast_graph,
                        helper,
                        ffi_enabled_scopes,
                        current_namespace,
                    )?;
                }

                current_namespace.pop();
                return Ok(());
            }
        }
        "module" => {
            // Extract module name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(module_name) = name_node.utf8_text(content)
            {
                let span = span_from_points(node.start_position(), node.end_position());
                let qualified_name = module_name.to_string();
                let mod_id = helper.add_module(&qualified_name, Some(span));

                // Export all modules from the file module
                // In Ruby, all modules are public by default and accessible from outside
                let file_module_id = helper.add_module(FILE_MODULE_NAME, None);
                helper.add_export_edge(file_module_id, mod_id);

                // Push module name to namespace for FFI scope tracking
                current_namespace.push(module_name.trim().to_string());

                // Recurse into children with updated namespace
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_tree_for_graph_impl(
                        child,
                        content,
                        ast_graph,
                        helper,
                        ffi_enabled_scopes,
                        current_namespace,
                    )?;
                }

                current_namespace.pop();
                return Ok(());
            }
        }
        "method" | "singleton_method" => {
            // Extract method context from AST graph
            if let Some(context) = ast_graph.context_for_node(&node) {
                let span = span_from_points(context.start_position, context.end_position);

                // Detect async patterns in Ruby (Fiber, Thread, async gem patterns)
                let is_async = detect_async_method(node, content);

                // Extract parameter signature
                let params = node
                    .child_by_field_name("parameters")
                    .and_then(|params_node| extract_method_parameters(params_node, content));

                // Extract return type from type annotations
                let return_type = extract_return_type(node, content);

                // Build complete signature: "params -> return_type" or just "params" or just "-> return_type"
                let signature = match (params.as_ref(), return_type.as_ref()) {
                    (Some(p), Some(r)) => Some(format!("{p} -> {r}")),
                    (Some(p), None) => Some(p.clone()),
                    (None, Some(r)) => Some(format!("-> {r}")),
                    (None, None) => None,
                };

                // Get visibility from context
                let visibility = context.visibility().as_str();

                // Add method node with signature metadata
                let method_id = helper.add_method_with_signature(
                    context.qualified_name(),
                    Some(span),
                    is_async,
                    context.is_singleton(),
                    Some(visibility),
                    signature.as_deref(),
                );

                // Export public methods from file module
                // Private/protected methods should NOT be exported
                if context.visibility() == Visibility::Public {
                    let module_id = helper.add_module(FILE_MODULE_NAME, None);
                    helper.add_export_edge(module_id, method_id);
                }
            }
        }
        "assignment" => {
            // Handle constant assignments (CONSTANT = value)
            if let Some(left_node) = node.child_by_field_name("left")
                && left_node.kind() == "constant"
                && let Ok(const_name) = left_node.utf8_text(content)
            {
                // Create qualified name with namespace
                let qualified_name = if current_namespace.is_empty() {
                    const_name.to_string()
                } else {
                    format!("{}::{}", current_namespace.join("::"), const_name)
                };

                let span = span_from_points(node.start_position(), node.end_position());
                let const_id = helper.add_constant(&qualified_name, Some(span));

                // Export public constants from file module
                let module_id = helper.add_module(FILE_MODULE_NAME, None);
                helper.add_export_edge(module_id, const_id);
            }
        }
        "call" | "command" | "command_call" | "identifier" | "super" => {
            // Check for include/extend statements (mixin pattern)
            if is_include_or_extend_statement(node, content) {
                handle_include_extend(node, content, helper, current_namespace);
            }
            // Ruby allows bare identifier statements like `validate` which can either be a local
            // variable reference or an implicit receiver method call. We only attempt to treat
            // identifiers as calls when they appear in statement position.
            else if node.kind() == "identifier" && !is_statement_identifier_call_candidate(node) {
                // Not a standalone statement; avoid misclassifying identifiers inside expressions.
            } else if is_require_statement(node, content) {
                // Build import edge
                if let Some((from_qname, to_qname)) =
                    build_import_for_staging(node, content, helper.file_path())
                {
                    // Ensure both module nodes exist
                    let from_id = helper.add_import(&from_qname, None);
                    let to_id = helper.add_import(
                        &to_qname,
                        Some(span_from_points(node.start_position(), node.end_position())),
                    );

                    // Add import edge
                    helper.add_import_edge(from_id, to_id);
                }
            } else if is_ffi_attach_function(node, content, ffi_enabled_scopes, current_namespace) {
                // FFI attach_function call - create FfiCall edge
                build_ffi_edge_for_attach_function(node, content, helper, current_namespace);
            } else {
                // Build call edge
                if let Ok(Some((source_qname, target_qname, argument_count, span, is_singleton))) =
                    build_call_for_staging(ast_graph, node, content)
                {
                    // Ensure both nodes exist
                    let source_id = helper.ensure_method(&source_qname, None, false, is_singleton);
                    let target_id =
                        helper.ensure_callee(&target_qname, span, CalleeKindHint::Function);

                    // Add call edge
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
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph_impl(
            child,
            content,
            ast_graph,
            helper,
            ffi_enabled_scopes,
            current_namespace,
        )?;
    }

    Ok(())
}

/// Check if a call is an FFI `attach_function` within an FFI-enabled scope.
///
/// Ruby FFI pattern:
/// ```ruby
/// module MyLib
///   extend FFI::Library
///   ffi_lib 'c'
///   attach_function :puts, [:string], :int
/// end
/// ```
fn is_ffi_attach_function(
    node: Node,
    content: &[u8],
    ffi_enabled_scopes: &HashSet<Vec<String>>,
    current_namespace: &[String],
) -> bool {
    // Extract method name
    let method_name = match node.kind() {
        "command" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok()),
        "call" | "command_call" => node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    };

    let Some(method_name) = method_name else {
        return false;
    };
    let method_name = method_name.trim();
    if !matches!(
        method_name,
        "attach_function" | "attach_variable" | "ffi_lib" | "callback"
    ) {
        return false;
    }

    let receiver = match node.kind() {
        "call" | "command_call" | "method_call" => node
            .child_by_field_name("receiver")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    };
    if let Some(receiver) = receiver {
        let trimmed = receiver.trim();
        if trimmed == "FFI" || trimmed.contains("FFI::Library") || trimmed.starts_with("FFI::") {
            return true;
        }
    }

    ffi_enabled_scopes.contains(current_namespace)
}

/// Build an `FfiCall` edge for an FFI `attach_function` call.
///
/// Extracts the Ruby method name and native function name from:
/// `attach_function :ruby_name, :native_name, [:args], :return_type`
/// or
/// `attach_function :name, [:args], :return_type` (same name for both)
fn build_ffi_edge_for_attach_function(
    node: Node,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    current_namespace: &[String],
) {
    // Extract the function name from the first symbol argument
    let arguments = node.child_by_field_name("arguments");

    // For command nodes, arguments might be inline children
    let func_name = if let Some(args) = arguments {
        extract_first_symbol_from_arguments(args, content)
    } else {
        // Try to find symbol children directly (for command nodes)
        let mut cursor = node.walk();
        let mut found_name = false;
        let mut result = None;
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            // Skip the method name itself
            if !found_name {
                found_name = true;
                continue;
            }
            // First symbol after method name is the function name
            if matches!(child.kind(), "symbol" | "simple_symbol")
                && let Ok(text) = child.utf8_text(content)
            {
                result = Some(text.trim().trim_start_matches(':').to_string());
                break;
            }
        }
        result
    };

    let Some(func_name) = func_name else {
        return;
    };

    // Build qualified caller name (the module containing the FFI binding)
    let caller_name = if current_namespace.is_empty() {
        "<module>".to_string()
    } else {
        current_namespace.join("::")
    };

    // Create caller node (the FFI module)
    let caller_id = helper.add_module(&caller_name, None);

    // Create FFI function node (the native function being bound)
    let ffi_func_name = format!("ffi::{func_name}");
    let span = span_from_points(node.start_position(), node.end_position());
    let ffi_func_id = helper.add_function(&ffi_func_name, Some(span), false, false);

    // Add FfiCall edge with C convention (Ruby FFI uses C ABI)
    helper.add_ffi_edge(caller_id, ffi_func_id, FfiConvention::C);
}

/// Extract the first symbol from arguments (for `attach_function`).
fn extract_first_symbol_from_arguments(arguments: Node, content: &[u8]) -> Option<String> {
    let mut cursor = arguments.walk();
    for child in arguments.children(&mut cursor) {
        if matches!(child.kind(), "symbol" | "simple_symbol")
            && let Ok(text) = child.utf8_text(content)
        {
            return Some(text.trim().trim_start_matches(':').to_string());
        }
        // Handle bare_symbol (just the identifier after :)
        if child.kind() == "bare_symbol"
            && let Ok(text) = child.utf8_text(content)
        {
            return Some(text.trim().to_string());
        }
    }
    None
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<CallEdgeData>> {
    let Some(call_context) = ast_graph.context_for_node(&call_node) else {
        return Ok(None);
    };

    let Some(method_call) = extract_method_call(call_node, content)? else {
        return Ok(None);
    };

    if is_visibility_command(&method_call) {
        return Ok(None);
    }

    let source_qualified = call_context.qualified_name().to_string();
    let target_name = resolve_callee(&method_call, call_context);

    if target_name.is_empty() {
        return Ok(None);
    }

    let span = span_from_node(call_node);
    let argument_count = count_arguments(method_call.arguments, content);
    let is_singleton = call_context.is_singleton();

    Ok(Some((
        source_qualified,
        target_name,
        argument_count,
        span,
        is_singleton,
    )))
}

/// Build import edge information for the staging graph.
fn build_import_for_staging(
    require_node: Node<'_>,
    content: &[u8],
    file_path: &str,
) -> Option<(String, String)> {
    // Extract the method name (require or require_relative)
    let method_name = match require_node.kind() {
        "command" => require_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string()),
        "call" | "method_call" => require_node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string()),
        _ => None,
    };

    let method_name = method_name?;

    // Only handle require and require_relative
    if !matches!(method_name.as_str(), "require" | "require_relative") {
        return None;
    }

    // Extract the module name from arguments
    let arguments = require_node.child_by_field_name("arguments");
    let module_name = if let Some(args) = arguments {
        extract_require_module_name(args, content)
    } else {
        // For command nodes, the first child after the method name is the argument
        let mut cursor = require_node.walk();
        let mut found_name = false;
        let mut result = None;
        for child in require_node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            if !found_name {
                found_name = true;
                continue;
            }
            // This is the argument (string node)
            result = extract_string_content(child, content);
            break;
        }
        result
    };

    let module_name = module_name?;

    if module_name.is_empty() {
        return None;
    }

    // Resolve the import path to a canonical module identifier
    let is_relative = method_name == "require_relative";
    let resolved_path = resolve_ruby_require(&module_name, is_relative, file_path);

    // Return from/to qualified names
    Some(("<module>".to_string(), resolved_path))
}

fn is_statement_identifier_call_candidate(node: Node<'_>) -> bool {
    node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "body_statement" | "program"))
}

/// Detect async patterns in Ruby methods (best-effort detection).
///
/// Ruby doesn't have native async/await, but uses patterns like:
/// - `Fiber` class for coroutines
/// - `Thread` class for threading
/// - `async` gem patterns (`Async do ... end`)
/// - `concurrent-ruby` patterns
///
/// This is a heuristic check looking for async-related keywords in method body.
fn detect_async_method(method_node: Node<'_>, content: &[u8]) -> bool {
    // Get method body
    let body_node = method_node.child_by_field_name("body");
    if body_node.is_none() {
        return false;
    }
    let body_node = body_node.unwrap();

    // Convert body to text and look for async patterns
    if let Ok(body_text) = body_node.utf8_text(content) {
        let body_lower = body_text.to_lowercase();

        // Check for common async patterns
        if body_lower.contains("fiber.")
            || body_lower.contains("fiber.new")
            || body_lower.contains("fiber.yield")
            || body_lower.contains("fiber.resume")
            || body_lower.contains("thread.new")
            || body_lower.contains("thread.start")
            || body_lower.contains("async do")
            || body_lower.contains("async {")
            || body_lower.contains("async.reactor")
            || body_lower.contains("concurrent::")
        {
            return true;
        }
    }

    false
}

/// Check if a Ruby call node is an `include` or `extend` statement (mixin pattern).
fn is_include_or_extend_statement(node: Node<'_>, content: &[u8]) -> bool {
    let method_name = match node.kind() {
        "command" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok()),
        "call" | "method_call" => node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    };

    method_name.is_some_and(|name| matches!(name.trim(), "include" | "extend"))
}

/// Handle `include` or `extend` statements to create Implements edges.
///
/// Ruby mixins work as follows:
/// - `include ModuleName`: Instance methods from module become instance methods
/// - `extend ModuleName`: Instance methods from module become class methods
///
/// Both are represented as Implements edges from the class to the module.
fn handle_include_extend(
    node: Node<'_>,
    content: &[u8],
    helper: &mut sqry_core::graph::unified::GraphBuildHelper,
    current_namespace: &[String],
) {
    // Extract the module name from arguments
    let module_name = if let Some(args) = node.child_by_field_name("arguments") {
        extract_first_constant_from_arguments(args, content)
    } else if node.kind() == "command" {
        // For command nodes, the first named child after the method name is the module
        let mut cursor = node.walk();
        let mut found_method = false;
        let mut result = None;
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            // Skip the method name itself
            if !found_method {
                found_method = true;
                continue;
            }
            // First constant after method name is the module name
            if child.kind() == "constant"
                && let Ok(text) = child.utf8_text(content)
            {
                result = Some(text.trim().to_string());
                break;
            }
        }
        result
    } else {
        None
    };

    let Some(module_name) = module_name else {
        return;
    };

    // Build qualified class name (the class doing the include/extend)
    let class_name = if current_namespace.is_empty() {
        return; // Can't include/extend outside a class
    } else {
        current_namespace.join("::")
    };

    // Create nodes
    let class_id = helper.add_class(&class_name, None);
    let module_id = helper.add_module(&module_name, None);

    // Add Implements edge (class implements module)
    helper.add_implements_edge(class_id, module_id);
}

/// Extract the first constant from an argument list.
fn extract_first_constant_from_arguments(args_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = args_node.walk();
    for child in args_node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        // Look for constant nodes
        if child.kind() == "constant"
            && let Ok(text) = child.utf8_text(content)
        {
            return Some(text.trim().to_string());
        }
    }
    None
}

/// Check if a node is a `require/require_relative` statement.
fn is_require_statement(node: Node<'_>, content: &[u8]) -> bool {
    let method_name = match node.kind() {
        "command" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok()),
        "call" | "method_call" => node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    };

    method_name.is_some_and(|name| matches!(name.trim(), "require" | "require_relative"))
}

struct ContextBuilder<'a> {
    contexts: Vec<RubyContext>,
    node_to_context: HashMap<usize, usize>,
    attr_visibility: HashMap<usize, Visibility>,
    namespace: Vec<String>,
    visibility_stack: Vec<Visibility>,
    ffi_enabled_scopes: HashSet<Vec<String>>,
    controller_dsl_hooks: Vec<ControllerDslHook>,
    max_depth: usize,
    content: &'a [u8],
    guard: sqry_core::query::security::RecursionGuard,
}

impl<'a> ContextBuilder<'a> {
    fn new(content: &'a [u8], max_depth: usize) -> Result<Self, String> {
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        Ok(Self {
            contexts: Vec::new(),
            node_to_context: HashMap::new(),
            attr_visibility: HashMap::new(),
            namespace: Vec::new(),
            visibility_stack: vec![Visibility::Public],
            ffi_enabled_scopes: HashSet::new(),
            controller_dsl_hooks: Vec::new(),
            max_depth,
            content,
            guard,
        })
    }

    /// # Errors
    ///
    /// Returns error if recursion depth exceeds the guard's limit.
    fn walk(&mut self, node: Node<'a>) -> Result<(), String> {
        self.guard
            .enter()
            .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

        match node.kind() {
            "class" => self.visit_class(node)?,
            "module" => self.visit_module(node)?,
            "singleton_class" => self.visit_singleton_class(node)?,
            "method" => self.visit_method(node)?,
            "singleton_method" => self.visit_singleton_method(node)?,
            "command" | "command_call" | "call" => {
                self.detect_ffi_extend(node)?;
                self.detect_controller_dsl(node)?;
                self.record_attr_visibility(node);
                self.adjust_visibility(node)?;
                self.walk_children(node)?;
            }
            "identifier" => {
                // Bare identifiers like `private`, `protected`, `public` at statement level
                // can adjust visibility scope
                self.adjust_visibility_from_identifier(node)?;
                self.walk_children(node)?;
            }
            _ => self.walk_children(node)?,
        }

        self.guard.exit();
        Ok(())
    }

    fn visit_class(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| "class node missing name".to_string())?;
        let class_name = self.node_text(name_node)?;

        if self.namespace.len() > self.max_depth {
            return Ok(());
        }

        self.namespace.push(class_name);
        self.visibility_stack.push(Visibility::Public);

        self.walk_children(node)?;

        self.visibility_stack.pop();
        self.namespace.pop();
        Ok(())
    }

    fn visit_module(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| "module node missing name".to_string())?;
        let module_name = self.node_text(name_node)?;

        if self.namespace.len() > self.max_depth {
            return Ok(());
        }

        self.namespace.push(module_name);
        self.visibility_stack.push(Visibility::Public);

        self.walk_children(node)?;

        self.visibility_stack.pop();
        self.namespace.pop();
        Ok(())
    }

    fn visit_method(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| "method node missing name".to_string())?;
        let method_name = self.node_text(name_node)?;

        let (qualified_name, container) =
            method_qualified_name(&self.namespace, &method_name, false);

        let visibility = inline_visibility_for_method(node, self.content)
            .unwrap_or_else(|| *self.visibility_stack.last().unwrap_or(&Visibility::Public));

        let context = RubyContext {
            qualified_name,
            container,
            kind: RubyContextKind::Method,
            visibility,
            start_position: node.start_position(),
            end_position: node.end_position(),
        };

        let idx = self.contexts.len();
        self.contexts.push(context);
        associate_descendants(node, idx, &mut self.node_to_context);

        self.walk_children(node)?;
        Ok(())
    }

    fn visit_singleton_class(&mut self, node: Node<'a>) -> Result<(), String> {
        // Extract the object: class << self, class << MyClass, etc.
        let value_node = node
            .child_by_field_name("value")
            .ok_or_else(|| "singleton_class missing value".to_string())?;
        let object_text = self.node_text(value_node)?;

        // Determine the scope name for methods in this singleton class
        let scope_name = if object_text == "self" {
            // class << self inside Foo → methods are Foo.method
            if let Some(current_class) = self.namespace.last() {
                format!("<<{current_class}>>")
            } else {
                "<<main>>".to_string()
            }
        } else {
            // class << SomeClass → methods are SomeClass.method
            format!("<<{object_text}>>")
        };

        if self.namespace.len() > self.max_depth {
            return Ok(());
        }

        // Push the singleton scope
        self.namespace.push(scope_name);
        self.visibility_stack.push(Visibility::Public);

        // Walk children, converting methods to singleton methods
        self.visit_singleton_class_body(node)?;

        // Pop the singleton scope
        self.visibility_stack.pop();
        self.namespace.pop();
        Ok(())
    }

    fn visit_singleton_class_body(&mut self, node: Node<'a>) -> Result<(), String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }

            // Methods inside singleton_class are automatically singleton methods
            if child.kind() == "method" {
                self.visit_method_as_singleton(child)?;
            } else {
                self.walk(child)?;
            }
        }
        Ok(())
    }

    fn visit_method_as_singleton(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| "method node missing name".to_string())?;
        let method_name = self.node_text(name_node)?;

        // Build as singleton method - strip the <<...>> wrapper from namespace
        let actual_namespace: Vec<String> = self
            .namespace
            .iter()
            .map(|s| {
                if s.starts_with("<<") && s.ends_with(">>") {
                    // Extract the class name from <<ClassName>>
                    s[2..s.len() - 2].to_string()
                } else {
                    s.clone()
                }
            })
            .collect();

        let (qualified_name, container) =
            method_qualified_name(&actual_namespace, &method_name, true);

        let visibility = inline_visibility_for_method(node, self.content)
            .unwrap_or_else(|| *self.visibility_stack.last().unwrap_or(&Visibility::Public));

        let context = RubyContext {
            qualified_name,
            container,
            kind: RubyContextKind::SingletonMethod,
            visibility,
            start_position: node.start_position(),
            end_position: node.end_position(),
        };

        let idx = self.contexts.len();
        self.contexts.push(context);
        associate_descendants(node, idx, &mut self.node_to_context);

        self.walk_children(node)?;
        Ok(())
    }

    fn visit_singleton_method(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| "singleton_method missing name".to_string())?;
        let method_name = self.node_text(name_node)?;

        let object_node = node
            .child_by_field_name("object")
            .ok_or_else(|| "singleton_method missing object".to_string())?;
        let object_text = self.node_text(object_node)?;

        let (qualified_name, container) =
            singleton_qualified_name(&self.namespace, object_text.trim(), &method_name);

        let visibility = inline_visibility_for_method(node, self.content)
            .unwrap_or_else(|| *self.visibility_stack.last().unwrap_or(&Visibility::Public));

        let context = RubyContext {
            qualified_name,
            container,
            kind: RubyContextKind::SingletonMethod,
            visibility,
            start_position: node.start_position(),
            end_position: node.end_position(),
        };

        let idx = self.contexts.len();
        self.contexts.push(context);
        associate_descendants(node, idx, &mut self.node_to_context);

        self.walk_children(node)?;
        Ok(())
    }

    fn detect_ffi_extend(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node.child_by_field_name("name");
        let Some(name_node) = name_node else {
            return Ok(());
        };

        let keyword = self.node_text(name_node)?;
        if keyword.trim() != "extend" {
            return Ok(());
        }

        let arg_text = if let Some(arguments) = node.child_by_field_name("arguments") {
            node_text_raw(arguments, self.content).unwrap_or_default()
        } else {
            let mut cursor = node.walk();
            let mut found_name = false;
            let mut result = String::new();
            for child in node.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                if !found_name {
                    found_name = true;
                    continue;
                }
                if let Some(text) = node_text_raw(child, self.content) {
                    result = text;
                    break;
                }
            }
            result
        };

        if arg_text.contains("FFI::Library") {
            // Mark current scope as FFI-enabled
            self.ffi_enabled_scopes.insert(self.namespace.clone());
        }

        Ok(())
    }

    fn detect_controller_dsl(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("method"));
        let Some(name_node) = name_node else {
            return Ok(());
        };
        let dsl = self.node_text(name_node)?;

        let kind = match dsl.as_str() {
            "before_action" => Some(ControllerDslKind::Before),
            "after_action" => Some(ControllerDslKind::After),
            "around_action" => Some(ControllerDslKind::Around),
            _ => None,
        };
        let Some(kind) = kind else {
            return Ok(());
        };

        if self.namespace.is_empty() {
            return Ok(());
        }
        let container = self.namespace.join("::");

        let mut callbacks: Vec<String> = Vec::new();
        let mut only: Option<Vec<String>> = None;
        let mut except: Option<Vec<String>> = None;

        if let Some(arguments) = node.child_by_field_name("arguments") {
            let mut cursor = arguments.walk();
            for child in arguments.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                let kind = child.kind();
                match kind {
                    "symbol" | "simple_symbol" | "array" if callbacks.is_empty() => {
                        let mut v = extract_symbols_from_node(child, self.content);
                        callbacks.append(&mut v);
                    }
                    "pair" => {
                        // Handle direct pair node (only: [...] or except: [...])
                        let key = child.child_by_field_name("key");
                        let val = child.child_by_field_name("value");
                        if key.is_none() || val.is_none() {
                            continue;
                        }
                        let key_text = self.node_text(key.unwrap()).unwrap_or_default();
                        let symbols = extract_symbols_from_node(val.unwrap(), self.content);
                        if key_text.contains("only") && !symbols.is_empty() {
                            only = Some(symbols);
                        } else if key_text.contains("except") && !symbols.is_empty() {
                            except = Some(symbols);
                        }
                    }
                    "hash" => {
                        // Parse pairs like only: [:new, :create]
                        let mut hcur = child.walk();
                        for pair in child.children(&mut hcur) {
                            if !pair.is_named() {
                                continue;
                            }
                            if pair.kind() != "pair" {
                                continue;
                            }
                            let key = pair.child_by_field_name("key");
                            let val = pair.child_by_field_name("value");
                            if key.is_none() || val.is_none() {
                                continue;
                            }
                            let key_text = self.node_text(key.unwrap()).unwrap_or_default();
                            let symbols = extract_symbols_from_node(val.unwrap(), self.content);
                            if key_text.contains("only") && !symbols.is_empty() {
                                only = Some(symbols);
                            } else if key_text.contains("except") && !symbols.is_empty() {
                                except = Some(symbols);
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            // Fallback: parse from raw node text
            if let Some(raw) = node_text_raw(node, self.content) {
                let (cbs, o, e) = parse_controller_dsl_args(&raw);
                callbacks = cbs;
                only = o;
                except = e;
            }
        }

        if callbacks.is_empty() {
            return Ok(());
        }

        self.controller_dsl_hooks.push(ControllerDslHook {
            container,
            kind,
            callbacks,
            only,
            except,
        });
        Ok(())
    }

    fn adjust_visibility(&mut self, node: Node<'a>) -> Result<(), String> {
        let name_node = node.child_by_field_name("name");
        let Some(name_node) = name_node else {
            return Ok(());
        };

        let keyword = self.node_text(name_node)?;
        let Some(new_visibility) = Visibility::from_keyword(keyword.trim()) else {
            return Ok(());
        };

        // Only adjust default visibility when command has no arguments.
        if !has_call_arguments(node)
            && let Some(last) = self.visibility_stack.last_mut()
        {
            *last = new_visibility;
        }
        Ok(())
    }

    /// Handle bare identifiers that can be visibility keywords (private, protected, public)
    fn adjust_visibility_from_identifier(&mut self, node: Node<'a>) -> Result<(), String> {
        let keyword = self.node_text(node)?;
        let Some(new_visibility) = Visibility::from_keyword(keyword.trim()) else {
            return Ok(());
        };

        // Bare identifier as statement adjusts visibility for following methods
        if let Some(last) = self.visibility_stack.last_mut() {
            *last = new_visibility;
        }

        Ok(())
    }

    fn record_attr_visibility(&mut self, node: Node<'a>) {
        if !is_attr_call(node, self.content) {
            return;
        }

        let visibility = self
            .visibility_stack
            .last()
            .copied()
            .unwrap_or(Visibility::Public);
        self.attr_visibility.insert(node.id(), visibility);
    }

    fn walk_children(&mut self, node: Node<'a>) -> Result<(), String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                self.walk(child)?;
            }
        }
        Ok(())
    }

    fn node_text(&self, node: Node<'a>) -> Result<String, String> {
        node.utf8_text(self.content)
            .map(|s| s.trim().to_string())
            .map_err(|err| err.to_string())
    }
}

#[derive(Clone)]
struct MethodCall<'a> {
    name: String,
    receiver: Option<String>,
    arguments: Option<Node<'a>>,
    node: Node<'a>,
}

fn extract_method_call<'a>(node: Node<'a>, content: &[u8]) -> GraphResult<Option<MethodCall<'a>>> {
    let method_name = match node.kind() {
        "call" | "command_call" | "method_call" => {
            let method_node = node
                .child_by_field_name("method")
                .ok_or_else(|| builder_parse_error(node, "call node missing method name"))?;
            node_text(method_node, content)?
        }
        "command" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| builder_parse_error(node, "command node missing name"))?;
            node_text(name_node, content)?
        }
        "super" => "super".to_string(),
        "identifier" => {
            if !should_treat_identifier_as_call(node) {
                return Ok(None);
            }
            node_text(node, content)?
        }
        _ => return Ok(None),
    };

    let receiver = match node.kind() {
        "call" | "command_call" | "method_call" => node
            .child_by_field_name("receiver")
            .and_then(|r| node_text(r, content).ok()),
        _ => None,
    };

    let arguments = node.child_by_field_name("arguments");

    Ok(Some(MethodCall {
        name: method_name,
        receiver,
        arguments,
        node,
    }))
}

fn should_treat_identifier_as_call(node: Node<'_>) -> bool {
    if let Some(parent) = node.parent() {
        let kind = parent.kind();
        if matches!(
            kind,
            "call"
                | "command"
                | "command_call"
                | "method_call"
                | "method"
                | "singleton_method"
                | "alias"
                | "symbol"
        ) {
            return false;
        }

        if kind.contains("assignment")
            || matches!(
                kind,
                "parameters"
                    | "method_parameters"
                    | "block_parameters"
                    | "lambda_parameters"
                    | "constant_path"
                    | "module"
                    | "class"
                    | "hash"
                    | "pair"
                    | "array"
                    | "argument_list"
            )
        {
            return false;
        }
    }

    true
}

/// Resolves the fully-qualified name of a method call's target (callee).
///
/// Handles different receiver patterns:
/// - `self.method` → qualified with current container
/// - `Constant.method` → qualified with constant name
/// - Bare `method` → qualified based on context (instance vs singleton)
///
/// # Arguments
/// * `method_call` - The extracted method call information
/// * `context` - The containing Ruby context (method, class, etc.)
///
/// # Returns
/// Fully-qualified callee name, or bare method name if no context available
fn resolve_callee(method_call: &MethodCall<'_>, context: &RubyContext) -> String {
    let name = method_call.name.trim();
    if name.is_empty() {
        return String::new();
    }

    // Special handling for inheritance super calls
    if name == "super" {
        // Use current method's qualified name as the target of the super call
        // This acts as a placeholder that downstream processors can resolve
        // to the actual parent implementation when available.
        return format!("super::{}", context.qualified_name());
    }

    if let Some(receiver) = method_call.receiver.as_deref() {
        let receiver = receiver.trim();
        if receiver == "self" {
            if let Some(container) = context.container() {
                return format!("{container}.{name}");
            }
            return format!("self.{name}");
        }

        if receiver.contains("::") || receiver.starts_with("::") || is_constant(receiver) {
            let cleaned = receiver.trim_start_matches("::");
            // Handle Class.new.method pattern → Class#method (instance method)
            if let Some(class_name) = cleaned.strip_suffix(".new") {
                return format!("{class_name}#{name}");
            }
            return format!("{cleaned}.{name}");
        }

        // Instance variable or expression receiver - fall back to method name.
        return name.to_string();
    }

    if context.is_singleton() {
        if let Some(container) = context.container() {
            return format!("{container}.{name}");
        }
        return name.to_string();
    }

    if let Some(container) = context.container() {
        return format!("{container}#{name}");
    }

    name.to_string()
}

/// Counts the number of actual arguments in a method call.
///
/// Filters out delimiters (parentheses, commas) and empty nodes to count
/// only meaningful arguments.
///
/// # Arguments
/// * `arguments` - Optional `argument_list` AST node
/// * `content` - Source file bytes for text extraction
///
/// # Returns
/// Number of non-empty, non-delimiter arguments
fn count_arguments(arguments: Option<Node<'_>>, content: &[u8]) -> usize {
    let Some(arguments) = arguments else {
        return 0;
    };

    let mut count = 0;
    let mut cursor = arguments.walk();
    for child in arguments.children(&mut cursor) {
        if child.is_named()
            && !is_literal_delimiter(child.kind())
            && node_text(child, content)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        {
            count += 1;
        }
    }
    count
}

/// Associates all descendant AST nodes with a context index.
///
/// Performs a depth-first traversal to map every child node ID to the
/// parent context (method, class, etc.). This enables fast context lookup
/// during call edge extraction.
///
/// # Arguments
/// * `node` - Root AST node to traverse
/// * `idx` - Context index to associate with all descendants
/// * `map` - Mutable `node_id` → `context_index` map
fn associate_descendants(node: Node<'_>, idx: usize, map: &mut HashMap<usize, usize>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        map.insert(current.id(), idx);
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Builds a fully-qualified name for an instance or singleton method.
///
/// Ruby naming conventions:
/// - Instance methods: `Class#method`
/// - Singleton methods: `Class.method`
///
/// # Arguments
/// * `namespace` - Stack of containing modules/classes (e.g., `["Module", "Class"]`)
/// * `method_name` - Base method name
/// * `singleton` - Whether this is a singleton (class) method
///
/// # Returns
/// Tuple of (`qualified_name`, `optional_container`)
fn method_qualified_name(
    namespace: &[String],
    method_name: &str,
    singleton: bool,
) -> (String, Option<String>) {
    if namespace.is_empty() {
        return (method_name.to_string(), None);
    }

    let container = namespace.join("::");
    let qualified = if singleton {
        format!("{container}.{method_name}")
    } else {
        format!("{container}#{method_name}")
    };
    (qualified, Some(container))
}

/// Builds a qualified name for a singleton method definition.
///
/// Handles both `def self.method` and `def SomeClass.method` patterns.
/// Resolves `self` relative to the current namespace.
///
/// # Arguments
/// * `current_namespace` - Current nesting context (modules/classes)
/// * `object_text` - The receiver text ("self" or a constant path)
/// * `method_name` - Base method name
///
/// # Returns
/// Tuple of (`qualified_name`, `optional_container`)
fn singleton_qualified_name(
    current_namespace: &[String],
    object_text: &str,
    method_name: &str,
) -> (String, Option<String>) {
    if object_text == "self" {
        if current_namespace.is_empty() {
            (method_name.to_string(), None)
        } else {
            let container = current_namespace.join("::");
            (format!("{container}.{method_name}"), Some(container))
        }
    } else {
        let parts = split_constant_path(object_text);
        if parts.is_empty() {
            (method_name.to_string(), None)
        } else {
            let container = parts.join("::");
            (format!("{container}.{method_name}"), Some(container))
        }
    }
}

/// Splits a Ruby constant path into individual segments.
///
/// Handles leading `::` for absolute paths and filters empty segments.
///
/// # Examples
/// - `"::Module::Class"` → `["Module", "Class"]`
/// - `"Foo::Bar"` → `["Foo", "Bar"]`
///
/// # Arguments
/// * `path` - Constant path string (e.g., "`Module::Class`")
///
/// # Returns
/// Vector of non-empty path segments
fn split_constant_path(path: &str) -> Vec<String> {
    path.trim()
        .trim_start_matches("::")
        .split("::")
        .filter_map(|seg| {
            let trimmed = seg.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

/// Checks if a string represents a Ruby constant (starts with uppercase).
///
/// Ruby constants must begin with an uppercase ASCII letter.
///
/// # Arguments
/// * `text` - String to test
///
/// # Returns
/// true if text starts with uppercase letter, false otherwise
fn is_constant(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Detects visibility modifier commands without arguments.
///
/// Bare `private`, `public`, or `protected` calls (without method names)
/// are visibility scope changes, not method calls to track as edges.
///
/// # Arguments
/// * `method_call` - Extracted method call
///
/// # Returns
/// true if this is a visibility scope change command
fn is_visibility_command(method_call: &MethodCall<'_>) -> bool {
    matches!(
        method_call.name.as_str(),
        "public" | "private" | "protected"
    ) && method_call.receiver.is_none()
        && !has_call_arguments(method_call.node)
}

/// Checks if a call/command node has any arguments.
///
/// Used to distinguish `private` (scope change) from `private :method_name` (method-level).
///
/// # Arguments
/// * `node` - AST node to check
///
/// # Returns
/// true if node has named argument children
fn has_call_arguments(node: Node<'_>) -> bool {
    if let Some(arguments) = node.child_by_field_name("arguments") {
        let mut cursor = arguments.walk();
        for child in arguments.children(&mut cursor) {
            if child.is_named() {
                return true;
            }
        }
    }
    false
}

fn inline_visibility_for_method(node: Node<'_>, content: &[u8]) -> Option<Visibility> {
    let parent = node.parent()?;
    let visibility_node = match parent.kind() {
        "call" | "command" | "command_call" => parent,
        "argument_list" => parent.parent()?,
        _ => return None,
    };

    if !matches!(visibility_node.kind(), "call" | "command" | "command_call") {
        return None;
    }

    let keyword_node = visibility_node
        .child_by_field_name("name")
        .or_else(|| visibility_node.child_by_field_name("method"))?;
    let keyword = node_text_raw(keyword_node, content)?;
    Visibility::from_keyword(keyword.trim())
}

/// Extracts UTF-8 text for an AST node with error handling.
///
/// Trims whitespace and converts UTF-8 errors to `GraphBuilderError`.
///
/// # Arguments
/// * `node` - AST node to extract text from
/// * `content` - Source file bytes
///
/// # Returns
/// Trimmed text content or error if UTF-8 decoding fails
fn node_text(node: Node<'_>, content: &[u8]) -> Result<String, GraphBuilderError> {
    node.utf8_text(content)
        .map(|s| s.trim().to_string())
        .map_err(|err| builder_parse_error(node, &format!("utf8 error: {err}")))
}

/// Raw text extraction without `GraphBuilderError` conversion
fn node_text_raw(node: Node<'_>, content: &[u8]) -> Option<String> {
    node.utf8_text(content)
        .ok()
        .map(std::string::ToString::to_string)
}

/// Creates a `GraphBuilderError::ParseError` with span information.
///
/// # Arguments
/// * `node` - AST node where error occurred
/// * `reason` - Human-readable error description
///
/// # Returns
/// `ParseError` with node's span and reason message
fn builder_parse_error(node: Node<'_>, reason: &str) -> GraphBuilderError {
    GraphBuilderError::ParseError {
        span: span_from_node(node),
        reason: reason.to_string(),
    }
}

/// Extract parameter signature from `method_parameters` node.
///
/// Handles all Ruby parameter types:
/// - Simple: `x`
/// - Optional: `x = 10`
/// - Splat: `*args`
/// - Keyword: `x:`, `x: 10`
/// - Hash splat: `**kwargs`
/// - Block: `&block`
///
/// # Arguments
/// * `params_node` - The `method_parameters` AST node
/// * `content` - Source file content
///
/// # Returns
/// Comma-separated parameter string, or None if no parameters
#[allow(clippy::match_same_arms)]
fn extract_method_parameters(params_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut params = Vec::new();
    let mut cursor = params_node.walk();

    for child in params_node.named_children(&mut cursor) {
        match child.kind() {
            // Simple parameter: def foo(x)
            // Optional parameter: def foo(x = 10)
            "identifier" | "optional_parameter" => {
                if let Ok(text) = child.utf8_text(content) {
                    params.push(text.to_string());
                }
            }
            // Splat: def foo(*args)
            "splat_parameter" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(content) {
                        params.push(format!("*{name}"));
                    }
                } else if let Ok(text) = child.utf8_text(content) {
                    // Fallback: use full text if no name field
                    params.push(text.to_string());
                }
            }
            // Hash splat: def foo(**kwargs)
            "hash_splat_parameter" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(content) {
                        params.push(format!("**{name}"));
                    }
                } else if let Ok(text) = child.utf8_text(content) {
                    // Fallback: use full text if no name field
                    params.push(text.to_string());
                }
            }
            // Block: def foo(&block)
            "block_parameter" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(content) {
                        params.push(format!("&{name}"));
                    }
                } else if let Ok(text) = child.utf8_text(content) {
                    // Fallback: use full text if no name field
                    params.push(text.to_string());
                }
            }
            // Keyword parameter: def foo(x:, y: 10)
            "keyword_parameter" => {
                if let Ok(text) = child.utf8_text(content) {
                    params.push(text.to_string());
                }
            }
            // Destructured parameter: def foo((a, b))
            "destructured_parameter" => {
                if let Ok(text) = child.utf8_text(content) {
                    params.push(text.to_string());
                }
            }
            // Forward parameter: def foo(...)
            "forward_parameter" => {
                params.push("...".to_string());
            }
            // Hash splat nil: def foo(**nil)
            "hash_splat_nil" => {
                params.push("**nil".to_string());
            }
            _ => {
                // Ignore other node types (e.g., punctuation)
            }
        }
    }

    if params.is_empty() {
        None
    } else {
        Some(params.join(", "))
    }
}

/// Extract return type from method definition.
///
/// Attempts to parse return type annotations from:
/// 1. Sorbet sig blocks: `sig { returns(Type) }`
/// 2. RBS inline comments: `#: (...) -> Type`
/// 3. YARD documentation: `@return [Type]`
///
/// # Arguments
/// * `method_node` - The method definition AST node
/// * `content` - Source file content
///
/// # Returns
/// Return type string if found, None otherwise
fn extract_return_type(method_node: Node<'_>, content: &[u8]) -> Option<String> {
    // Try Sorbet first
    if let Some(return_type) = extract_sorbet_return_type(method_node, content) {
        return Some(return_type);
    }

    // Try RBS inline comment
    if let Some(return_type) = extract_rbs_return_type(method_node, content) {
        return Some(return_type);
    }

    // Try YARD documentation
    if let Some(return_type) = extract_yard_return_type(method_node, content) {
        return Some(return_type);
    }

    None
}

/// Extract return type from Sorbet sig block.
///
/// Looks for `sig { returns(Type) }` before method definition.
///
/// # Arguments
/// * `method_node` - The method definition AST node
/// * `content` - Source file content
///
/// # Returns
/// Return type if found in sig block
fn extract_sorbet_return_type(method_node: Node<'_>, content: &[u8]) -> Option<String> {
    // Look for previous sibling that is a call to 'sig'
    let mut sibling = method_node.prev_sibling()?;

    // Skip comments and whitespace
    while sibling.kind() == "comment" {
        sibling = sibling.prev_sibling()?;
    }

    // Check if this is a sig call
    if sibling.kind() == "call"
        && let Some(method_name) = sibling.child_by_field_name("method")
        && let Ok(name_text) = method_name.utf8_text(content)
        && name_text == "sig"
    {
        // Look for block with returns call
        if let Some(block_node) = sibling.child_by_field_name("block") {
            return extract_returns_from_sig_block(block_node, content);
        }
    }

    None
}

/// Extract return type from sig block's `returns()` call.
fn extract_returns_from_sig_block(block_node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = block_node.walk();

    for child in block_node.named_children(&mut cursor) {
        if child.kind() == "call"
            && let Some(method_name) = child.child_by_field_name("method")
            && let Ok(name_text) = method_name.utf8_text(content)
            && name_text == "returns"
        {
            // Get the argument to returns()
            if let Some(args) = child.child_by_field_name("arguments") {
                let mut args_cursor = args.walk();
                for arg in args.named_children(&mut args_cursor) {
                    if arg.kind() != ","
                        && let Ok(type_text) = arg.utf8_text(content)
                    {
                        return Some(type_text.to_string());
                    }
                }
            }
        }
        // Recursively search in nested structures
        if let Some(nested_type) = extract_returns_from_sig_block(child, content) {
            return Some(nested_type);
        }
    }

    None
}

/// Extract return type from RBS inline comment.
///
/// Looks for `#: (...) -> Type` pattern as a child of method node.
///
/// # Arguments
/// * `method_node` - The method definition AST node
/// * `content` - Source file content
///
/// # Returns
/// Return type if found in RBS comment
fn extract_rbs_return_type(method_node: Node<'_>, content: &[u8]) -> Option<String> {
    // RBS comments are children of the method node
    let mut cursor = method_node.walk();
    for child in method_node.children(&mut cursor) {
        if child.kind() == "comment"
            && let Ok(comment_text) = child.utf8_text(content)
        {
            // Parse RBS inline comment: #: (...) -> Type
            // Require #: prefix to avoid false positives from regular comments
            if comment_text.trim_start().starts_with("#:") {
                // Find the top-level arrow (not nested inside parens/brackets/braces)
                if let Some(arrow_pos) = find_top_level_arrow(comment_text) {
                    let return_part = &comment_text[arrow_pos + 2..];
                    let return_type = return_part.trim().to_string();
                    if !return_type.is_empty() {
                        return Some(return_type);
                    }
                }
            }
        }
    }

    None
}

/// Find the position of the top-level `->` arrow (not nested in parens/brackets/braces).
///
/// Tracks depth of (), [], and {} to avoid selecting arrows inside nested types like proc types.
fn find_top_level_arrow(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut depth: i32 = 0;
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '-' if i + 1 < chars.len() && chars[i + 1] == '>' && depth == 0 => {
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }

    None
}

/// Extract return type from YARD documentation.
///
/// Looks for `@return [Type]` in comment block before method.
///
/// # Arguments
/// * `method_node` - The method definition AST node
/// * `content` - Source file content
///
/// # Returns
/// Return type if found in YARD comment
fn extract_yard_return_type(method_node: Node<'_>, content: &[u8]) -> Option<String> {
    // Look for comment block before method
    let mut sibling_opt = method_node.prev_sibling();
    let method_start_row = method_node.start_position().row;

    // Collect all preceding comments that are adjacent to the method
    let mut comments = Vec::new();
    let mut expected_row = method_start_row;

    while let Some(sibling) = sibling_opt {
        if sibling.kind() == "comment" {
            let comment_end_row = sibling.end_position().row;

            // Check adjacency: comment should end on the line right before expected row
            // Allow at most 1 line gap (expected_row - 1 or expected_row)
            if comment_end_row + 1 >= expected_row {
                if let Ok(comment_text) = sibling.utf8_text(content) {
                    comments.push(comment_text);
                }
                expected_row = sibling.start_position().row;
                sibling_opt = sibling.prev_sibling();
            } else {
                // Gap too large, stop collecting
                break;
            }
        } else {
            break;
        }
    }

    // Search for @return [Type] pattern (reverse order since we collected backwards)
    for comment in comments.iter().rev() {
        if let Some(return_pos) = comment.find("@return") {
            let after_return = &comment[return_pos + 7..];
            // Find [Type] pattern
            if let Some(start_bracket) = after_return.find('[')
                && let Some(end_bracket) = after_return.find(']')
                && end_bracket > start_bracket
            {
                let return_type = &after_return[start_bracket + 1..end_bracket];
                return Some(return_type.trim().to_string());
            }
        }
    }

    None
}

/// Converts a tree-sitter Node to a sqry Span.
///
/// # Arguments
/// * `node` - AST node
///
/// # Returns
/// Span with start/end positions
fn span_from_node(node: Node<'_>) -> Span {
    span_from_points(node.start_position(), node.end_position())
}

/// Converts tree-sitter Points to a sqry Span.
///
/// # Arguments
/// * `start` - Start position (row, column)
/// * `end` - End position (row, column)
///
/// # Returns
/// Span covering the range
fn span_from_points(start: Point, end: Point) -> Span {
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

/// Checks if a node kind is a literal syntax delimiter.
///
/// Delimiters (parentheses, commas, brackets) are filtered when counting arguments.
///
/// # Arguments
/// * `kind` - Node kind string
///
/// # Returns
/// true if kind is a delimiter
fn is_literal_delimiter(kind: &str) -> bool {
    matches!(kind, "," | "(" | ")" | "[" | "]")
}

/// Parse controller DSL arguments using simple heuristics.
/// Returns (callbacks, only, except).
fn parse_controller_dsl_args(
    text: &str,
) -> (Vec<String>, Option<Vec<String>>, Option<Vec<String>>) {
    // Split callbacks head from kwargs tail (only:/except:)
    let mut head = text;
    let mut tail = "";
    if let Some(idx) = text.find("only:") {
        head = &text[..idx];
        tail = &text[idx..];
    } else if let Some(idx) = text.find("except:") {
        head = &text[..idx];
        tail = &text[idx..];
    }
    let callbacks = extract_symbol_list_from_args(head);
    let only = extract_kw_symbol_list(tail, "only:");
    let except = extract_kw_symbol_list(tail, "except:");
    (callbacks, only, except)
}

fn extract_symbol_list_from_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() {
                let c = bytes[j] as char;
                if c.is_ascii_alphanumeric() || c == '_' {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > start {
                out.push(text[start..j].to_string());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn extract_kw_symbol_list(text: &str, kw: &str) -> Option<Vec<String>> {
    let pos = text.find(kw)?;
    let mut after = &text[pos + kw.len()..];
    // trim leading whitespace and commas
    after = after.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    if after.starts_with('[')
        && let Some(end) = after.find(']')
    {
        return Some(extract_symbol_list_from_args(&after[..=end]));
    }
    // single symbol
    if let Some(colon) = after.find(':') {
        let mut j = colon + 1;
        while j < after.len() {
            let ch = after.as_bytes()[j] as char;
            if ch.is_ascii_alphanumeric() || ch == '_' {
                j += 1;
            } else {
                break;
            }
        }
        if j > colon + 1 {
            return Some(vec![after[colon + 1..j].to_string()]);
        }
    }
    None
}

fn extract_symbols_from_node(node: Node<'_>, content: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    match node.kind() {
        "symbol" | "simple_symbol" => {
            if let Ok(t) = node_text(node, content) {
                out.push(t.trim_start_matches(':').to_string());
            }
        }
        "array" => {
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                if matches!(ch.kind(), "symbol" | "simple_symbol")
                    && let Ok(t) = node_text(ch, content)
                {
                    out.push(t.trim_start_matches(':').to_string());
                }
            }
        }
        _ => {
            // For other nodes, fall back to text scan
            if let Some(txt) = node_text_raw(node, content) {
                out = extract_symbol_list_from_args(&txt);
            }
        }
    }
    out
}

/// Extract the module name from require arguments
fn extract_require_module_name(arguments: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = arguments.walk();
    for child in arguments.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if let Some(s) = extract_string_content(child, content) {
            return Some(s);
        }
    }
    None
}

/// Extract string content from a string node (handles quotes)
fn extract_string_content(node: Node<'_>, content: &[u8]) -> Option<String> {
    let text = node.utf8_text(content).ok()?;
    let trimmed = text.trim();

    // Handle quoted strings
    if ((trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
        && trimmed.len() >= 2
    {
        return Some(trimmed[1..trimmed.len() - 1].to_string());
    }

    // For string nodes, look for string_content child
    if matches!(node.kind(), "string" | "chained_string") {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "string_content"
                && let Ok(s) = child.utf8_text(content)
            {
                return Some(s.to_string());
            }
        }
    }

    None
}

/// Resolve Ruby require path to canonical identifier
///
/// For `require_relative`, incorporates the source file's directory to produce
/// a unique, stable identifier that won't collide across different directories.
///
/// # Examples
///
/// - `a/file.rb` with `require_relative "util"` -> `a::util`
/// - `b/file.rb` with `require_relative "util"` -> `b::util`
/// - `require "json"` -> `json`
pub(crate) fn resolve_ruby_require(
    module_name: &str,
    is_relative: bool,
    source_file: &str,
) -> String {
    if is_relative {
        // For require_relative, resolve relative to the source file's directory.
        // This ensures `a/file.rb: require_relative "util"` and `b/file.rb: require_relative "util"`
        // produce distinct identifiers `a::util` and `b::util`.
        let source_path = std::path::Path::new(source_file);
        let source_dir = source_path.parent().unwrap_or(std::path::Path::new(""));

        // Join the source directory with the relative path
        let relative_path = std::path::Path::new(module_name);
        let resolved = source_dir.join(relative_path);

        // Normalize to collapse `.` and `..` components
        let normalized = normalize_path(&resolved);

        // Convert to canonical identifier using :: separators
        // Handle both Unix (/) and Windows (\) path separators
        let path_str = normalized.to_string_lossy();
        let separators: &[char] = &['/', '\\'];
        path_str
            .split(separators)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::")
    } else {
        // require 'json' -> json
        // require 'active_support/core_ext' -> active_support::core_ext
        module_name.replace('/', "::")
    }
}

/// Normalize a path by resolving `.` and `..` components without filesystem access
fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {
                // Skip `.` components
            }
            std::path::Component::ParentDir => {
                // Pop for `..` if there's something to pop (and it's not another ..)
                if components
                    .last()
                    .is_some_and(|c| *c != std::path::Component::ParentDir)
                {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => {
                components.push(component);
            }
        }
    }

    components.iter().collect()
}

// ============================================================================
// YARD Annotation Processing - TypeOf and Reference Edges
// ============================================================================

/// Process YARD annotations for `TypeOf` and Reference edges
/// Recursively walks the tree looking for nodes with YARD comments
fn process_yard_annotations(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    match node.kind() {
        "method" => {
            process_method_yard(node, content, helper)?;
        }
        "singleton_method" => {
            process_singleton_method_yard(node, content, helper)?;
        }
        "call" | "command" | "command_call" => {
            // Check if this is an attr_reader/attr_writer/attr_accessor call
            if is_attr_call(node, content) {
                process_attr_yard(node, content, ast_graph, helper)?;
            }
        }
        "assignment" => {
            // Check if this is an instance variable assignment
            if is_instance_variable_assignment(node, content) {
                process_assignment_yard(node, content, helper)?;
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        process_yard_annotations(child, content, ast_graph, helper)?;
    }

    Ok(())
}

/// Process YARD for method definitions
fn process_method_yard(
    method_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract YARD comment
    let Some(yard_text) = extract_yard_comment(method_node, content) else {
        return Ok(());
    };

    // Parse YARD tags
    let tags = parse_yard_tags(&yard_text);

    // Get method name
    let Some(name_node) = method_node.child_by_field_name("name") else {
        return Ok(());
    };

    let method_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(method_node),
            reason: "failed to read method name".to_string(),
        })?
        .trim()
        .to_string();

    if method_name.is_empty() {
        return Ok(());
    }

    // Find the class name by walking up the tree
    let class_name = get_enclosing_class_name(method_node, content);

    // Create qualified method name
    let qualified_name = if let Some(class_name) = class_name {
        format!("{class_name}#{method_name}")
    } else {
        method_name.clone()
    };

    // Get or create method node
    let method_node_id = helper.ensure_method(&qualified_name, None, false, false);

    // Process @param tags
    for (param_idx, param_tag) in tags.params.iter().enumerate() {
        // Create TypeOf edge: method -> parameter type
        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            param_idx.try_into().ok(),
            Some(&param_tag.name),
        );

        // Create Reference edges: method -> each referenced type
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    // Process @return tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges for return type
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process YARD for singleton method definitions (class methods)
fn process_singleton_method_yard(
    method_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract YARD comment
    let Some(yard_text) = extract_yard_comment(method_node, content) else {
        return Ok(());
    };

    // Parse YARD tags
    let tags = parse_yard_tags(&yard_text);

    // Get method name
    let Some(name_node) = method_node.child_by_field_name("name") else {
        return Ok(());
    };

    let method_name = name_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(method_node),
            reason: "failed to read method name".to_string(),
        })?
        .trim()
        .to_string();

    if method_name.is_empty() {
        return Ok(());
    }

    // Find the class name
    let class_name = get_enclosing_class_name(method_node, content);

    // Create qualified method name (singleton methods use . separator)
    let qualified_name = if let Some(class_name) = class_name {
        format!("{class_name}.{method_name}")
    } else {
        method_name.clone()
    };

    // Get or create method node (singleton method)
    let method_node_id = helper.ensure_method(&qualified_name, None, false, true);

    // Process @param tags
    for (param_idx, param_tag) in tags.params.iter().enumerate() {
        // Create TypeOf edge: method -> parameter type
        let canonical_type = canonical_type_string(&param_tag.type_str);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Parameter),
            param_idx.try_into().ok(),
            Some(&param_tag.name),
        );

        // Create Reference edges: method -> each referenced type
        let type_names = extract_type_names(&param_tag.type_str);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    // Process @return tag
    if let Some(return_type) = &tags.returns {
        let canonical_type = canonical_type_string(return_type);
        let type_node_id = helper.add_type(&canonical_type, None);
        helper.add_typeof_edge_with_context(
            method_node_id,
            type_node_id,
            Some(TypeOfContext::Return),
            Some(0),
            None,
        );

        // Create Reference edges for return type
        let type_names = extract_type_names(return_type);
        for type_name in type_names {
            let ref_type_id = helper.add_type(&type_name, None);
            helper.add_reference_edge(method_node_id, ref_type_id);
        }
    }

    Ok(())
}

/// Process `attr_reader` / `attr_writer` / `attr_accessor` declarations.
///
/// Emission is **unconditional** per the cross-language field-emission
/// contract (DAG U06 / `C2_OTHER_RUBY`, design §4.5):
///
/// - `attr_reader`                    → `NodeKind::Constant`
/// - `attr_writer` / `attr_accessor`  → `NodeKind::Property`
/// - Qualified name retains the Ruby `Class#attr` idiom (§3.1.3); the
///   shared canonicalizer rewrites `#` → `::` for graph identity, but
///   the builder seam still spells `#` to match Ruby source intent and
///   the planner U15 lock-in test.
/// - `is_static = false`; visibility follows Ruby method visibility scope:
///   `public` by default, then `private` / `protected` after visibility
///   commands in the enclosing class/module body.
/// - YARD `@return [Type]` is preserved purely as **type enrichment**
///   for the `TypeOf{Field}` edge (with `TypeOfContext::Field` and the
///   bare attr name as edge metadata) and for `References` edges to
///   nested type identifiers; its absence does **not** suppress node
///   emission.
#[allow(clippy::unnecessary_wraps)]
fn process_attr_yard(
    attr_node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Determine the attr_* variant so we can branch the NodeKind. The
    // `is_attr_call` check at the call site has already guaranteed this is
    // one of attr_reader/attr_writer/attr_accessor, so a missing/unknown
    // method name here is a no-op (defensive).
    let Some(method_name) = attr_method_name(attr_node, content) else {
        return Ok(());
    };
    let is_reader = method_name == "attr_reader";

    // Extract attribute names from the call arguments. Empty argument lists
    // (syntactically invalid but still parseable) emit nothing.
    let attr_names = extract_attr_names(attr_node, content);
    if attr_names.is_empty() {
        return Ok(());
    }

    // Find the enclosing class/module namespace (may be None at top level).
    let class_name = get_enclosing_class_name(attr_node, content);

    // Extract optional YARD type-tag for enrichment. Absence is fine — node
    // emission proceeds either way.
    let yard_return = extract_yard_comment(attr_node, content)
        .map(|yard_text| parse_yard_tags(&yard_text))
        .and_then(|tags| tags.returns);

    // Span anchored on the attr_* call node so the field-emission edges and
    // node carry useful source coordinates (was None pre-fix).
    let span = span_from_node(attr_node);
    let visibility = ast_graph.attr_visibility_for_node(&attr_node).as_str();

    for attr_name in attr_names {
        // Build the qualified name in the Ruby `Class#attr` idiom. The
        // helper canonicalizes `#` → `::` internally; passing `#` keeps
        // the design §3.1.3 deviation visible at this seam.
        let qualified_name = if let Some(ref class) = class_name {
            format!("{class}#{attr_name}")
        } else {
            attr_name.clone()
        };

        // Branch the node kind on the attr_* method. is_static is always
        // false (attr_* are instance members); visibility follows the active
        // Ruby accessor-method visibility scope.
        let attr_node_id = if is_reader {
            helper.add_constant_with_static_and_visibility(
                &qualified_name,
                Some(span),
                false,
                Some(visibility),
            )
        } else {
            helper.add_property_with_static_and_visibility(
                &qualified_name,
                Some(span),
                false,
                Some(visibility),
            )
        };

        // YARD type-tag enrichment is opportunistic: if present, emit the
        // TypeOf{Field} edge with bare attr name + Field context, plus
        // References for any nested identifiers in the type expression.
        if let Some(var_type) = &yard_return {
            let canonical_type = canonical_type_string(var_type);
            let type_node_id = helper.add_type(&canonical_type, None);
            helper.add_typeof_edge_with_context(
                attr_node_id,
                type_node_id,
                Some(TypeOfContext::Field),
                None,
                Some(&attr_name),
            );

            for type_name in extract_type_names(var_type) {
                let ref_type_id = helper.add_type(&type_name, None);
                helper.add_reference_edge(attr_node_id, ref_type_id);
            }
        }
    }

    Ok(())
}

/// Resolve the method name of an `attr_*` `call/command/command_call` node so
/// the caller can branch the emitted `NodeKind` (reader → Constant,
/// writer/accessor → Property).
fn attr_method_name(node: Node, content: &[u8]) -> Option<String> {
    let raw = match node.kind() {
        "command" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok()),
        "call" | "command_call" => node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    }?;
    Some(raw.trim().to_string())
}

/// Process YARD for instance variable assignments
fn process_assignment_yard(
    assignment_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    // Extract YARD comment
    let Some(yard_text) = extract_yard_comment(assignment_node, content) else {
        return Ok(());
    };

    // Parse YARD tags
    let tags = parse_yard_tags(&yard_text);

    // Only process @type tags for assignments
    let Some(var_type) = &tags.type_annotation else {
        return Ok(());
    };

    // Get the variable name (instance variable like @username)
    let Some(left_node) = assignment_node.child_by_field_name("left") else {
        return Ok(());
    };

    if left_node.kind() != "instance_variable" {
        return Ok(());
    }

    let var_name = left_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(assignment_node),
            reason: "failed to read variable name".to_string(),
        })?
        .trim()
        .to_string();

    if var_name.is_empty() {
        return Ok(());
    }

    // Find the class name
    let class_name = get_enclosing_class_name(assignment_node, content);

    // Create qualified name for the instance variable
    let qualified_name = if let Some(class) = class_name {
        format!("{class}#{var_name}")
    } else {
        var_name.clone()
    };

    // Create variable node
    let var_node_id = helper.add_variable(&qualified_name, None);

    // Create TypeOf edge: variable -> type
    let canonical_type = canonical_type_string(var_type);
    let type_node_id = helper.add_type(&canonical_type, None);
    helper.add_typeof_edge_with_context(
        var_node_id,
        type_node_id,
        Some(TypeOfContext::Variable),
        None,
        Some(&var_name),
    );

    // Create Reference edges: variable -> each referenced type
    let type_names = extract_type_names(var_type);
    for type_name in type_names {
        let ref_type_id = helper.add_type(&type_name, None);
        helper.add_reference_edge(var_node_id, ref_type_id);
    }

    Ok(())
}

/// Check if a node is an `attr_reader/attr_writer/attr_accessor` call
fn is_attr_call(node: Node, content: &[u8]) -> bool {
    let method_name = match node.kind() {
        "command" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok()),
        "call" | "command_call" => node
            .child_by_field_name("method")
            .and_then(|n| n.utf8_text(content).ok()),
        _ => None,
    };

    method_name
        .is_some_and(|name| matches!(name.trim(), "attr_reader" | "attr_writer" | "attr_accessor"))
}

/// Check if an assignment is to an instance variable
fn is_instance_variable_assignment(node: Node, _content: &[u8]) -> bool {
    if let Some(left_node) = node.child_by_field_name("left") {
        left_node.kind() == "instance_variable"
    } else {
        false
    }
}

/// Extract attribute names from `attr_reader/attr_writer/attr_accessor` arguments
/// Supports both symbol and string arguments
fn extract_attr_names(attr_node: Node, content: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    // Get arguments (could be inline for command nodes)
    let arguments = attr_node.child_by_field_name("arguments");

    if let Some(args) = arguments {
        // Process argument list
        let mut cursor = args.walk();
        for child in args.children(&mut cursor) {
            if matches!(child.kind(), "symbol" | "simple_symbol")
                && let Ok(text) = child.utf8_text(content)
            {
                let cleaned = text.trim().trim_start_matches(':');
                if !cleaned.is_empty() {
                    names.push(cleaned.to_string());
                }
            } else if child.kind() == "string"
                && let Ok(text) = child.utf8_text(content)
            {
                // Handle string arguments: attr_reader "name"
                let cleaned = text
                    .trim()
                    .trim_start_matches(['\'', '"'])
                    .trim_end_matches(['\'', '"']);
                if !cleaned.is_empty() {
                    names.push(cleaned.to_string());
                }
            }
        }
    } else if matches!(attr_node.kind(), "command" | "command_call") {
        // For command/command_call nodes, symbols/strings might be direct children
        let mut cursor = attr_node.walk();
        let mut found_method = false;
        for child in attr_node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            // Skip the method name itself
            if !found_method {
                found_method = true;
                continue;
            }
            // Extract symbol arguments
            if matches!(child.kind(), "symbol" | "simple_symbol")
                && let Ok(text) = child.utf8_text(content)
            {
                let cleaned = text.trim().trim_start_matches(':');
                if !cleaned.is_empty() {
                    names.push(cleaned.to_string());
                }
            } else if child.kind() == "string"
                && let Ok(text) = child.utf8_text(content)
            {
                // Handle string arguments: attr_reader "name"
                let cleaned = text
                    .trim()
                    .trim_start_matches(['\'', '"'])
                    .trim_end_matches(['\'', '"']);
                if !cleaned.is_empty() {
                    names.push(cleaned.to_string());
                }
            }
        }
    }

    names
}

/// Get the fully qualified enclosing class/module name for a node
/// Returns the full namespace path (e.g., "`MyModule::MyClass`") by walking
/// up the parent chain and collecting all enclosing module/class names.
/// Handles absolute constants (e.g., `class ::Foo`) by detecting leading `::`.
fn get_enclosing_class_name(node: Node, content: &[u8]) -> Option<String> {
    let mut current = node;
    let mut namespace_parts = Vec::new();

    // Walk up the tree to collect all enclosing class/module names
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), "class" | "module") {
            // Extract the name of this class/module
            if let Some(name_node) = parent.child_by_field_name("name")
                && let Ok(name_text) = name_node.utf8_text(content)
            {
                let trimmed = name_text.trim();
                // Check for absolute constant (starts with ::)
                if trimmed.starts_with("::") {
                    // Absolute constant - stop accumulating parents
                    namespace_parts.clear();
                    namespace_parts.push(trimmed.trim_start_matches("::").to_string());
                    break;
                }
                // Add to the beginning of the list (outermost first)
                namespace_parts.insert(0, trimmed.to_string());
            }
        }
        current = parent;
    }

    // Join all namespace parts with "::"
    if namespace_parts.is_empty() {
        None
    } else {
        Some(namespace_parts.join("::"))
    }
}

#[cfg(test)]
mod field_emission_tests {
    //! Tests for unconditional Property/Constant emission from Ruby
    //! `attr_reader` / `attr_writer` / `attr_accessor` invocations
    //! (DAG U06 / `C2_OTHER_RUBY`).
    //!
    //! Post-fix contract:
    //! - YARD gate removed: emission is unconditional on `attr_*`.
    //! - `attr_reader` -> `Constant`; `attr_writer` / `attr_accessor` -> `Property`.
    //! - Qualified name retains the Ruby `Class#attr` idiom (per design §3.1.3 +
    //!   planner U15 lock-in test). The shared canonicalizer rewrites `#` to
    //!   `::` for graph identity, but the builder still passes `Class#attr`.
    //! - YARD `@return [Type]` survives as enrichment for the `TypeOf{Field}`
    //!   edge (with `TypeOfContext::Field` and the bare attr name as edge
    //!   metadata), NOT as the emission gate.
    //! - `is_static = false`; visibility follows Ruby accessor-method scope:
    //!   `public` by default, then `private` / `protected` after visibility
    //!   commands in the enclosing class/module body.

    use sqry_core::graph::GraphBuilder;
    use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
    use sqry_core::graph::unified::build::test_helpers::{
        build_node_name_lookup, build_string_lookup,
    };
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::edge::kind::TypeOfContext;
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::Path;
    use tree_sitter::Parser;

    use super::RubyGraphBuilder;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .expect("load Ruby grammar");
        parser.parse(source, None).expect("parse Ruby source")
    }

    fn build(source: &str) -> StagingGraph {
        let tree = parse(source);
        let mut staging = StagingGraph::new();
        let builder = RubyGraphBuilder::default();
        builder
            .build_graph(&tree, source.as_bytes(), Path::new("test.rb"), &mut staging)
            .expect("build graph");
        staging
    }

    /// Look up a node entry by its qualified-or-bare canonical name, optionally
    /// requiring a kind. Note: Ruby canonicalization rewrites `#` -> `::`, so
    /// a builder-side `User#name` becomes `User::name` here.
    fn find_node<'a>(
        staging: &'a StagingGraph,
        name: &str,
        kind: Option<NodeKind>,
    ) -> Option<&'a sqry_core::graph::unified::storage::NodeEntry> {
        let strings = build_string_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op {
                if let Some(k) = kind
                    && entry.kind != k
                {
                    continue;
                }
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                if let Some(s) = strings.get(&name_idx)
                    && s == name
                {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn count_nodes_named(staging: &StagingGraph, name: &str) -> usize {
        let strings = build_string_lookup(staging);
        staging
            .operations()
            .iter()
            .filter(|op| {
                if let StagingOp::AddNode { entry, .. } = op {
                    let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                    strings.get(&name_idx).is_some_and(|s| s == name)
                } else {
                    false
                }
            })
            .count()
    }

    fn visibility(
        staging: &StagingGraph,
        entry: &sqry_core::graph::unified::storage::NodeEntry,
    ) -> Option<String> {
        let strings = build_string_lookup(staging);
        entry
            .visibility
            .and_then(|visibility_id| strings.get(&visibility_id.index()).cloned())
    }

    fn typeof_edges_for_node(
        staging: &StagingGraph,
        source_name: &str,
    ) -> Vec<(Option<TypeOfContext>, Option<String>, String)> {
        let names = build_node_name_lookup(staging);
        let strings = build_string_lookup(staging);
        let mut out = Vec::new();
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::TypeOf { context, name, .. },
                ..
            } = op
            {
                let src = names.get(source).cloned().unwrap_or_default();
                if src != source_name {
                    continue;
                }
                let edge_name = name.and_then(|sid| strings.get(&sid.index()).cloned());
                let target_name = names.get(target).cloned().unwrap_or_default();
                out.push((*context, edge_name, target_name));
            }
        }
        out
    }

    // -- AC-1: YARD gate removed -------------------------------------------

    #[test]
    fn req_r0001_attr_accessor_without_yard_emits_property_node() {
        let src = "class Foo\n  attr_accessor :x\nend\n";
        let staging = build(src);
        find_node(&staging, "Foo::x", Some(NodeKind::Property))
            .expect("Foo#x Property must be emitted without YARD");
    }

    #[test]
    fn req_r0001_attr_reader_without_yard_emits_constant_node() {
        let src = "class Foo\n  attr_reader :y\nend\n";
        let staging = build(src);
        find_node(&staging, "Foo::y", Some(NodeKind::Constant))
            .expect("Foo#y Constant must be emitted without YARD");
    }

    #[test]
    fn req_r0001_attr_writer_without_yard_emits_property_node() {
        let src = "class Foo\n  attr_writer :z\nend\n";
        let staging = build(src);
        find_node(&staging, "Foo::z", Some(NodeKind::Property))
            .expect("Foo#z Property must be emitted without YARD");
    }

    #[test]
    fn req_r0001_attr_with_yard_still_emits() {
        let src = "class Foo\n  # @return [String]\n  attr_reader :y\nend\n";
        let staging = build(src);
        find_node(&staging, "Foo::y", Some(NodeKind::Constant))
            .expect("Foo#y Constant must be emitted when YARD is present too");
    }

    // -- AC-2: kind branching by attr_* method -----------------------------

    #[test]
    fn req_r0023_attr_reader_branches_to_constant() {
        let src = "class Bar\n  attr_reader :name\nend\n";
        let staging = build(src);
        let entry = find_node(&staging, "Bar::name", Some(NodeKind::Constant))
            .expect("attr_reader must produce Constant");
        assert_eq!(entry.kind, NodeKind::Constant);
        assert!(
            find_node(&staging, "Bar::name", Some(NodeKind::Property)).is_none(),
            "attr_reader must NOT also produce a Property"
        );
    }

    #[test]
    fn req_r0023_attr_writer_branches_to_property() {
        let src = "class Bar\n  attr_writer :name\nend\n";
        let staging = build(src);
        let entry = find_node(&staging, "Bar::name", Some(NodeKind::Property))
            .expect("attr_writer must produce Property");
        assert_eq!(entry.kind, NodeKind::Property);
        assert!(
            find_node(&staging, "Bar::name", Some(NodeKind::Constant)).is_none(),
            "attr_writer must NOT also produce a Constant"
        );
    }

    #[test]
    fn req_r0023_attr_accessor_branches_to_property() {
        let src = "class Bar\n  attr_accessor :name\nend\n";
        let staging = build(src);
        let entry = find_node(&staging, "Bar::name", Some(NodeKind::Property))
            .expect("attr_accessor must produce Property");
        assert_eq!(entry.kind, NodeKind::Property);
        assert!(
            find_node(&staging, "Bar::name", Some(NodeKind::Constant)).is_none(),
            "attr_accessor must NOT also produce a Constant"
        );
    }

    #[test]
    fn req_r0023_attr_accessor_emits_one_per_argument() {
        let src = "class Multi\n  attr_accessor :a, :b, :c\nend\n";
        let staging = build(src);
        find_node(&staging, "Multi::a", Some(NodeKind::Property))
            .expect("Multi#a Property must exist");
        find_node(&staging, "Multi::b", Some(NodeKind::Property))
            .expect("Multi#b Property must exist");
        find_node(&staging, "Multi::c", Some(NodeKind::Property))
            .expect("Multi#c Property must exist");
        // Each name appears exactly once (no Variable shadow from old path).
        assert_eq!(count_nodes_named(&staging, "Multi::a"), 1);
        assert_eq!(count_nodes_named(&staging, "Multi::b"), 1);
        assert_eq!(count_nodes_named(&staging, "Multi::c"), 1);
    }

    // -- AC-3: qualified name retains Ruby `#` idiom -----------------------

    #[test]
    fn req_r0017_qualified_name_uses_ruby_hash_idiom() {
        // Builder passes "Foo#x"; canonicalizer rewrites `#` -> `::` for graph
        // identity (verified by canonical lookup below). The `#` form is the
        // user-facing source-level idiom that the planner U15 lock-in test
        // accepts as a literal name pattern. Any escape from this contract
        // (e.g. switching to `.` or `::` at the builder seam) breaks the §3.1.3
        // deviation.
        let src = "class Foo\n  attr_accessor :x\nend\n";
        let staging = build(src);
        // Canonical form (post-canonicalize) is `Foo::x`.
        find_node(&staging, "Foo::x", Some(NodeKind::Property))
            .expect("canonical Foo::x must exist");
        // The bare attr name must NOT leak as the canonical identity.
        assert!(
            find_node(&staging, "x", Some(NodeKind::Property)).is_none(),
            "bare 'x' must not be the qualified name (would collide across classes)"
        );
    }

    // -- AC-4: YARD type-tag preserved as TypeOf{Field} enrichment ---------

    #[test]
    fn req_r0006_yard_type_tag_drives_typeof_field_edge() {
        let src = "class User\n  # @return [String]\n  attr_reader :name\nend\n";
        let staging = build(src);
        let edges = typeof_edges_for_node(&staging, "User::name");
        assert!(
            !edges.is_empty(),
            "User#name should have a TypeOf edge from YARD @return"
        );
        let has_string = edges.iter().any(|(_, _, t)| t == "String");
        assert!(
            has_string,
            "YARD @return [String] should produce a TypeOf target 'String', got {edges:?}"
        );
    }

    #[test]
    fn req_r0006_typeof_uses_field_context_and_bare_name() {
        let src = "class C\n  # @return [String]\n  attr_accessor :title\nend\n";
        let staging = build(src);
        let edges = typeof_edges_for_node(&staging, "C::title");
        assert!(!edges.is_empty(), "C#title should have a TypeOf edge");
        for (ctx, name, _) in &edges {
            assert_eq!(*ctx, Some(TypeOfContext::Field), "context must be Field");
            assert_eq!(
                name.as_deref(),
                Some("title"),
                "edge name must be the bare attr name"
            );
        }
    }

    #[test]
    fn req_r0006_no_yard_means_no_typeof_edge_but_node_emitted() {
        // Node emission is unconditional; type enrichment is opportunistic.
        let src = "class C\n  attr_accessor :untyped\nend\n";
        let staging = build(src);
        find_node(&staging, "C::untyped", Some(NodeKind::Property))
            .expect("Property must emit even without YARD type tag");
        let edges = typeof_edges_for_node(&staging, "C::untyped");
        assert!(
            edges.is_empty(),
            "no YARD => no TypeOf{{Field}} enrichment edge, got {edges:?}"
        );
    }

    // -- AC-5 + design §4.5: visibility scope propagates, is_static is false --

    #[test]
    fn req_r0023_attr_node_visibility_defaults_to_public() {
        let src = "class V\n  attr_accessor :x\nend\n";
        let staging = build(src);
        let entry =
            find_node(&staging, "V::x", Some(NodeKind::Property)).expect("V#x Property must exist");
        assert_eq!(
            visibility(&staging, entry).as_deref(),
            Some("public"),
            "Ruby attr_* nodes default to public visibility"
        );
    }

    #[test]
    fn req_r0023_attr_node_visibility_tracks_private_and_protected_scope() {
        let src = "class V\n  private\n  attr_accessor :hidden\n  protected\n  attr_reader :guarded\nend\n";
        let staging = build(src);
        let hidden = find_node(&staging, "V::hidden", Some(NodeKind::Property))
            .expect("V#hidden Property must exist");
        let guarded = find_node(&staging, "V::guarded", Some(NodeKind::Constant))
            .expect("V#guarded Constant must exist");
        assert_eq!(
            visibility(&staging, hidden).as_deref(),
            Some("private"),
            "Ruby attr_* nodes must inherit private visibility scope"
        );
        assert_eq!(
            visibility(&staging, guarded).as_deref(),
            Some("protected"),
            "Ruby attr_reader nodes must inherit protected visibility scope"
        );
    }

    #[test]
    fn req_r0023_attr_node_is_not_static() {
        let src = "class S\n  attr_reader :y\nend\n";
        let staging = build(src);
        let entry =
            find_node(&staging, "S::y", Some(NodeKind::Constant)).expect("S#y Constant must exist");
        assert!(
            !entry.is_static,
            "attr_* nodes must have is_static=false (always instance per design §4.5)"
        );
    }

    // -- design §3.1.3 + cross-class collision guard -----------------------

    #[test]
    fn req_r0017_same_attr_name_across_classes_distinct_nodes() {
        let src = "class A\n  attr_accessor :x\nend\nclass B\n  attr_accessor :x\nend\n";
        let staging = build(src);
        find_node(&staging, "A::x", Some(NodeKind::Property)).expect("A#x Property must exist");
        find_node(&staging, "B::x", Some(NodeKind::Property)).expect("B#x Property must exist");
        assert!(
            find_node(&staging, "x", Some(NodeKind::Property)).is_none(),
            "bare 'x' must not exist; qualified names disambiguate cross-class"
        );
    }

    // -- nested namespace -------------------------------------------------

    #[test]
    fn req_r0017_nested_module_class_qualifies_attr() {
        let src = "module M\n  class Inner\n    attr_accessor :n\n  end\nend\n";
        let staging = build(src);
        find_node(&staging, "M::Inner::n", Some(NodeKind::Property))
            .expect("M::Inner#n Property must exist with full namespace");
    }

    // -- string-form arg + command_call form (regression) -----------------

    #[test]
    fn req_r0001_attr_reader_string_argument_emits_constant() {
        let src = "class User\n  attr_reader \"username\"\nend\n";
        let staging = build(src);
        find_node(&staging, "User::username", Some(NodeKind::Constant))
            .expect("attr_reader with string arg must emit Constant");
    }

    #[test]
    fn req_r0001_attr_accessor_command_call_form_emits_property() {
        let src = "class Service\n  self.attr_accessor :logger\nend\n";
        let staging = build(src);
        find_node(&staging, "Service::logger", Some(NodeKind::Property))
            .expect("self.attr_accessor command_call must emit Property");
    }
}
