use std::{collections::HashMap, path::Path, sync::OnceLock};

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::GraphBuildHelper;
use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeId as UnifiedNodeId;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

use super::local_scopes;

const DEFAULT_SCOPE_DEPTH: usize = 4;
const STD_C_MODULES: &[&str] = &[
    "_ctypes",
    "_socket",
    "_ssl",
    "_hashlib",
    "_json",
    "_pickle",
    "_struct",
    "_sqlite3",
    "_decimal",
    "_lzma",
    "_bz2",
    "_zlib",
    "_elementtree",
    "_csv",
    "_datetime",
    "_heapq",
    "_bisect",
    "_random",
    "_collections",
    "_functools",
    "_itertools",
    "_operator",
    "_io",
    "_thread",
    "_multiprocessing",
    "_posixsubprocess",
    "_asyncio",
    "array",
    "math",
    "cmath",
];
const THIRD_PARTY_C_PACKAGES: &[&str] = &[
    "numpy",
    "pandas",
    "scipy",
    "sklearn",
    "cv2",
    "PIL",
    "torch",
    "tensorflow",
    "lxml",
    "psycopg2",
    "MySQLdb",
    "sqlite3",
    "cryptography",
    "bcrypt",
    "regex",
    "ujson",
    "orjson",
    "msgpack",
    "greenlet",
    "gevent",
    "uvloop",
];

/// Graph builder for Python files using unified `CodeGraph` architecture.
#[derive(Debug, Clone, Copy)]
pub struct PythonGraphBuilder {
    max_scope_depth: usize,
}

impl Default for PythonGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl PythonGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for PythonGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Python);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Check if __all__ is defined in the module
        let has_all = has_all_assignment(tree.root_node(), content);

        // Build local variable scope tree
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

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

        // Walk tree to find functions, classes, methods, calls, and imports
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            has_all,
            &mut guard,
            &mut scope_tree,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Python
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(python_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Python: the SPEC anchor for the
/// identifier-blind body-shape descriptor.
///
/// Holds a precomputed `kind_id -> CfBucket` table so the hot shape walk does a
/// single array index per node instead of a grammar string lookup. The table is
/// built once from the tree-sitter-python grammar and shared process-wide via
/// [`python_shape_mapping`]. Everything except this mapping is the one shared
/// `compute_shape_descriptor` routine in sqry-core.
pub struct PythonShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl PythonShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-python grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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
                *slot = cf_bucket_for_python_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for PythonShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        if let Some(params) = fn_node.child_by_field_name("parameters") {
            // Python keyword-only parameters follow a bare `*` or a `*args`
            // splat. Track whether we have crossed that boundary so positional
            // and keyword-only arities are counted into the right slot.
            let mut keyword_only = false;
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                match child.kind() {
                    // `*args`: variadic AND the start of the keyword-only region.
                    "list_splat_pattern" => {
                        shape.has_varargs = true;
                        keyword_only = true;
                    }
                    // `**kwargs`.
                    "dictionary_splat_pattern" => shape.has_kwargs = true,
                    // A plain positional / keyword parameter (`x`).
                    "identifier" | "typed_parameter" => {
                        bump_arity(&mut shape, keyword_only);
                    }
                    // A parameter carrying a default value (`x=1`, `x: int = 1`).
                    "default_parameter" | "typed_default_parameter" => {
                        shape.has_defaults = true;
                        bump_arity(&mut shape, keyword_only);
                    }
                    _ => {}
                }
            }
        }
        shape.has_return_annotation = fn_node.child_by_field_name("return_type").is_some();
        shape
    }
}

/// Count one parameter into the positional or keyword-only arity slot.
fn bump_arity(shape: &mut SignatureShape, keyword_only: bool) {
    if keyword_only {
        shape.arity_keyword_only = shape.arity_keyword_only.saturating_add(1);
    } else {
        shape.arity_positional = shape.arity_positional.saturating_add(1);
    }
}

/// Map one tree-sitter-python grammar node-kind name to its canonical
/// control-flow bucket. Additive-only: the bucket set is frozen (see
/// [`CfBucket`]), so new Python kinds extend the match, never reorder the buckets.
fn cf_bucket_for_python_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "elif_clause" | "conditional_expression" => CfBucket::Branch,
        "for_statement" | "while_statement" => CfBucket::Loop,
        "match_statement" | "case_clause" => CfBucket::Match,
        "try_statement" => CfBucket::Try,
        "except_clause" | "except_group_clause" => CfBucket::Catch,
        "raise_statement" => CfBucket::Throw,
        // `with`/`async with` is Python's resource-acquisition construct.
        "with_statement" => CfBucket::Resource,
        "return_statement" => CfBucket::Return,
        "yield" => CfBucket::Yield,
        "await" => CfBucket::Await,
        "break_statement" | "continue_statement" => CfBucket::BreakContinue,
        "call" => CfBucket::Call,
        "assignment" | "augmented_assignment" | "named_expression" => CfBucket::Assign,
        "lambda" => CfBucket::Closure,
        "list_comprehension"
        | "dictionary_comprehension"
        | "set_comprehension"
        | "generator_expression" => CfBucket::Comprehension,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Python shape mapping, built once on first use.
#[must_use]
pub fn python_shape_mapping() -> &'static PythonShapeMapping {
    static MAPPING: OnceLock<PythonShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(PythonShapeMapping::build)
}

/// Check if the module defines `__all__`.
fn has_all_assignment(node: Node, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            // Check for __all__ assignment
            let assignment = child
                .children(&mut child.walk())
                .find(|c| c.kind() == "assignment" || c.kind() == "augmented_assignment");

            if let Some(assignment) = assignment
                && let Some(left) = assignment.child_by_field_name("left")
                && let Ok(left_text) = left.utf8_text(content)
                && left_text.trim() == "__all__"
            {
                return true;
            }
        }
    }
    false
}

/// Walk the tree and populate the staging graph.
/// # Errors
///
/// Returns [`GraphBuilderError`] if graph operations fail or recursion depth exceeds the guard's limit.
#[allow(clippy::too_many_lines)]
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    has_all: bool,
    guard: &mut sqry_core::query::security::RecursionGuard,
    scope_tree: &mut local_scopes::PythonScopeTree,
) -> GraphResult<()> {
    guard.enter().map_err(|e| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!("Recursion limit exceeded: {e}"),
    })?;

    match node.kind() {
        "class_definition" => {
            // Extract class name
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let span = span_from_node(node);

                // Build qualified class name from scope
                let qualified_name = class_name.to_string();

                // Add class node. Real class declaration (issue #394): opt the
                // dual-use add_class bare helper into is_definition = true.
                let class_id = helper.add_class(&qualified_name, Some(span));
                helper.mark_definition(class_id);

                // Process inheritance (base classes)
                process_class_inheritance(node, content, class_id, helper);

                // Note: Class body annotations are processed via normal recursion in walk_tree_for_graph

                // Export public classes at module level (only if __all__ is not defined)
                if !has_all && is_module_level(node) && is_public_name(class_name) {
                    export_from_file_module(helper, class_id);
                }
            }
        }
        "expression_statement" => {
            // Check for __all__ assignment (exports)
            process_all_assignment(node, content, helper);

            // Check for annotated assignments (type hints on variables)
            process_annotated_assignment(node, content, ast_graph, helper);
        }
        "function_definition" => {
            // Extract function context from AST graph
            if let Some(call_context) = ast_graph.get_callable_context(node.id()) {
                let span = span_from_node(node);

                // Extract visibility from function name
                let func_name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(content).ok())
                    .unwrap_or("");
                let visibility = extract_visibility_from_name(func_name);

                // Check if this is a property (has @property decorator)
                let is_property = has_property_decorator(node, content);

                // Extract return type annotation for signature (normalized — strips
                // generics/unions/quotes for human-readable display).
                let return_type = extract_return_type_annotation(node, content);

                // Extract byte-exact source text of the return-type annotation for
                // the `TypeOf { context: Return }` edge consumed by `returns:<Type>`
                // queries. This text is intentionally NOT normalized — `Optional[int]`,
                // `List[Dict[str, int]]`, `pd.DataFrame`, `"User"` are all preserved
                // verbatim so byte-exact predicates work as documented.
                let return_type_source = extract_return_type_source_text(node, content);

                // Add function/method/property node
                let function_id = if is_property && call_context.is_method {
                    // Property node
                    helper.add_node_with_visibility(
                        &call_context.qualified_name,
                        Some(span),
                        sqry_core::graph::unified::node::NodeKind::Property,
                        Some(visibility),
                    )
                } else if call_context.is_method {
                    // Regular method with signature
                    if return_type.is_some() {
                        helper.add_method_with_signature(
                            &call_context.qualified_name,
                            Some(span),
                            call_context.is_async,
                            false, // Python doesn't have static methods in the same way
                            Some(visibility),
                            return_type.as_deref(),
                        )
                    } else {
                        helper.add_method_with_visibility(
                            &call_context.qualified_name,
                            Some(span),
                            call_context.is_async,
                            false,
                            Some(visibility),
                        )
                    }
                } else {
                    // Regular function with signature
                    if return_type.is_some() {
                        helper.add_function_with_signature(
                            &call_context.qualified_name,
                            Some(span),
                            call_context.is_async,
                            false, // Python doesn't have unsafe
                            Some(visibility),
                            return_type.as_deref(),
                        )
                    } else {
                        helper.add_function_with_visibility(
                            &call_context.qualified_name,
                            Some(span),
                            call_context.is_async,
                            false,
                            Some(visibility),
                        )
                    }
                };

                // Emit `TypeOf { context: Return }` edge for the return type
                // annotation when present. Property nodes (Python `@property`) and
                // un-annotated functions get no edge — `extract_return_type_source_text`
                // returns `None` for `def foo():` (no `-> Type`).
                //
                // The type-text is byte-exact source from the annotation node so
                // `returns:Optional[int]`, `returns:pd.DataFrame`, etc. work as
                // documented. A paired Reference edge is also emitted to keep
                // typeof/reference-edge invariants in sync with C# / Go / Kotlin /
                // TypeScript plugins.
                //
                // The synthesized Type node is anchored at the return-type
                // annotation's span (mirroring the Rust precedent in
                // `sqry-lang-rust/src/relations/graph_builder.rs`) so downstream
                // consumers (LSP `textDocument/documentSymbol`, MCP
                // `get_document_symbols`) report a concrete source location
                // rather than line 0.
                if !(is_property && call_context.is_method)
                    && let Some(annotation_text) = return_type_source.as_deref()
                    && let Some(return_type_node) = node.child_by_field_name("return_type")
                {
                    let type_span = span_from_node(return_type_node);
                    let type_id = helper.add_type(annotation_text, Some(type_span));
                    helper.add_typeof_edge_with_context(
                        function_id,
                        type_id,
                        Some(TypeOfContext::Return),
                        Some(0),
                        Some(call_context.qualified_name.as_str()),
                    );
                    helper.add_reference_edge(function_id, type_id);
                }

                // Check for HTTP route decorators (Flask/FastAPI)
                if let Some((http_method, route_path)) = extract_route_decorator_info(node, content)
                {
                    let endpoint_name = format!("route::{http_method}::{route_path}");
                    let endpoint_id = helper.add_endpoint(&endpoint_name, Some(span));
                    helper.add_contains_edge(endpoint_id, function_id);
                }

                // Process parameters to create TypeOf and Reference edges for type hints
                process_function_parameters(node, content, ast_graph, helper);

                // Export public functions at module level (not methods, only if __all__ is not defined)
                if !has_all
                    && !call_context.is_method
                    && is_module_level(node)
                    && let Some(name_node) = node.child_by_field_name("name")
                    && let Ok(func_name) = name_node.utf8_text(content)
                    && is_public_name(func_name)
                {
                    export_from_file_module(helper, function_id);
                }
            }
        }
        "call" => {
            // Check for FFI patterns first (ctypes, cffi)
            let is_ffi = build_ffi_call_edge(ast_graph, node, content, helper)?;
            if !is_ffi {
                // Not an FFI call - build regular call edge
                if let Ok(Some((caller_qname, callee_qname, argument_count, is_awaited))) =
                    build_call_for_staging(ast_graph, node, content)
                {
                    // Ensure both nodes exist
                    let call_context = ast_graph.get_callable_context(node.id());
                    let _is_async = call_context.is_some_and(|c| c.is_async);

                    let call_span = span_from_node(node);
                    let source_id =
                        helper.ensure_callee(&caller_qname, call_span, CalleeKindHint::Function);
                    let target_id =
                        helper.ensure_callee(&callee_qname, call_span, CalleeKindHint::Function);

                    // Add call edge
                    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
                    helper.add_call_edge_full_with_span(
                        source_id,
                        target_id,
                        argument_count,
                        is_awaited,
                        vec![call_span],
                    );
                }
            }
        }
        "import_statement" | "import_from_statement" => {
            // Build import edge
            if let Ok(Some((from_qname, to_qname))) =
                build_import_for_staging(node, content, helper)
            {
                // Ensure both module nodes exist
                let from_id = helper.add_import(&from_qname, None);
                let to_id = helper.add_import(&to_qname, Some(span_from_node(node)));

                // Add import edge
                helper.add_import_edge(from_id, to_id);

                // Check if this imports a known native C extension module
                if is_native_extension_import(&to_qname) {
                    build_native_import_ffi_edge(&to_qname, node, helper);
                }
            }
        }
        "identifier" => {
            // Local variable reference tracking
            local_scopes::handle_identifier_for_reference(node, content, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(
            child, content, ast_graph, helper, has_all, guard, scope_tree,
        )?;
    }

    guard.exit();
    Ok(())
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<(String, String, usize, bool)>> {
    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(call_node.id()) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: (0, content.len()),
            is_async: false,
            is_method: false,
            class_name: None,
        };
        &module_context
    };

    let Some(callee_expr) = call_node.child_by_field_name("function") else {
        return Ok(None);
    };

    let callee_text = callee_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim()
        .to_string();

    if callee_text.is_empty() {
        return Ok(None);
    }

    let callee_simple = simple_name(&callee_text);
    if callee_simple.is_empty() {
        return Ok(None);
    }

    // Derive qualified callee name with proper self resolution
    let caller_qname = call_context.qualified_name();
    let target_qname = if let Some(method_name) = callee_text.strip_prefix("self.") {
        // Resolve self.method() to ClassName.method()
        if let Some(class_name) = &call_context.class_name {
            format!("{}.{}", class_name, simple_name(method_name))
        } else {
            callee_simple.to_string()
        }
    } else {
        callee_simple.to_string()
    };

    let argument_count = count_arguments(call_node);
    let is_awaited = is_awaited_call(call_node);
    Ok(Some((
        caller_qname,
        target_qname,
        argument_count,
        is_awaited,
    )))
}

/// Build import edge information for the staging graph.
fn build_import_for_staging(
    import_node: Node<'_>,
    content: &[u8],
    helper: &GraphBuildHelper,
) -> GraphResult<Option<(String, String)>> {
    // Extract the raw module name from the AST
    let raw_module_name = if import_node.kind() == "import_statement" {
        import_node
            .child_by_field_name("name")
            .and_then(|n| extract_module_name(n, content))
    } else if import_node.kind() == "import_from_statement" {
        import_node
            .child_by_field_name("module_name")
            .and_then(|n| extract_module_name(n, content))
    } else {
        None
    };

    // Handle relative imports with no module name
    let module_name = if raw_module_name.is_none() && import_node.kind() == "import_from_statement"
    {
        if let Ok(import_text) = import_node.utf8_text(content) {
            if let Some(from_idx) = import_text.find("from") {
                if let Some(import_idx) = import_text.find("import") {
                    let between = import_text[from_idx + 4..import_idx].trim();
                    if between.starts_with('.') {
                        Some(between.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        raw_module_name
    };

    let Some(module_name) = module_name else {
        return Ok(None);
    };

    if module_name.is_empty() {
        return Ok(None);
    }

    // Resolve the import path to a canonical module identifier
    let resolved_path = sqry_core::graph::resolve_python_import(
        std::path::Path::new(helper.file_path()),
        &module_name,
        import_node.kind() == "import_from_statement",
    )?;

    // Return from/to qualified names
    Ok(Some((helper.file_path().to_string(), resolved_path)))
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

fn count_arguments(call_node: Node<'_>) -> usize {
    call_node
        .child_by_field_name("arguments")
        .map_or(0, |args| {
            args.named_children(&mut args.walk())
                .filter(|child| {
                    // Count actual arguments, not commas or parentheses
                    !matches!(child.kind(), "," | "(" | ")")
                })
                .count()
        })
}

fn is_awaited_call(call_node: Node<'_>) -> bool {
    let mut current = call_node.parent();
    while let Some(node) = current {
        let kind = node.kind();
        if kind == "await" || kind == "await_expression" {
            return true;
        }
        current = node.parent();
    }
    false
}

/// Extract the simple name from a dotted identifier (for general call targets).
///
/// Takes the last component after splitting by dots.
/// Used for qualified names like "module.func" → "func" or "obj.method" → "method".
fn simple_name(qualified: &str) -> &str {
    qualified.split('.').next_back().unwrap_or(qualified)
}

/// Extract a simple library name from an FFI library path.
///
/// For library paths with file extensions, extracts the base name before the extension.
/// This prevents different libraries with the same extension (lib1.so, lib2.so) from
/// colliding as duplicate "so" targets.
///
/// Handles:
/// - Full paths: "/opt/v1.2/libfoo.so" → "libfoo"
/// - Relative paths: "libs/lib1.so" → "lib1"
/// - Versioned libs: "libc.so.6" → "libc"
/// - Simple names: "kernel32" → "kernel32"
/// - Variable refs: "$libname" → "$libname"
fn ffi_library_simple_name(library_path: &str) -> String {
    use std::path::Path;

    // Strip directory components first (handles /opt/v1.2/libfoo.so)
    let filename = Path::new(library_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(library_path);

    // Handle versioned .so files first (libc.so.6 → libc)
    if let Some(so_pos) = filename.find(".so.") {
        return filename[..so_pos].to_string();
    }

    // Handle standard library extensions
    if let Some(dot_pos) = filename.find('.') {
        let extension = &filename[dot_pos + 1..];

        // Check for known library extensions
        if extension == "so" || extension == "dll" || extension == "dylib" {
            // Extract base name before extension
            return filename[..dot_pos].to_string();
        }
    }

    // No library extension found - return filename as-is
    filename.to_string()
}

/// Check if a name is public (does not start with underscore).
///
/// In Python, names starting with a single underscore are considered private by convention.
/// Names starting with double underscores trigger name mangling in classes.
/// Public names do not start with an underscore.
fn is_public_name(name: &str) -> bool {
    !name.starts_with('_')
}

/// Check if a node is at module level (direct child of the module body).
///
/// In tree-sitter Python AST, module-level items are direct children of the root "module" node.
/// We check if the parent is "module" to determine module-level scope.
fn is_module_level(node: Node<'_>) -> bool {
    // Walk up the tree to find the immediate container
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "module" => return true,
            "function_definition" | "class_definition" => return false,
            _ => current = parent.parent(),
        }
    }
    false
}

/// Export a symbol from the file module.
///
/// File-level module name for exports/imports.
/// Distinct from `<module>` to avoid conflicts with top-level call context.
const FILE_MODULE_NAME: &str = "<file_module>";

fn export_from_file_module(
    helper: &mut GraphBuildHelper,
    exported: sqry_core::graph::unified::node::NodeId,
) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

/// Extract module name from a `dotted_name`, `aliased_import`, or `relative_import` node
///
/// For `import numpy as np`, the "name" field is an `aliased_import` node with structure:
/// `aliased_import { name: dotted_name("numpy"), alias: identifier("np") }`
/// We need to extract just "numpy", not "numpy as np".
fn extract_module_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    // Handle aliased imports: `import numpy as np` -> extract "numpy"
    if node.kind() == "aliased_import" {
        // The "name" field of aliased_import contains the actual module name
        return node
            .child_by_field_name("name")
            .and_then(|name_node| name_node.utf8_text(content).ok())
            .map(std::string::ToString::to_string);
    }

    // Regular dotted_name or identifier
    node.utf8_text(content)
        .ok()
        .map(std::string::ToString::to_string)
}

// ============================================================================
// Exports - __all__ assignment handling
// ============================================================================

/// Process `__all__ = ['name1', 'name2']` assignments to create export edges.
///
/// Python's `__all__` list explicitly defines the public API of a module.
/// Each name in the list gets an Export edge from the module to the exported symbol.
fn process_all_assignment(node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    // expression_statement contains an assignment child
    let assignment = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "assignment" || child.kind() == "augmented_assignment");

    let Some(assignment) = assignment else {
        return;
    };

    // Check if left side is __all__
    let left = assignment.child_by_field_name("left");
    let Some(left) = left else {
        return;
    };

    let Ok(left_text) = left.utf8_text(content) else {
        return;
    };

    if left_text.trim() != "__all__" {
        return;
    }

    // Get the right side (should be a list)
    let right = assignment.child_by_field_name("right");
    let Some(right) = right else {
        return;
    };

    // Handle list or tuple literal (both valid for __all__)
    if right.kind() == "list" || right.kind() == "tuple" {
        process_all_list(right, content, helper);
    }
}

/// Process a list/tuple of exported names from __all__.
fn process_all_list(list_node: Node<'_>, content: &[u8], helper: &mut GraphBuildHelper) {
    for child in list_node.children(&mut list_node.walk()) {
        // Look for string literals
        if child.kind() == "string"
            && let Some(export_name) = extract_string_content(child, content)
            && !export_name.is_empty()
        {
            // Create a node for the exported symbol
            // We use add_function here as a generic symbol; the actual type
            // will be resolved later by cross-file analysis
            let span = span_from_node(child);
            let export_id = helper.add_function(&export_name, Some(span), false, false);

            // Add export edge (Direct export, no alias for Python __all__)
            export_from_file_module(helper, export_id);
        }
    }
}

/// Extract the content of a string literal node (removing quotes).
fn extract_string_content(string_node: Node<'_>, content: &[u8]) -> Option<String> {
    // String nodes contain string_content or string_start/string_content/string_end
    // Try to get the full text and strip quotes
    let Ok(text) = string_node.utf8_text(content) else {
        return None;
    };

    let text = text.trim();

    // Handle various Python string formats: 'x', "x", '''x''', """x""", r'x', etc.
    let stripped = text
        .trim_start_matches(|c: char| {
            c == 'r'
                || c == 'b'
                || c == 'f'
                || c == 'u'
                || c == 'R'
                || c == 'B'
                || c == 'F'
                || c == 'U'
        })
        .trim_start_matches("'''")
        .trim_end_matches("'''")
        .trim_start_matches("\"\"\"")
        .trim_end_matches("\"\"\"")
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .trim_start_matches('"')
        .trim_end_matches('"');

    Some(stripped.to_string())
}

// ============================================================================
// OOP - Inheritance handling
// ============================================================================

/// Process class inheritance to create Inherits edges.
///
/// Python supports multiple inheritance: `class Child(Parent1, Parent2):`
/// Each base class gets an Inherits edge from the child class.
fn process_class_inheritance(
    class_node: Node<'_>,
    content: &[u8],
    class_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) {
    // In Python AST, base classes are in the superclasses field (argument_list)
    // class_definition has a "superclasses" field containing argument_list
    let superclasses = class_node.child_by_field_name("superclasses");

    let Some(superclasses) = superclasses else {
        return;
    };

    // argument_list contains the base classes
    for child in superclasses.children(&mut superclasses.walk()) {
        if child.kind() == "keyword_argument" {
            // Skip keyword arguments like metaclass=ABCMeta.
            continue;
        }

        match child.kind() {
            "identifier" => {
                // Simple base class: class Child(Parent):
                if let Ok(base_name) = child.utf8_text(content) {
                    let base_name = base_name.trim();
                    if !base_name.is_empty() {
                        let span = span_from_node(child);
                        let base_id = helper.add_class(base_name, Some(span));
                        helper.add_inherits_edge(class_id, base_id);
                    }
                }
            }
            "attribute" => {
                // Qualified base class: class Child(module.Parent):
                if let Ok(base_name) = child.utf8_text(content) {
                    let base_name = base_name.trim();
                    if !base_name.is_empty() {
                        let span = span_from_node(child);
                        let base_id = helper.add_class(base_name, Some(span));
                        helper.add_inherits_edge(class_id, base_id);
                    }
                }
            }
            "call" => {
                // Parameterized base class with call syntax: class Child(SomeBase(arg)):
                // Extract the function being called
                if let Some(func) = child.child_by_field_name("function")
                    && let Ok(base_name) = func.utf8_text(content)
                {
                    let base_name = base_name.trim();
                    if !base_name.is_empty() {
                        let span = span_from_node(child);
                        let base_id = helper.add_class(base_name, Some(span));
                        helper.add_inherits_edge(class_id, base_id);
                    }
                }
            }
            "subscript" => {
                // Generic base class: class Child(Generic[T]): or class Child(List[int]):
                // Extract the base type from the subscript (value field)
                if let Some(value) = child.child_by_field_name("value")
                    && let Ok(base_name) = value.utf8_text(content)
                {
                    let base_name = base_name.trim();
                    if !base_name.is_empty() {
                        let span = span_from_node(child);
                        let base_id = helper.add_class(base_name, Some(span));
                        helper.add_inherits_edge(class_id, base_id);
                    }
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    #[allow(dead_code)] // Reserved for scope analysis
    span: (usize, usize),
    is_async: bool,
    is_method: bool,
    class_name: Option<String>,
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
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();
        let mut scope_stack: Vec<String> = Vec::new();
        let mut class_stack: Vec<String> = Vec::new();

        walk_ast(
            tree.root_node(),
            content,
            &mut contexts,
            &mut node_to_context,
            &mut scope_stack,
            &mut class_stack,
            max_depth,
        )?;

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    #[allow(dead_code)] // Reserved for future context queries
    fn contexts(&self) -> &[CallContext] {
        &self.contexts
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }
}

fn walk_ast(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallContext>,
    node_to_context: &mut HashMap<usize, usize>,
    scope_stack: &mut Vec<String>,
    class_stack: &mut Vec<String>,
    max_depth: usize,
) -> Result<(), String> {
    if scope_stack.len() > max_depth {
        return Ok(());
    }

    match node.kind() {
        "class_definition" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| "class_definition missing name".to_string())?;
            let class_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read class name".to_string())?;

            // Build qualified class name
            let qualified_class = if scope_stack.is_empty() {
                class_name.to_string()
            } else {
                format!("{}.{}", scope_stack.join("."), class_name)
            };

            class_stack.push(qualified_class.clone());
            scope_stack.push(class_name.to_string());

            // Recurse into class body
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        class_stack,
                        max_depth,
                    )?;
                }
            }

            class_stack.pop();
            scope_stack.pop();
        }
        "function_definition" => {
            let name_node = node
                .child_by_field_name("name")
                .ok_or_else(|| "function_definition missing name".to_string())?;
            let func_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read function name".to_string())?;

            // Check if async
            let is_async = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "async");

            // Build qualified function name
            let qualified_func = if scope_stack.is_empty() {
                func_name.to_string()
            } else {
                format!("{}.{}", scope_stack.join("."), func_name)
            };

            // Determine if this is a method (inside a class)
            let is_method = !class_stack.is_empty();
            let class_name = class_stack.last().cloned();

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: (node.start_byte(), node.end_byte()),
                is_async,
                is_method,
                class_name,
            });

            // Associate the function definition node itself with this context
            // This is required so walk_tree_for_graph can find the context
            node_to_context.insert(node.id(), context_idx);

            // Associate all descendants with this context
            if let Some(body) = node.child_by_field_name("body") {
                associate_descendants(body, context_idx, node_to_context);
            }

            scope_stack.push(func_name.to_string());

            // Recurse into function body to find nested functions
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_ast(
                        child,
                        content,
                        contexts,
                        node_to_context,
                        scope_stack,
                        class_stack,
                        max_depth,
                    )?;
                }
            }

            scope_stack.pop();
        }
        _ => {
            // Recurse into children for other node types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_ast(
                    child,
                    content,
                    contexts,
                    node_to_context,
                    scope_stack,
                    class_stack,
                    max_depth,
                )?;
            }
        }
    }

    Ok(())
}

fn associate_descendants(
    node: Node,
    context_idx: usize,
    node_to_context: &mut HashMap<usize, usize>,
) {
    node_to_context.insert(node.id(), context_idx);

    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        node_to_context.insert(current.id(), context_idx);

        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ============================================================================
// FFI Detection - ctypes, cffi, and C extensions
// ============================================================================

/// Build FFI edges for call expressions.
///
/// Detects Python FFI patterns:
/// - `ctypes.CDLL('libfoo.so')` / `ctypes.cdll.LoadLibrary('libfoo.so')`
/// - `ctypes.WinDLL('kernel32')` / `ctypes.windll.kernel32`
/// - `ctypes.PyDLL('libpython.so')`
/// - `cffi.FFI().dlopen('libfoo.so')`
/// - `ffi.dlopen('libfoo.so')`
///
/// Returns true if an FFI edge was created, false otherwise.
fn build_ffi_call_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<bool> {
    let Some(callee_expr) = call_node.child_by_field_name("function") else {
        return Ok(false);
    };

    let callee_text = callee_expr
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim();

    // Check for ctypes library loading patterns
    if is_ctypes_load_call(callee_text) {
        return Ok(build_ctypes_ffi_edge(
            ast_graph,
            call_node,
            content,
            callee_text,
            helper,
        ));
    }

    // Check for cffi dlopen patterns
    if is_cffi_dlopen_call(callee_text) {
        return Ok(build_cffi_ffi_edge(ast_graph, call_node, content, helper));
    }

    Ok(false)
}

/// Check if the callee is a ctypes library loading function.
///
/// Narrowed patterns to reduce false positives - only match explicit ctypes paths.
/// Previous: `callee_text.ends_with(".LoadLibrary")` matched too broadly.
///
/// Note: `ctypes.cdll.kernel32` style attribute access patterns are not detected
/// because they're attribute access (not function calls). We only detect explicit
/// library loading function calls like CDLL('lib.so').
fn is_ctypes_load_call(callee_text: &str) -> bool {
    // Direct ctypes constructors (fully qualified)
    callee_text == "ctypes.CDLL"
        || callee_text == "ctypes.WinDLL"
        || callee_text == "ctypes.OleDLL"
        || callee_text == "ctypes.PyDLL"
        // ctypes.cdll/windll LoadLibrary (fully qualified)
        || callee_text == "ctypes.cdll.LoadLibrary"
        || callee_text == "ctypes.windll.LoadLibrary"
        || callee_text == "ctypes.oledll.LoadLibrary"
        // After `from ctypes import *` or `from ctypes import CDLL, etc.`
        || callee_text == "CDLL"
        || callee_text == "WinDLL"
        || callee_text == "OleDLL"
        || callee_text == "PyDLL"
        // After `from ctypes import cdll` or similar
        || callee_text == "cdll.LoadLibrary"
        || callee_text == "windll.LoadLibrary"
        || callee_text == "oledll.LoadLibrary"
}

/// Check if the callee is a cffi dlopen function.
///
/// Narrowed patterns to reduce false positives - only match known cffi patterns.
/// Previous: `callee_text.ends_with(".dlopen")` matched too broadly.
fn is_cffi_dlopen_call(callee_text: &str) -> bool {
    // Common cffi FFI variable names followed by dlopen
    callee_text == "ffi.dlopen"
        || callee_text == "cffi.dlopen"
        || callee_text == "_ffi.dlopen"
        // FFI() constructor followed by dlopen (chained call)
        // This pattern typically appears as: FFI().dlopen('lib.so')
        // In tree-sitter, the callee text would be the method access part
        // After `from cffi import FFI`
        || callee_text == "FFI().dlopen"
}

/// Build FFI edge for ctypes library loading.
fn build_ctypes_ffi_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    callee_text: &str,
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get caller context
    let caller_id = get_ffi_caller_node_id(ast_graph, call_node, content, helper);

    // Determine FFI convention based on the ctypes type
    let convention = if callee_text.contains("WinDLL")
        || callee_text.contains("windll")
        || callee_text.contains("OleDLL")
    {
        FfiConvention::Stdcall
    } else {
        FfiConvention::C
    };

    // Try to extract library name from first argument
    let library_name = extract_ffi_library_name(call_node, content)
        .unwrap_or_else(|| "ctypes::unknown".to_string());

    let ffi_name = format!("native::{}", ffi_library_simple_name(&library_name));
    let ffi_node_id = helper.add_module(&ffi_name, Some(span_from_node(call_node)));

    // Add FFI edge
    helper.add_ffi_edge(caller_id, ffi_node_id, convention);

    true
}

/// Build FFI edge for cffi dlopen.
fn build_cffi_ffi_edge(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> bool {
    // Get caller context
    let caller_id = get_ffi_caller_node_id(ast_graph, call_node, content, helper);

    // Try to extract library name from first argument
    let library_name =
        extract_ffi_library_name(call_node, content).unwrap_or_else(|| "cffi::unknown".to_string());

    let ffi_name = format!("native::{}", ffi_library_simple_name(&library_name));
    let ffi_node_id = helper.add_module(&ffi_name, Some(span_from_node(call_node)));

    // cffi uses C calling convention
    helper.add_ffi_edge(caller_id, ffi_node_id, FfiConvention::C);

    true
}

/// Get the caller node ID for FFI edges.
fn get_ffi_caller_node_id(
    ast_graph: &ASTGraph,
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> UnifiedNodeId {
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_callable_context(node.id()) {
        ctx
    } else {
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: (0, content.len()),
            is_async: false,
            is_method: false,
            class_name: None,
        };
        &module_context
    };

    let caller_span = Some(Span::from_bytes(call_context.span.0, call_context.span.1));
    helper.ensure_function(
        &call_context.qualified_name(),
        caller_span,
        call_context.is_async,
        false,
    )
}

/// Extract the library name from the first argument of a call.
fn extract_ffi_library_name(call_node: Node<'_>, content: &[u8]) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;

    let mut cursor = args.walk();
    let first_arg = args
        .children(&mut cursor)
        .find(|child| !matches!(child.kind(), "(" | ")" | ","))?;

    // Handle string literals
    if first_arg.kind() == "string" {
        return extract_string_content(first_arg, content);
    }

    // Handle identifiers (variable names) - we can't resolve them statically
    if first_arg.kind() == "identifier" {
        let text = first_arg.utf8_text(content).ok()?;
        return Some(format!("${}", text.trim())); // Mark as variable reference
    }

    None
}

/// Check if an import statement imports a known native extension module.
///
/// This detects patterns like:
/// - `import numpy` (known C extension)
/// - `from numpy import array` (known C extension)
/// - `import _sqlite3` (private C module)
fn is_native_extension_import(module_name: &str) -> bool {
    // Private C modules (underscore prefix)
    if module_name.starts_with('_') && !module_name.starts_with("__") {
        return true;
    }

    // Check against known modules
    let base_module = module_name.split('.').next().unwrap_or(module_name);

    STD_C_MODULES.contains(&base_module) || THIRD_PARTY_C_PACKAGES.contains(&base_module)
}

/// Build FFI edge for native extension import.
fn build_native_import_ffi_edge(
    module_name: &str,
    import_node: Node<'_>,
    helper: &mut GraphBuildHelper,
) {
    // Create module node for the importing file
    let file_path = helper.file_path().to_string();
    let importer_id = helper.add_module(&file_path, None);

    // Create node for the native module
    let ffi_name = format!("native::{}", simple_name(module_name));
    let ffi_node_id = helper.add_module(&ffi_name, Some(span_from_node(import_node)));

    // Add FFI edge (C convention for Python C extensions)
    helper.add_ffi_edge(importer_id, ffi_node_id, FfiConvention::C);
}

// ============================================================================
// HTTP Route Endpoint Detection - Flask/FastAPI decorators
// ============================================================================

/// HTTP methods recognized in route decorators.
const ROUTE_METHOD_NAMES: &[&str] = &["get", "post", "put", "delete", "patch"];

/// Receiver names recognized as route-capable objects.
///
/// `Flask` uses `app` or `blueprint`, `FastAPI` uses `app` or `router`.
const ROUTE_RECEIVER_NAMES: &[&str] = &["app", "router", "blueprint"];

/// Extract HTTP route information from Flask/FastAPI-style decorators on a function.
///
/// Checks whether the given `function_definition` node is wrapped in a `decorated_definition`
/// and whether any of its decorators match known route patterns:
///
/// - `@app.route('/path')` or `@app.route('/path', methods=['GET'])` -- GET by default
/// - `@app.get('/path')` / `@app.post('/path')` / `@app.put('/path')` / etc.
/// - `@router.get('/path')` (`FastAPI`)
/// - `@blueprint.route('/path')` (Flask blueprints)
///
/// Returns `Some((method, path))` where `method` is the uppercased HTTP method and
/// `path` is the route path string, or `None` if no route decorator is found.
fn extract_route_decorator_info(func_node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
    // The function_definition must be a child of decorated_definition
    let parent = func_node.parent()?;
    if parent.kind() != "decorated_definition" {
        return None;
    }

    // Iterate through decorator children of the decorated_definition
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() != "decorator" {
            continue;
        }

        let Ok(decorator_text) = child.utf8_text(content) else {
            continue;
        };
        let decorator_text = decorator_text.trim();

        // Strip the leading '@'
        let without_at = decorator_text.strip_prefix('@')?;

        // Try to parse as a route decorator
        if let Some(result) = parse_route_decorator_text(without_at) {
            return Some(result);
        }
    }

    None
}

/// Parse a single decorator text (without the leading `@`) to extract route information.
///
/// Recognized patterns:
/// - `app.route('/path')` or `app.route('/path', methods=['POST'])`
/// - `app.get('/path')` / `router.post('/path')` / `blueprint.delete('/path')`
///
/// Returns `Some((HTTP_METHOD, path))` or `None`.
fn parse_route_decorator_text(text: &str) -> Option<(String, String)> {
    // Split into receiver.method and argument portion
    // e.g. "app.route('/api/users')" -> ("app.route", "'/api/users')")
    let paren_pos = text.find('(')?;
    let accessor = &text[..paren_pos];
    let args_text = &text[paren_pos + 1..];

    // Split accessor into receiver and method_name
    let dot_pos = accessor.rfind('.')?;
    let receiver = &accessor[..dot_pos];
    let method_name = &accessor[dot_pos + 1..];

    // Check that the receiver is a known route-capable object.
    // Allow dotted receivers (e.g., "api.v1") as long as the final segment matches.
    let receiver_base = receiver.rsplit('.').next().unwrap_or(receiver);
    if !ROUTE_RECEIVER_NAMES.contains(&receiver_base) {
        return None;
    }

    // Extract the route path from the first argument (string literal)
    let path = extract_path_from_decorator_args(args_text)?;

    // Determine the HTTP method
    let method_lower = method_name.to_ascii_lowercase();
    if ROUTE_METHOD_NAMES.contains(&method_lower.as_str()) {
        // Direct method decorator: @app.get('/path') -> GET
        return Some((method_lower.to_ascii_uppercase(), path));
    }

    if method_lower == "route" {
        // Generic route decorator: @app.route('/path', methods=['POST'])
        let http_method = extract_method_from_route_args(args_text);
        return Some((http_method, path));
    }

    None
}

/// Extract the route path string from decorator arguments text.
///
/// The `args_text` parameter is everything after the opening parenthesis of the decorator call,
/// e.g. `'/api/users', methods=['GET'])` or `"/api/items")`.
///
/// Returns the path string with quotes stripped, or `None` if no path is found.
fn extract_path_from_decorator_args(args_text: &str) -> Option<String> {
    let trimmed = args_text.trim();

    // Find the first string literal (single or double quoted)
    let (quote_char, start_pos) = {
        let single_pos = trimmed.find('\'');
        let double_pos = trimmed.find('"');
        match (single_pos, double_pos) {
            (Some(s), Some(d)) => {
                if s < d {
                    ('\'', s)
                } else {
                    ('"', d)
                }
            }
            (Some(s), None) => ('\'', s),
            (None, Some(d)) => ('"', d),
            (None, None) => return None,
        }
    };

    // Find the closing quote
    let after_open = start_pos + 1;
    let close_pos = trimmed[after_open..].find(quote_char)?;
    let path = &trimmed[after_open..after_open + close_pos];

    if path.is_empty() {
        return None;
    }

    Some(path.to_string())
}

/// Extract the HTTP method from `@app.route('/path', methods=['POST'])` style arguments.
///
/// Looks for a `methods=` keyword argument containing a list of method strings.
/// If found, returns the first method in uppercase. Otherwise defaults to `"GET"`.
fn extract_method_from_route_args(args_text: &str) -> String {
    // Look for 'methods' keyword in the arguments
    let Some(methods_pos) = args_text.find("methods") else {
        return "GET".to_string();
    };

    // Find the opening bracket after 'methods='
    let after_methods = &args_text[methods_pos..];
    let Some(bracket_pos) = after_methods.find('[') else {
        return "GET".to_string();
    };

    let after_bracket = &after_methods[bracket_pos + 1..];

    // Find the first string literal inside the bracket
    let method_str = extract_first_string_literal(after_bracket);
    match method_str {
        Some(m) => m.to_ascii_uppercase(),
        None => "GET".to_string(),
    }
}

/// Extract the first single- or double-quoted string literal from the given text.
fn extract_first_string_literal(text: &str) -> Option<String> {
    let trimmed = text.trim();

    let (quote_char, start_pos) = {
        let single_pos = trimmed.find('\'');
        let double_pos = trimmed.find('"');
        match (single_pos, double_pos) {
            (Some(s), Some(d)) => {
                if s < d {
                    ('\'', s)
                } else {
                    ('"', d)
                }
            }
            (Some(s), None) => ('\'', s),
            (None, Some(d)) => ('"', d),
            (None, None) => return None,
        }
    };

    let after_open = start_pos + 1;
    let close_pos = trimmed[after_open..].find(quote_char)?;
    let literal = &trimmed[after_open..after_open + close_pos];

    if literal.is_empty() {
        return None;
    }

    Some(literal.to_string())
}

// ============================================================================
// Property Detection - @property decorator
// ============================================================================

/// Check if a function definition has a `@property` decorator.
///
/// Python AST structure for decorated functions:
/// ```python
/// @property
/// def name(self):
///     return self._name
/// ```
///
/// The tree-sitter AST wraps the `function_definition` in a `decorated_definition` node:
/// ```text
/// (block
///   (decorated_definition
///     decorator: (decorator "@property")
///     definition: (function_definition)))
/// ```
fn has_property_decorator(func_node: Node<'_>, content: &[u8]) -> bool {
    // The function_definition is a child of decorated_definition
    let Some(parent) = func_node.parent() else {
        return false;
    };

    // Check if parent is decorated_definition
    if parent.kind() != "decorated_definition" {
        return false;
    }

    // Look for @property decorator in the decorated_definition
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() == "decorator" {
            // Extract decorator text
            if let Ok(decorator_text) = child.utf8_text(content) {
                let decorator_text = decorator_text.trim();
                // Match @property or @property()
                if decorator_text == "@property"
                    || decorator_text.starts_with("@property(")
                    || decorator_text.starts_with("@property (")
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Extract visibility from Python identifier based on naming convention.
///
/// Python uses naming conventions for visibility:
/// - `__name` (dunder) -> private (name mangling)
/// - `_name` (single underscore) -> protected/internal
/// - `name` -> public
fn extract_visibility_from_name(name: &str) -> &'static str {
    if name.starts_with("__") && !name.ends_with("__") {
        "private"
    } else if name.starts_with('_') {
        "protected"
    } else {
        "public"
    }
}

// ============================================================================
// Type Hint Processing - TypeOf and Reference Edges
// ============================================================================

/// Find the containing scope (function/class) for a node to create scope-qualified names.
///
/// This walks up the AST to find the nearest enclosing function or class definition.
/// Returns:
/// - Empty string for module-level
/// - Class name for class-level (e.g., "`MyClass`")
/// - Function qualified name for function-level (e.g., "MyClass.method" or "process")
fn find_containing_scope(node: Node<'_>, content: &[u8], ast_graph: &ASTGraph) -> String {
    let mut current = node;
    let mut found_class_name: Option<String> = None;

    // Walk up the tree to find enclosing function or class
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "function_definition" => {
                // Found enclosing function - get its qualified name
                if let Some(ctx) = ast_graph.get_callable_context(parent.id()) {
                    return ctx.qualified_name.clone();
                }
            }
            "class_definition" => {
                // Remember the class name but continue walking up
                // to check if we're inside a function within this class
                if found_class_name.is_none() {
                    // Extract class name directly from node
                    if let Some(name_node) = parent.child_by_field_name("name")
                        && let Ok(class_name) = name_node.utf8_text(content)
                    {
                        found_class_name = Some(class_name.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    // If we found a class but no enclosing function, it's a class attribute
    found_class_name.unwrap_or_default()
}

/// Extract return type annotation from a function definition.
///
/// Python AST structure:
/// ```python
/// def foo() -> int:  # return_type field contains type annotation
/// ```
fn extract_return_type_annotation(func_node: Node<'_>, content: &[u8]) -> Option<String> {
    let return_type_node = func_node.child_by_field_name("return_type")?;
    extract_type_from_node(return_type_node, content)
}

/// Extract the byte-exact source text of a function's `-> Type` annotation.
///
/// Unlike [`extract_return_type_annotation`], this returns the raw annotation
/// text verbatim — no quote stripping, no union flattening, no generic-base
/// extraction. This is the form consumed by `returns:<TypeName>` predicates,
/// which match the byte-exact qualified name of the target Type node.
///
/// Returns `None` when the function has no `-> Type` annotation (e.g.
/// `def foo():`), in which case no Return edge is emitted.
///
/// Examples (input → returned text):
/// - `def foo() -> int:` → `Some("int")`
/// - `def foo() -> Optional[int]:` → `Some("Optional[int]")`
/// - `def foo() -> List[Dict[str, int]]:` → `Some("List[Dict[str, int]]")`
/// - `def foo() -> pd.DataFrame:` → `Some("pd.DataFrame")`
/// - `async def foo() -> AsyncIterator[int]:` → `Some("AsyncIterator[int]")`
/// - `def foo() -> "User":` → `Some("\"User\"")`
/// - `def foo():` → `None`
fn extract_return_type_source_text(func_node: Node<'_>, content: &[u8]) -> Option<String> {
    let return_type_node = func_node.child_by_field_name("return_type")?;
    let text = return_type_node.utf8_text(content).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Process function parameters to create `TypeOf` and Reference edges for type hints.
///
/// Handles:
/// - `def foo(x: int, y: str):` - typed parameters
/// - `def foo(self, x: int):` - skips self/cls
/// - `def foo(x: List[int]):` - extracts base type from generics
fn process_function_parameters(
    func_node: Node<'_>,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    let Some(params_node) = func_node.child_by_field_name("parameters") else {
        return;
    };

    // Get the qualified name of the containing function/method for scope qualification
    let scope_prefix = ast_graph
        .get_callable_context(func_node.id())
        .map_or("", |ctx| ctx.qualified_name.as_str());

    // Iterate through parameters in the parameter_list
    for param in params_node.children(&mut params_node.walk()) {
        // Python tree-sitter uses "typed_parameter" and "typed_default_parameter"
        // but we need to handle the actual structure
        match param.kind() {
            "typed_parameter" | "typed_default_parameter" => {
                process_typed_parameter(param, content, scope_prefix, helper);
            }
            // Untyped parameter - check if it has a type annotation in parent context
            // For now, skip (no type hint)
            // Default parameter without type - skip
            "identifier" | "default_parameter" => {}
            _ => {
                // Other parameter types - try to process if they have type annotations
                // This handles various parameter structures
                if param.child_by_field_name("type").is_some() {
                    process_typed_parameter(param, content, scope_prefix, helper);
                }
            }
        }
    }
}

/// Process a single typed parameter node.
///
/// Creates scope-qualified variable names to prevent cross-scope type contamination.
/// Format: `<scope_prefix>:<param_name>` (e.g., `MyClass.method:x` or `process:x`)
fn process_typed_parameter(
    param: Node<'_>,
    content: &[u8],
    scope_prefix: &str,
    helper: &mut GraphBuildHelper,
) {
    // Extract parameter name (could be in "name" field or as identifier child)
    let param_name = if let Some(name_node) = param.child_by_field_name("name") {
        name_node.utf8_text(content).ok()
    } else {
        // Fallback: look for identifier child
        param
            .children(&mut param.walk())
            .find(|c| c.kind() == "identifier")
            .and_then(|n| n.utf8_text(content).ok())
    };

    let Some(param_name) = param_name else {
        return;
    };

    // Skip self and cls (special method parameters)
    if param_name == "self" || param_name == "cls" {
        return;
    }

    // Extract type annotation
    let Some(type_node) = param.child_by_field_name("type") else {
        return;
    };

    let Some(type_name) = extract_type_from_node(type_node, content) else {
        return;
    };

    // Create scope-qualified parameter name to prevent cross-scope contamination
    // Format: <scope_prefix>:<param_name> (e.g., "MyClass.method:x" or "process:x")
    let qualified_param_name = if scope_prefix.is_empty() {
        // Top-level function parameter
        format!(":{param_name}")
    } else {
        format!("{scope_prefix}:{param_name}")
    };

    // Create parameter variable node with qualified name
    let param_id = helper.add_variable(&qualified_param_name, Some(span_from_node(param)));

    // Create type node
    let type_id = helper.add_type(&type_name, None);

    // Add TypeOf and Reference edges
    helper.add_typeof_edge(param_id, type_id);
    helper.add_reference_edge(param_id, type_id);
}

/// Process annotated assignments to create `TypeOf` and Reference edges.
///
/// Handles:
/// - `user: User = get_user()` - annotated assignment with value
/// - `count: int` - annotated assignment without value
/// - `items: List[str] = []` - generic types
fn process_annotated_assignment(
    expr_stmt_node: Node<'_>,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
) {
    // Get the containing scope for scope qualification
    // For assignments, we need to find the enclosing function/class
    let scope_prefix = find_containing_scope(expr_stmt_node, content, ast_graph);

    // Look for expression_statement containing an assignment
    for child in expr_stmt_node.children(&mut expr_stmt_node.walk()) {
        if child.kind() == "assignment" {
            process_typed_assignment(child, content, &scope_prefix, helper);
        }
    }
}

/// Process a typed assignment node (shared logic for variables and class attributes).
///
/// Creates scope-qualified variable names to prevent cross-scope type contamination.
fn process_typed_assignment(
    assignment_node: Node<'_>,
    content: &[u8],
    scope_prefix: &str,
    helper: &mut GraphBuildHelper,
) {
    // Check if this is a typed assignment by looking for type annotation
    // In Python, annotated assignments look like: name: type = value
    // The AST structure is: assignment { left: identifier, type: type, right: expression }

    let Some(left) = assignment_node.child_by_field_name("left") else {
        return;
    };

    let Some(type_node) = assignment_node.child_by_field_name("type") else {
        return;
    };

    // Extract variable name
    let Ok(var_name) = left.utf8_text(content) else {
        return;
    };

    // Extract type
    let Some(type_name) = extract_type_from_node(type_node, content) else {
        return;
    };

    // Create scope-qualified variable name to prevent cross-scope contamination
    // For class attributes (module-level or class-level), use simple name
    // For function-local variables, use qualified name
    let qualified_var_name = if scope_prefix.is_empty() {
        // Module-level variable
        var_name.to_string()
    } else if scope_prefix.contains('.') && !scope_prefix.contains(':') {
        // Class attribute (scope_prefix is class name without function)
        format!("{scope_prefix}.{var_name}")
    } else {
        // Function-local variable
        format!("{scope_prefix}:{var_name}")
    };

    // Create variable node with qualified name
    let var_id = helper.add_variable(&qualified_var_name, Some(span_from_node(assignment_node)));

    // Create type node
    let type_id = helper.add_type(&type_name, None);

    // Add TypeOf and Reference edges
    helper.add_typeof_edge(var_id, type_id);
    helper.add_reference_edge(var_id, type_id);
}

/// Extract type name from a type annotation node.
///
/// Handles:
/// - Simple types: `int`, `str`, `bool`
/// - Generic types: `List[int]` → extract base type `List`
/// - Optional types: `Optional[User]` → extract base type `Optional`
/// - Qualified types: `module.Type` → extract full qualified name
/// - Forward references: `"User"` → `User` (strips quotes)
/// - PEP 604 unions: `User | None` → `User` (extracts left-most base type)
fn extract_type_from_node(type_node: Node<'_>, content: &[u8]) -> Option<String> {
    match type_node.kind() {
        "type" => {
            // The "type" node wraps the actual type - recurse into first child
            type_node
                .named_child(0)
                .and_then(|child| extract_type_from_node(child, content))
        }
        "identifier" => {
            // Simple type: int, str, User
            type_node.utf8_text(content).ok().map(String::from)
        }
        "string" => {
            // Forward reference: "User" -> User
            // Strip surrounding quotes from string literal annotations
            let text = type_node.utf8_text(content).ok()?;
            let trimmed = text.trim();

            // Remove quotes: "User" or 'User' -> User
            if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
            {
                let unquoted = &trimmed[1..trimmed.len() - 1];
                // Handle potential unions inside string: "User | None" -> "User"
                Some(normalize_union_type(unquoted))
            } else {
                Some(trimmed.to_string())
            }
        }
        "binary_operator" => {
            // PEP 604 union: User | None -> User
            // Extract left operand as the primary type
            if let Some(left) = type_node.child_by_field_name("left") {
                extract_type_from_node(left, content)
            } else {
                // Fallback: extract text and normalize
                type_node
                    .utf8_text(content)
                    .ok()
                    .map(|text| normalize_union_type(text.trim()))
            }
        }
        "generic_type" | "subscript" => {
            // Generic type: List[int], Dict[str, int], Optional[User]
            // Extract base type (before the brackets)
            // Structure: subscript { value: identifier, subscript: [...] }
            if let Some(value_node) = type_node.child_by_field_name("value") {
                extract_type_from_node(value_node, content)
            } else {
                // Fallback: try first named child
                type_node
                    .named_child(0)
                    .and_then(|child| extract_type_from_node(child, content))
                    .or_else(|| {
                        // Last resort: extract the full text and take the base type
                        type_node.utf8_text(content).ok().and_then(|text| {
                            // Extract base type from "List[str]" -> "List"
                            text.split('[').next().map(|s| s.trim().to_string())
                        })
                    })
            }
        }
        "attribute" => {
            // Qualified type: module.Type or package.module.Type
            type_node.utf8_text(content).ok().map(String::from)
        }
        "list" | "tuple" | "set" => {
            // Collection literals (though rare in type annotations)
            type_node.utf8_text(content).ok().map(String::from)
        }
        _ => {
            // Fallback: try to extract text from any other node
            // For unknown node types, try to extract intelligently
            let text = type_node.utf8_text(content).ok()?;
            let trimmed = text.trim();

            // If it looks like a generic type, extract base type
            if trimmed.contains('[') {
                trimmed.split('[').next().map(|s| s.trim().to_string())
            } else {
                // Check for union syntax
                Some(normalize_union_type(trimmed))
            }
        }
    }
}

/// Normalize union types by extracting the left-most/primary type.
///
/// Examples:
/// - `User | None` → `User`
/// - `str | int` → `str`
/// - `Optional[User]` → `Optional[User]` (unchanged, not a union)
fn normalize_union_type(type_str: &str) -> String {
    if let Some(pipe_pos) = type_str.find('|') {
        // Extract left side of union and trim
        type_str[..pipe_pos].trim().to_string()
    } else {
        type_str.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_name_extracts_dotted_identifiers() {
        // General dotted identifier handling (for call targets)
        assert_eq!(simple_name("module.func"), "func");
        assert_eq!(simple_name("obj.method"), "method");
        assert_eq!(simple_name("package.module.func"), "func");
        assert_eq!(simple_name("self.helper"), "helper");

        // No dots - return as-is
        assert_eq!(simple_name("function"), "function");
        assert_eq!(simple_name(""), "");
    }

    #[test]
    fn test_ffi_library_simple_name_extracts_library_base_names() {
        // Standard shared library names
        assert_eq!(ffi_library_simple_name("libfoo.so"), "libfoo");
        assert_eq!(ffi_library_simple_name("lib1.so"), "lib1");
        assert_eq!(ffi_library_simple_name("lib2.so"), "lib2");

        // Different extensions
        assert_eq!(ffi_library_simple_name("kernel32.dll"), "kernel32");
        assert_eq!(ffi_library_simple_name("libSystem.dylib"), "libSystem");

        // Versioned shared libraries (libc.so.6)
        assert_eq!(ffi_library_simple_name("libc.so.6"), "libc");

        // No extension - return as-is
        assert_eq!(ffi_library_simple_name("kernel32"), "kernel32");
        assert_eq!(ffi_library_simple_name("numpy"), "numpy");

        // Variable references (prefixed with $)
        assert_eq!(ffi_library_simple_name("$libname"), "$libname");

        // Edge cases
        assert_eq!(ffi_library_simple_name(""), "");
        assert_eq!(ffi_library_simple_name("lib.so"), "lib");
    }

    #[test]
    fn test_ffi_library_simple_name_prevents_duplicate_edges() {
        // This was the bug: lib1.so and lib2.so both became "so"
        let name1 = ffi_library_simple_name("lib1.so");
        let name2 = ffi_library_simple_name("lib2.so");

        // They should be different
        assert_ne!(
            name1, name2,
            "lib1.so and lib2.so must produce different simple names"
        );
        assert_eq!(name1, "lib1");
        assert_eq!(name2, "lib2");
    }

    #[test]
    fn test_ffi_library_simple_name_handles_directory_paths() {
        // Full paths with directories containing dots (Codex finding)
        assert_eq!(ffi_library_simple_name("/opt/v1.2/libfoo.so"), "libfoo");
        assert_eq!(
            ffi_library_simple_name("/usr/lib/x86_64-linux-gnu/libc.so.6"),
            "libc"
        );
        assert_eq!(ffi_library_simple_name("libs/lib1.so"), "lib1");

        // Relative paths
        assert_eq!(ffi_library_simple_name("./libs/kernel32.dll"), "kernel32");
        assert_eq!(
            ffi_library_simple_name("../lib/libSystem.dylib"),
            "libSystem"
        );
    }

    // ====================================================================
    // Route decorator parsing unit tests
    // ====================================================================

    #[test]
    fn test_parse_route_decorator_app_route_default_get() {
        let result = parse_route_decorator_text("app.route('/api/users')");
        assert_eq!(result, Some(("GET".to_string(), "/api/users".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_app_route_with_methods_post() {
        let result = parse_route_decorator_text("app.route('/api/users', methods=['POST'])");
        assert_eq!(result, Some(("POST".to_string(), "/api/users".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_app_route_with_methods_put_double_quotes() {
        let result = parse_route_decorator_text("app.route(\"/api/items\", methods=[\"PUT\"])");
        assert_eq!(result, Some(("PUT".to_string(), "/api/items".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_app_get() {
        let result = parse_route_decorator_text("app.get('/api/users')");
        assert_eq!(result, Some(("GET".to_string(), "/api/users".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_app_post() {
        let result = parse_route_decorator_text("app.post('/api/items')");
        assert_eq!(result, Some(("POST".to_string(), "/api/items".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_app_put() {
        let result = parse_route_decorator_text("app.put('/api/items/1')");
        assert_eq!(
            result,
            Some(("PUT".to_string(), "/api/items/1".to_string()))
        );
    }

    #[test]
    fn test_parse_route_decorator_app_delete() {
        let result = parse_route_decorator_text("app.delete('/api/items/1')");
        assert_eq!(
            result,
            Some(("DELETE".to_string(), "/api/items/1".to_string()))
        );
    }

    #[test]
    fn test_parse_route_decorator_app_patch() {
        let result = parse_route_decorator_text("app.patch('/api/items/1')");
        assert_eq!(
            result,
            Some(("PATCH".to_string(), "/api/items/1".to_string()))
        );
    }

    #[test]
    fn test_parse_route_decorator_router_get_fastapi() {
        let result = parse_route_decorator_text("router.get('/api/users')");
        assert_eq!(result, Some(("GET".to_string(), "/api/users".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_router_post_fastapi() {
        let result = parse_route_decorator_text("router.post('/api/items')");
        assert_eq!(result, Some(("POST".to_string(), "/api/items".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_blueprint_route() {
        let result = parse_route_decorator_text("blueprint.route('/health')");
        assert_eq!(result, Some(("GET".to_string(), "/health".to_string())));
    }

    #[test]
    fn test_parse_route_decorator_unknown_receiver_returns_none() {
        // "server" is not a recognized receiver
        let result = parse_route_decorator_text("server.get('/api/users')");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_route_decorator_unknown_method_returns_none() {
        // "options" is not in the ROUTE_METHOD_NAMES list and is not "route"
        let result = parse_route_decorator_text("app.options('/api/users')");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_route_decorator_no_parens_returns_none() {
        let result = parse_route_decorator_text("app.route");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_route_decorator_no_dot_returns_none() {
        let result = parse_route_decorator_text("route('/api/users')");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_path_from_decorator_args_single_quotes() {
        let result = extract_path_from_decorator_args("'/api/users')");
        assert_eq!(result, Some("/api/users".to_string()));
    }

    #[test]
    fn test_extract_path_from_decorator_args_double_quotes() {
        let result = extract_path_from_decorator_args("\"/api/items\")");
        assert_eq!(result, Some("/api/items".to_string()));
    }

    #[test]
    fn test_extract_path_from_decorator_args_empty_returns_none() {
        let result = extract_path_from_decorator_args("'')");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_path_from_decorator_args_no_string_returns_none() {
        let result = extract_path_from_decorator_args("some_var)");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_method_from_route_args_with_methods_keyword() {
        let result = extract_method_from_route_args("'/api/users', methods=['POST'])");
        assert_eq!(result, "POST");
    }

    #[test]
    fn test_extract_method_from_route_args_without_methods_keyword() {
        let result = extract_method_from_route_args("'/api/users')");
        assert_eq!(result, "GET");
    }

    #[test]
    fn test_extract_method_from_route_args_delete() {
        let result = extract_method_from_route_args("'/api/items', methods=['DELETE'])");
        assert_eq!(result, "DELETE");
    }

    #[test]
    fn test_extract_method_from_route_args_lowercase_normalizes() {
        let result = extract_method_from_route_args("'/x', methods=['put'])");
        assert_eq!(result, "PUT");
    }

    #[test]
    fn test_extract_first_string_literal_single_quotes() {
        let result = extract_first_string_literal("'POST']");
        assert_eq!(result, Some("POST".to_string()));
    }

    #[test]
    fn test_extract_first_string_literal_double_quotes() {
        let result = extract_first_string_literal("\"DELETE\"]");
        assert_eq!(result, Some("DELETE".to_string()));
    }

    #[test]
    fn test_extract_first_string_literal_empty_returns_none() {
        let result = extract_first_string_literal("no quotes here");
        assert_eq!(result, None);
    }
}

#[cfg(test)]
mod shape_tests {
    use super::{cf_bucket_for_python_kind, python_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/reference/sample.py"
    ));

    fn parse(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        let mut p = tree_sitter::Parser::new();
        p.set_language(&lang).expect("load python grammar");
        p.parse(src, None).expect("parse")
    }

    /// Resolve the function_definition with the given name from the fixture.
    fn function_named<'t>(tree: &'t tree_sitter::Tree, name: &str) -> tree_sitter::Node<'t> {
        let root = tree.root_node();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_definition"
                && node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(SAMPLE.as_bytes()).ok())
                    == Some(name)
            {
                return node;
            }
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no function_definition named {name}");
    }

    #[test]
    fn cf_table_is_non_empty() {
        let mapping = python_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        let mut covered = 0;
        for id in 0..lang.node_kind_count() {
            let kid = id as u16;
            if mapping.cf_bucket(kid).is_some() {
                covered += 1;
            }
        }
        assert!(
            covered >= 10,
            "expected many python CF kinds mapped, got {covered}"
        );
    }

    #[test]
    fn histogram_covers_real_control_flow() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            python_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(!d.is_unhashable(), "classify body must be hashable");
        for bucket in [
            CfBucket::Branch,
            CfBucket::Loop,
            CfBucket::Match,
            CfBucket::Try,
            CfBucket::Catch,
            CfBucket::Throw,
            CfBucket::Resource,
            CfBucket::Return,
            CfBucket::BreakContinue,
            CfBucket::Call,
            CfBucket::Assign,
            CfBucket::Comprehension,
        ] {
            assert!(
                d.cf_histogram[bucket.index()] >= 1,
                "classify must exercise {bucket:?}"
            );
        }
    }

    #[test]
    fn async_body_covers_yield_await_closure() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "fetch");
        let d = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            python_shape_mapping(),
            &ShapeBudget::default(),
        );
        assert!(d.cf_histogram[CfBucket::Await.index()] >= 1, "await");
        assert!(d.cf_histogram[CfBucket::Yield.index()] >= 1, "yield");
        assert!(
            d.cf_histogram[CfBucket::Closure.index()] >= 1,
            "lambda closure"
        );
        assert!(
            d.signature_shape.has_return_annotation,
            "-> str return annotation"
        );
    }

    #[test]
    fn signature_shape_reads_arity_and_splats() {
        let tree = parse(SAMPLE);
        let func = function_named(&tree, "classify");
        let mapping = python_shape_mapping();
        let shape = mapping.signature_shape(func, SAMPLE.as_bytes());
        // classify(values, threshold=0, *extra, **opts)
        assert_eq!(
            shape.arity_positional, 2,
            "values + threshold are positional"
        );
        assert!(shape.has_defaults, "threshold=0");
        assert!(shape.has_varargs, "*extra");
        assert!(shape.has_kwargs, "**opts");
    }

    #[test]
    fn unknown_kind_maps_to_none() {
        assert!(cf_bucket_for_python_kind("module").is_none());
        assert!(cf_bucket_for_python_kind("identifier").is_none());
    }
}
