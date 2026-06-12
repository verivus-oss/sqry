use std::collections::{HashMap, HashSet};
use std::path::Path;

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::storage::c_indirect::{BindingSiteKind, IndirectShape};
use sqry_core::graph::unified::{FfiConvention, GraphBuildHelper, GraphSnapshot, StagingGraph};
use sqry_core::graph::{CodeEdge, GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

use crate::relations::scope_index::build_local_scope_index;
use crate::relations::signature_builder::{TypedefChain, build_function_signature};
use crate::relations::type_extractor::{
    extract_all_type_names_from_c_type, extract_type_specifiers_from_declaration,
};

/// Registry of FFI declarations discovered during graph building.
///
/// Maps simple function names (e.g., `printf`) to their qualified FFI name
/// (e.g., `extern::C::printf`) and calling convention. This allows call edge
/// construction to detect when a call targets an FFI function and create
/// `FfiCall` edges instead of regular `Call` edges.
///
/// C extern declarations use the C calling convention by default.
type FfiRegistry = HashMap<String, (String, FfiConvention)>;

const DEFAULT_SCOPE_DEPTH: usize = 4;

/// Graph builder for C files using unified `CodeGraph` architecture.
///
/// This implementation follows the two-phase `ASTGraph` architecture for O(1)
/// context lookups during call edge detection.
///
/// # Supported Features
///
/// - Function definitions (including static functions)
/// - Function declarations (headers)
/// - Function call expressions
/// - Pointer function calls
/// - Static (file-local) functions
/// - FFI declarations (extern functions/variables)
/// - Inline assembly detection (`gnu_asm_expression`)
///
/// # C-Specific Considerations
///
/// - No classes, namespaces, or templates (simpler than C++)
/// - Function declarations vs definitions (header vs implementation)
/// - Static functions have file-local scope (NOT exported)
/// - Non-static functions have external linkage (exported)
/// - Function pointers are tracked but not deeply analyzed
/// - Extern declarations are treated as FFI (calls create `FfiCall` edges)
#[derive(Debug, Clone, Copy)]
pub struct CGraphBuilder {
    max_scope_depth: usize,
}

impl Default for CGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: DEFAULT_SCOPE_DEPTH,
        }
    }
}

impl CGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for CGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::C);

        // Build AST graph for call context tracking
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth);

        // C indirect-call precision (Phase A, U10) — Phase 1 instrumentation.
        //
        // These additive walks each populate a per-file
        // `CIndirectStagingPayload` slot owned by `StagingGraph`. U11
        // (Phase 3 commit) drains the payload into the workspace-global
        // `CIndirectSideTables`; U12 (Pass 5b) consumes it to rewrite
        // synthetic `Calls` edges into precise binding-plane / type-match
        // candidates.
        //
        // Step 1 — single combined pre-pass over the AST that collects both
        // (a) every known C function name in the file (definitions +
        // declarations), used as a predicate by `classify_address_taken_sites`,
        // and (b) the type-permits maps (DESIGN §2.6). These were two
        // independent full-tree walks; PERF-280 fuses them into one
        // traversal. `type_permits` is also consumed by
        // `walk_tree_for_graph` below.
        let (known_fn_names, type_permits) =
            collect_known_fns_and_type_permits(tree.root_node(), content);

        // Step 2 — recursive address-taken classifier walker covering the
        // DESIGN §2.5 pattern table (unary `&` of fn, fn-as-argument,
        // designated/positional initializer RHS, field/subscript
        // assignment RHS, return identifier, init_declarator RHS). Requires
        // the full-file `known_fn_names` + `type_permits` from Step 1.
        classify_address_taken_sites(
            tree.root_node(),
            content,
            &mut helper,
            &known_fn_names,
            &type_permits,
        );

        // Step 3 — per-file local scope index (DESIGN §4.1). The block-
        // scope arena drives U12's `(*fp)(...)` / `fp(...)` resolution by
        // mapping an identifier at a use-site byte offset to its declared
        // type token.
        let scope_index = build_local_scope_index(tree, content);
        helper.set_local_scope_index(scope_index);

        // Two-pass approach for FFI call linking:
        // Pass 1: Collect FFI declarations so calls can be resolved regardless of source order
        let mut ffi_registry = FfiRegistry::new();
        collect_ffi_declarations(tree.root_node(), content, &mut ffi_registry);

        // Track seen includes for deduplication
        let mut seen_includes: HashSet<String> = HashSet::new();

        // Track exported symbols to avoid duplicates (prototype + definition = single export)
        // Covers both functions and FFI constants.
        let mut exported_symbols: HashSet<String> = HashSet::new();

        // Pass 2: Walk tree to find functions, structs, calls, includes, and FFI
        // The ffi_registry is now fully populated, so FFI calls will be properly linked
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &mut seen_includes,
            &mut exported_symbols,
            &ffi_registry,
            &type_permits,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::C
    }

    fn detect_cross_language_edges(&self, _snapshot: &GraphSnapshot) -> GraphResult<Vec<CodeEdge>> {
        // C is a target language (called from Rust FFI, Java JNI, Go CGo, C# P/Invoke)
        // Global detector handles: Rust → C, Java → C, Go → C, C# → C
        // This builder does not detect outbound cross-language edges
        Ok(Vec::new())
    }
}

/// AST context for C code
///
/// This structure provides O(1) lookup of function contexts during
/// call edge detection, avoiding repeated tree traversals.
struct ASTGraph {
    contexts: Vec<FunctionContext>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], max_depth: usize) -> Self {
        let root = tree.root_node();
        let mut contexts = Vec::new();

        // Create recursion guard with configured limit
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .expect("Failed to load recursion limits");
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .expect("Invalid file_ops_depth configuration");
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .expect("Failed to create recursion guard");

        // Extract function contexts with recursion protection
        if let Err(e) =
            extract_function_contexts(root, content, &mut contexts, 0, max_depth, &mut guard)
        {
            // Log recursion error but continue with partial results
            eprintln!("Warning: AST traversal hit recursion limit: {e}");
        }

        Self { contexts }
    }

    fn contexts(&self) -> &[FunctionContext] {
        &self.contexts
    }

    /// Find the function context containing a given node
    fn find_context(&self, node: Node) -> Option<&FunctionContext> {
        let start = node.start_byte();
        let end = node.end_byte();

        self.contexts
            .iter()
            .filter(|ctx| start >= ctx.span.0 && end <= ctx.span.1)
            .min_by_key(|ctx| ctx.span.1 - ctx.span.0)
    }
}

/// Function context in C
#[derive(Debug, Clone)]
struct FunctionContext {
    /// Function name
    name: String,
    /// Whether this is a static (file-local) function
    #[allow(dead_code)] // Reserved for static function analysis
    is_static: bool,
    /// Whether this is a declaration (header) vs definition (implementation)
    #[allow(dead_code)] // Reserved for header/implementation tracking
    is_declaration: bool,
    /// Byte span (start, end)
    span: (usize, usize),
}

impl FunctionContext {
    fn qualified_name(&self) -> String {
        // C doesn't have namespaces, so qualified name is just the function name
        // Static functions are implicitly qualified by file, but we don't encode that here
        self.name.clone()
    }
}

/// Extract function contexts from the AST
///
/// # Errors
///
/// Returns [`RecursionError::DepthLimitExceeded`] if recursion depth exceeds the guard's limit.
fn extract_function_contexts(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<FunctionContext>,
    depth: usize,
    max_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), sqry_core::query::security::RecursionError> {
    // Enter recursion guard
    guard.enter()?;

    if depth > max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        "function_definition" => {
            if let Some(context) = extract_function_definition_context(node, content) {
                contexts.push(context);
            }
        }
        "declaration" => {
            // Check if this is a function declaration (not a variable declaration)
            if is_function_declaration(node)
                && let Some(context) = extract_function_declaration_context(node, content)
            {
                contexts.push(context);
            }
        }
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = node.child(i as u32) {
            extract_function_contexts(child, content, contexts, depth + 1, max_depth, guard)?;
        }
    }

    guard.exit();
    Ok(())
}

/// Extract function context from `function_definition`
fn extract_function_definition_context(node: Node, content: &[u8]) -> Option<FunctionContext> {
    // function_definition has a declarator field
    let declarator_node = node.child_by_field_name("declarator")?;
    let name = extract_function_name_from_declarator(declarator_node, content)?;

    // Check for static storage class
    let is_static = has_static_storage_class(node, content);

    Some(FunctionContext {
        name,
        is_static,
        is_declaration: false,
        span: (node.start_byte(), node.end_byte()),
    })
}

/// Extract function context from function declaration (header)
fn extract_function_declaration_context(node: Node, content: &[u8]) -> Option<FunctionContext> {
    // declaration node contains a function_declarator
    let declarator_node = find_function_declarator(node)?;
    let name = extract_function_name_from_declarator(declarator_node, content)?;

    // Check for static storage class
    let is_static = has_static_storage_class(node, content);

    Some(FunctionContext {
        name,
        is_static,
        is_declaration: true,
        span: (node.start_byte(), node.end_byte()),
    })
}

/// Check if a declaration is a function declaration (not a variable declaration)
fn is_function_declaration(node: Node) -> bool {
    // Look for function_declarator child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return true;
        }
        // Recurse into pointer_declarator, etc.
        if child.kind() == "pointer_declarator" && has_function_declarator_descendant(child) {
            return true;
        }
    }
    false
}

/// Recursively check for `function_declarator` in descendants
fn has_function_declarator_descendant(node: Node) -> bool {
    if node.kind() == "function_declarator" {
        return true;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if has_function_declarator_descendant(child) {
            return true;
        }
    }
    false
}

/// Find `function_declarator` in a declaration node
fn find_function_declarator(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return Some(child);
        }
        // Recurse into pointer_declarator, etc.
        if child.kind() == "pointer_declarator"
            && let Some(func_decl) = find_function_declarator(child)
        {
            return Some(func_decl);
        }
    }
    None
}

/// Extract function name from declarator (handles various declarator forms)
fn extract_function_name_from_declarator(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "function_declarator" => {
            // function_declarator has declarator field (identifier or nested)
            if let Some(decl) = node.child_by_field_name("declarator") {
                return extract_identifier_from_declarator(decl, content);
            }
        }
        "pointer_declarator" => {
            // pointer_declarator has declarator field
            if let Some(decl) = node.child_by_field_name("declarator") {
                return extract_function_name_from_declarator(decl, content);
            }
        }
        "identifier" => {
            return node
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string);
        }
        _ => {}
    }
    None
}

/// Extract identifier from any declarator form
fn extract_identifier_from_declarator(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node
            .utf8_text(content)
            .ok()
            .map(std::string::ToString::to_string),
        "pointer_declarator" | "function_declarator" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                return extract_identifier_from_declarator(decl, content);
            }
            None
        }
        _ => None,
    }
}

/// Check if a node has static storage class specifier.
///
/// Handles various forms:
/// - `static int foo() {}` - direct static specifier
/// - `static inline int bar() {}` - static with inline modifier
/// - Nested specifiers under `declaration_specifiers`
///
/// Note: In C, `static` affects linkage (file-local) regardless of other specifiers.
fn has_static_storage_class(node: Node, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "storage_class_specifier" => {
                if let Ok(text) = child.utf8_text(content)
                    && text == "static"
                {
                    return true;
                }
            }
            "declaration_specifiers" => {
                // Check nested specifiers (e.g., in some grammars storage class
                // may be grouped under declaration_specifiers)
                if has_static_in_specifiers(child, content) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Check for static specifier within a `declaration_specifiers` node.
fn has_static_in_specifiers(node: Node, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "storage_class_specifier"
            && let Ok(text) = child.utf8_text(content)
            && text == "static"
        {
            return true;
        }
    }
    false
}

/// Walk the tree and populate the staging graph.
///
/// Handles:
/// - Function definitions and declarations → Function nodes
/// - Call expressions → Call edges (or `FfiCall` for FFI targets)
/// - Include directives → Import edges
/// - Extern declarations → FFI function nodes
/// - Inline assembly → (markers only, no edges)
///
/// The `ffi_registry` is pre-populated by `collect_ffi_declarations` to ensure
/// FFI calls are properly linked regardless of source code order.
///
/// The `exported_symbols` set tracks which functions have already been exported
/// to avoid duplicates when both a declaration and definition exist in the same file.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    seen_includes: &mut HashSet<String>,
    exported_symbols: &mut HashSet<String>,
    ffi_registry: &FfiRegistry,
    type_permits: &TypePermits,
) -> GraphResult<()> {
    match node.kind() {
        "function_definition" => {
            handle_function_node(node, content, ast_graph, helper, exported_symbols);
        }
        "declaration" => {
            handle_declaration(
                node,
                content,
                ast_graph,
                helper,
                exported_symbols,
                type_permits,
            );
        }
        "call_expression" => {
            handle_call_expression(node, content, ast_graph, helper, ffi_registry);
        }
        "preproc_include" => {
            handle_preproc_include(node, content, helper, seen_includes);
        }
        "struct_specifier" => {
            handle_struct_specifier(node, content, helper);
        }
        "union_specifier" => {
            handle_union_specifier(node, content, helper);
        }
        "enum_specifier" => {
            handle_enum_specifier(node, content, helper);
        }
        "type_definition" => {
            handle_type_definition(node, content, helper);
        }
        "preproc_def" | "preproc_function_def" => {
            handle_macro_definition(node, content, helper, exported_symbols);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph(
            child,
            content,
            ast_graph,
            helper,
            seen_includes,
            exported_symbols,
            ffi_registry,
            type_permits,
        )?;
    }

    Ok(())
}

fn handle_function_node(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    exported_symbols: &mut HashSet<String>,
) {
    let Some(fn_context) = ast_graph
        .contexts()
        .iter()
        .find(|ctx| node.start_byte() >= ctx.span.0 && node.end_byte() <= ctx.span.1)
    else {
        return;
    };

    let span = span_from_node(node);
    let qname = fn_context.qualified_name();

    // Determine visibility: static = private, non-static = public
    let is_static = has_static_storage_class(node, content);
    let visibility = if is_static { "private" } else { "public" };

    // Add function node with visibility
    let fn_id = helper.add_function_with_visibility(
        &qname,
        Some(span),
        false, // C doesn't have async
        false, // C doesn't have unsafe keyword
        Some(visibility),
    );

    // Non-static functions have external linkage and are exported
    // Static functions are file-local and NOT exported
    // Only export if not already exported (prevents duplicates from decl+def)
    if !is_static && exported_symbols.insert(qname.clone()) {
        let file_name = helper
            .file_path()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(helper.file_path())
            .to_string();
        let module_id = helper.add_module(&file_name, None);
        helper.add_export_edge(module_id, fn_id);
    }

    // Process parameters and return types for TypeOf/Reference edges
    process_function_parameters(node, &qname, content, helper);
    process_function_returns(node, &qname, content, helper);
}

fn handle_declaration(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    exported_symbols: &mut HashSet<String>,
    type_permits: &TypePermits,
) {
    if has_extern_storage_class(node, content) {
        build_ffi_declaration_for_staging(node, content, helper, exported_symbols);
        return;
    }

    if is_function_declaration(node) {
        handle_function_node(node, content, ast_graph, helper, exported_symbols);
        return;
    }

    handle_variable_declaration(node, content, helper, exported_symbols, type_permits);

    // Process variable TypeOf/Reference edges for non-function declarations
    process_variable_typeof_edges(node, content, helper);
}

fn handle_call_expression(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    ffi_registry: &FfiRegistry,
) {
    let Ok(Some((caller_qualified, target_qualified, argument_count, span))) =
        build_call_for_staging(ast_graph, node, content)
    else {
        return;
    };

    let caller_function_id =
        helper.ensure_callee(&caller_qualified, span, CalleeKindHint::Function);

    // C indirect-call precision (Phase A, U10): for `field_expression` /
    // `pointer_expression` callees, capture a `PendingIndirectCallsite`
    // alongside today's synthetic stub edge. U12's `pass5b_c_indirect`
    // rewrites the stub edge into precise candidates resolved via the
    // binding plane / type-match path. Direct (`identifier`) callees stay
    // on the existing direct-call path.
    if let Some(function_node) = node.child_by_field_name("function")
        && let Some(shape) = classify_indirect_callsite_shape(function_node, content)
    {
        let arg_count_u32 = u32::try_from(argument_count).unwrap_or(u32::MAX);
        helper.push_indirect_callsite(
            &caller_qualified,
            (node.start_byte(), node.end_byte()),
            shape,
            arg_count_u32,
            // C `call_expression` has no async-marker concept; the field
            // exists for parity with `EdgeKind::Calls.is_async`.
            false,
        );
    }

    if let Some((ffi_qualified, ffi_convention)) = ffi_registry.get(&target_qualified) {
        let ffi_target_id = helper.ensure_callee(ffi_qualified, span, CalleeKindHint::Function);
        helper.add_ffi_edge(caller_function_id, ffi_target_id, *ffi_convention);
        return;
    }

    let target_function_id =
        helper.ensure_callee(&target_qualified, span, CalleeKindHint::Function);
    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
    helper.add_call_edge_full_with_span(
        caller_function_id,
        target_function_id,
        argument_count,
        false,
        vec![span],
    );
}

/// Classify an indirect call's callee node into a [`IndirectShape`].
///
/// Returns `None` for direct (`identifier`) callees — those stay on the
/// existing direct-call resolution path. For `field_expression` and
/// `pointer_expression` callees, returns the shape U12's resolver
/// dispatches on.
///
/// `field_expression` example: `obj->cb(...)` / `obj.cb(...)`. The
/// receiver is the `argument` field child; the field name is the `field`
/// child.
///
/// `pointer_expression` example: `(*fp)(...)`. The variable name is the
/// `argument` field child. Bare `fp(...)` (an `identifier` callee whose
/// declared type is a function pointer) flows through the direct-call
/// path today; U12 detects it post-hoc via `LocalScopeIndex` lookup, so
/// it does not produce a `PendingIndirectCallsite` here.
fn classify_indirect_callsite_shape(
    function_node: Node<'_>,
    content: &[u8],
) -> Option<IndirectShape> {
    match function_node.kind() {
        "field_expression" => {
            let receiver_node = function_node.child_by_field_name("argument")?;
            let field_node = function_node.child_by_field_name("field")?;
            let receiver_name = receiver_node.utf8_text(content).ok()?.trim().to_string();
            let field_name = field_node.utf8_text(content).ok()?.trim().to_string();
            if receiver_name.is_empty() || field_name.is_empty() {
                return None;
            }
            Some(IndirectShape::FieldExpr {
                receiver_name,
                field_name,
            })
        }
        "pointer_expression" => {
            let argument = function_node.child_by_field_name("argument")?;
            // The inner argument is typically `identifier` (`(*fp)(...)`)
            // or a `parenthesized_expression` wrapping one. Walk inward
            // until we hit an identifier or give up.
            let var_name = unwrap_pointer_expr_var_name(argument, content)?;
            Some(IndirectShape::PointerExpr { var_name })
        }
        // tree-sitter-c shapes `(*fp)(...)` as
        // `call_expression { function: parenthesized_expression { pointer_expression { identifier } } }`,
        // so the call's `function` field is `parenthesized_expression`,
        // not `pointer_expression`. Unwrap one level and dispatch.
        "parenthesized_expression" => {
            let inner = function_node.named_child(0)?;
            classify_indirect_callsite_shape(inner, content)
        }
        _ => None,
    }
}

/// Recursively unwrap `parenthesized_expression` / `pointer_expression`
/// wrappers around the function-pointer variable identifier inside a
/// `(*fp)(...)` callee shape.
///
/// Returns the bare identifier text. Returns `None` if the inner-most
/// node is not an `identifier` (e.g. `(*(struct ops *)p)(...)` — a cast
/// expression that U12 does not resolve in Phase A).
fn unwrap_pointer_expr_var_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => {
            let name = node.utf8_text(content).ok()?.trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        }
        "parenthesized_expression" | "pointer_expression" => {
            // tree-sitter-c puts the inner expression as the first named
            // child for both wrapper kinds.
            let inner = node.named_child(0)?;
            unwrap_pointer_expr_var_name(inner, content)
        }
        _ => None,
    }
}

fn module_id_for_file(helper: &mut GraphBuildHelper) -> sqry_core::graph::unified::NodeId {
    let file_name = helper
        .file_path()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(helper.file_path())
        .to_string();
    helper.add_module(&file_name, None)
}

fn handle_variable_declaration(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    exported_symbols: &mut HashSet<String>,
    type_permits: &TypePermits,
) {
    let declarations = extract_declarator_names(node, content);
    if declarations.is_empty() {
        return;
    }

    let is_top_level = is_top_level_declaration(node);
    if !is_top_level {
        return;
    }
    let is_static = has_static_storage_class(node, content);
    let module_id = if is_top_level && !is_static {
        Some(module_id_for_file(helper))
    } else {
        None
    };

    // Extract the enclosing struct tag (if any) for binding-plane capture.
    // For `struct file_operations ops = { ... }`, the tag is
    // `file_operations`. Returns `None` for non-struct declarations (e.g.
    // primitive types, unions, or anonymous types), which short-circuits
    // binding capture without affecting the existing References-edge
    // path.
    let struct_tag = extract_enclosing_struct_tag(node, content);

    for (name, span) in declarations {
        let var_id = helper.add_variable(&name, Some(span));
        if let Some(module_id) = module_id
            && exported_symbols.insert(name.clone())
        {
            helper.add_export_edge(module_id, var_id);
        }

        // Detect function pointer assignments in designated initializers.
        // Patterns like: `const struct file_operations ops = { .read = my_func, ... };`
        // create References edges from the variable to each assigned function.
        extract_designated_initializer_targets(
            node,
            content,
            &name,
            var_id,
            helper,
            struct_tag.as_deref(),
            type_permits,
        );
    }
}

/// Extract the struct tag from the type specifier of a declaration node, if
/// the declared type is `struct <Tag>` (with `<Tag>` named).
///
/// Returns `None` for primitive types, anonymous structs, unions, enums,
/// or typedef-named types. The tag is the bare struct name (e.g.
/// `"file_operations"` for `struct file_operations`).
///
/// Used by `extract_designated_initializer_targets` /
/// `extract_initializer_list_targets` to attach binding-plane entries to
/// `(struct_tag, field_name)` keys for U12's resolver.
fn extract_enclosing_struct_tag(decl_node: Node<'_>, content: &[u8]) -> Option<String> {
    let type_node = decl_node.child_by_field_name("type")?;
    if type_node.kind() != "struct_specifier" {
        return None;
    }
    let name_node = type_node.child_by_field_name("name")?;
    let tag = name_node.utf8_text(content).ok()?.trim().to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

/// Extract function pointer targets from designated initializers.
///
/// Scans the `init_declarator` matching `var_name` for `initializer_pair` nodes
/// where the value is an identifier (function name). Creates `References` edges
/// from the containing variable to each referenced function.
///
/// Only processes the declarator that matches `var_name`, avoiding misattribution
/// when a declaration defines multiple variables (e.g., `struct ops a = {...}, b = {...};`).
///
/// This enables sqry to resolve C "vtable" patterns like the Linux kernel's
/// `file_operations`, `net_device_ops`, etc.
fn extract_designated_initializer_targets(
    decl_node: Node,
    content: &[u8],
    var_name: &str,
    var_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
    struct_tag: Option<&str>,
    type_permits: &TypePermits,
) {
    // Walk the declaration to find the init_declarator matching this variable
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() != "init_declarator" {
            continue;
        }

        // Check if this init_declarator belongs to the current variable
        let Some((decl_name, _)) = extract_declarator_name(child, content) else {
            continue;
        };
        if decl_name != var_name {
            continue;
        }

        // Find the initializer_list within the matching init_declarator
        let mut inner_cursor = child.walk();
        for init_child in child.children(&mut inner_cursor) {
            if init_child.kind() != "initializer_list" {
                continue;
            }

            extract_initializer_list_targets(
                init_child,
                content,
                var_name,
                var_id,
                helper,
                struct_tag,
                type_permits,
            );
        }

        // Found the matching declarator — no need to continue
        break;
    }
}

/// Extract function pointer targets from an `initializer_list` node.
///
/// Handles both shapes of aggregate initializer (DESIGN §2.5 / §3.1.1):
///
/// * **Designated** — `{ .field = function_name }`. Children are
///   `initializer_pair` nodes; the field name is the leading designator
///   and the value is the trailing identifier.
/// * **Positional** — `{ function_name, ... }`. Children are bare
///   identifiers ordered to match the struct's declared field layout.
///   The field name is not directly available; the binding is keyed on
///   `(struct_tag, "<positional>")` (empty `field_name` placeholder)
///   so U12's resolver can still consume it via the type-match path.
///
/// For each function-name target the function additionally:
///
/// * pushes a [`PendingBinding`] under `(struct_tag, field_name)` keyed
///   on `instance_name` (only when `struct_tag` is `Some` and, for
///   positional slots, when the slot's declared field type is a function
///   pointer per DESIGN §2.6 row 4).
///
/// **Address-taken marks are NOT pushed here** (DESIGN §2.6, U10 iter-3).
/// The single source of truth for `pending_address_taken_names` is
/// `classify_address_taken_sites`, which already applies the
/// fnptr-slot guard via `positional_init_slot_is_fnptr` (pattern 4) and
/// covers the unguarded designated-pair arm (pattern 3). Re-marking from
/// this legacy path would bypass that guard for top-level positional
/// initializers (e.g. `struct S { int x; }; struct S s = { f };` where
/// the slot is not fnptr) and falsely mark `f` as address-taken.
fn extract_initializer_list_targets(
    list_node: Node,
    content: &[u8],
    instance_name: &str,
    var_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
    struct_tag: Option<&str>,
    type_permits: &TypePermits,
) {
    let mut pair_cursor = list_node.walk();
    // Track the positional index across **named** children that are bare
    // identifiers (matching the indexing convention of
    // `classify_address_taken_recursive`'s `initializer_list` arm). Used
    // to look up the slot's declared field type for the positional
    // binding-push guard (DESIGN §2.6 row 4).
    let mut positional_index: usize = 0;
    for pair in list_node.named_children(&mut pair_cursor) {
        match pair.kind() {
            "initializer_pair" => {
                // Designated initializer: `.field = function_name`.
                // Designated entries do NOT participate in the positional
                // index — they consume their slot by name, not order.
                #[allow(clippy::cast_possible_truncation)]
                // Graph storage: node/edge index counts fit in u32
                let child_count = pair.named_child_count() as u32;
                if child_count < 2 {
                    continue;
                }

                let Some(value_node) = pair.named_child(child_count - 1) else {
                    continue;
                };

                if value_node.kind() != "identifier" {
                    continue;
                }

                let Ok(func_name) = value_node.utf8_text(content) else {
                    continue;
                };

                if is_skipped_init_value(func_name) {
                    continue;
                }

                let target_id = helper.ensure_callee(
                    func_name,
                    span_from_node(value_node),
                    CalleeKindHint::Function,
                );
                helper.add_reference_edge(var_id, target_id);

                // C indirect-call precision (U10): push a designated
                // `PendingBinding` (when we know the enclosing struct
                // tag). The address-taken mark is applied by
                // `classify_address_taken_sites` (pattern 3, unguarded
                // per SPEC §3.1.1 row 3), not here.
                if let Some(tag) = struct_tag {
                    let field_name = extract_designator_field_name(pair, content);
                    if let Some(field) = field_name {
                        helper.push_binding(
                            tag,
                            &field,
                            instance_name,
                            func_name,
                            BindingSiteKind::DesignatedInitializer,
                        );
                    }
                }
            }
            "identifier" => {
                // Positional initializer slot: `{ function_name, ... }`.
                let slot_index = positional_index;
                positional_index += 1;

                let Ok(func_name) = pair.utf8_text(content) else {
                    continue;
                };
                if is_skipped_init_value(func_name) {
                    continue;
                }

                // Binding capture only fires when we know the enclosing
                // struct tag AND the slot's declared field type is a
                // function pointer (DESIGN §2.6 row 4, SPEC §3.1.1
                // row 4). Without the guard, top-level
                // `struct S { int x; }; struct S s = { f };` would push
                // a `(S, <positional>, s -> f)` binding, which is wrong:
                // `f` is not bound to a function-pointer slot.
                //
                // The field name is a placeholder (`<positional>`)
                // because tree-sitter-c does not give us the declared
                // field name from the initializer alone. U12's resolver
                // consumes positional bindings via the type-match path
                // (matching the function-pointer signature against the
                // struct's field-signature table) rather than the
                // binding-plane key.
                //
                // The address-taken mark for this slot is applied by
                // `classify_address_taken_sites` (pattern 4, guarded by
                // `positional_init_slot_is_fnptr`), not here.
                if let Some(tag) = struct_tag
                    && positional_init_slot_is_fnptr(list_node, slot_index, content, type_permits)
                {
                    helper.push_binding(
                        tag,
                        "<positional>",
                        instance_name,
                        func_name,
                        BindingSiteKind::PositionalInitializer,
                    );
                }
            }
            _ => {
                // Numeric literals, string literals, nested initializer
                // lists, etc. — not a function reference. These DO
                // consume a positional slot for layout purposes (the
                // `_` arm cannot be `initializer_pair` because that's
                // matched above), so the index advances.
                positional_index += 1;
            }
        }
    }
}

/// Common skip filter for initializer values: `NULL`, `nullptr`, and
/// all-uppercase macro-shaped identifiers that look like constants
/// (e.g. `INT_MAX`, `PAGE_SIZE`).
fn is_skipped_init_value(text: &str) -> bool {
    text == "NULL" || text == "nullptr" || text.chars().all(|c| c.is_uppercase() || c == '_')
}

/// Extract the field name from a designated `initializer_pair`'s
/// designator.
///
/// tree-sitter-c shapes a designated initializer as
/// `initializer_pair { designator: field_designator(field_identifier), value: <expr> }`.
/// We pull the `field_identifier` text. Returns `None` for malformed input
/// or for designators that are not field designators (e.g. array
/// subscript designators `[0]`, which don't participate in struct
/// binding-plane capture).
fn extract_designator_field_name(pair: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = pair.walk();
    for child in pair.named_children(&mut cursor) {
        if child.kind() == "field_designator" {
            // field_designator wraps a field_identifier child.
            let mut inner = child.walk();
            for id in child.named_children(&mut inner) {
                if id.kind() == "field_identifier" {
                    let text = id.utf8_text(content).ok()?.trim().to_string();
                    if text.is_empty() {
                        return None;
                    }
                    return Some(text);
                }
            }
        }
    }
    None
}

fn extract_declarator_names(node: Node, content: &[u8]) -> Vec<(String, Span)> {
    let mut names = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "init_declarator" | "declarator" | "array_declarator" => {
                if let Some((name, span)) = extract_declarator_name(child, content) {
                    names.push((name, span));
                }
            }
            _ => {}
        }
    }

    names
}

fn extract_declarator_name(node: Node, content: &[u8]) -> Option<(String, Span)> {
    match node.kind() {
        "identifier" => node
            .utf8_text(content)
            .ok()
            .map(|text| (text.to_string(), span_from_node(node))),
        "init_declarator"
        | "declarator"
        | "array_declarator"
        | "pointer_declarator"
        | "function_declarator"
        | "parenthesized_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_declarator_name(inner, content);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(result) = extract_declarator_name(child, content) {
                    return Some(result);
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(result) = extract_declarator_name(child, content) {
                    return Some(result);
                }
            }
            None
        }
    }
}

fn is_top_level_declaration(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "translation_unit" {
            return true;
        }
        if parent.kind() == "function_definition" {
            return false;
        }
        current = parent.parent();
    }
    false
}

fn handle_struct_specifier(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(content)
    {
        let name = name.trim();
        if !name.is_empty() {
            helper.add_struct(name, Some(span_from_node(node)));

            // Process struct fields for TypeOf/Reference edges
            process_struct_fields(node, name, content, helper);
        }
    }
}

fn handle_union_specifier(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(content)
    {
        let name = name.trim();
        if !name.is_empty() {
            // Use add_struct for unions (they're similar in the graph)
            helper.add_struct(name, Some(span_from_node(node)));

            // Process union fields for TypeOf/Reference edges
            process_union_fields(node, name, content, helper);
        }
    }
}

fn handle_enum_specifier(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(content)
    {
        let name = name.trim();
        if !name.is_empty() {
            helper.add_enum(name, Some(span_from_node(node)));
        }
    }
}

fn handle_type_definition(node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    let Some(name) = extract_typedef_name(node, content) else {
        return;
    };
    if name.trim().is_empty() {
        return;
    }
    helper.add_type(&name, Some(span_from_node(node)));

    // Process typedef TypeOf/Reference edges
    process_typedef_edges(node, content, helper);
}

fn extract_typedef_name(node: Node, content: &[u8]) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    extract_typedef_name_from_declarator(declarator, content)
}

fn extract_typedef_name_from_declarator(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" => node
            .utf8_text(content)
            .ok()
            .map(std::string::ToString::to_string),
        "pointer_declarator" | "function_declarator" | "parenthesized_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_typedef_name_from_declarator(inner, content);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = extract_typedef_name_from_declarator(child, content) {
                    return Some(name);
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = extract_typedef_name_from_declarator(child, content) {
                    return Some(name);
                }
            }
            None
        }
    }
}

fn handle_macro_definition(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    exported_symbols: &mut HashSet<String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(content) else {
        return;
    };
    let name = name.trim();
    if name.is_empty() {
        return;
    }

    let const_id = helper.add_constant(name, Some(span_from_node(node)));
    if exported_symbols.insert(name.to_string()) {
        let module_id = module_id_for_file(helper);
        helper.add_export_edge(module_id, const_id);
    }
}

fn handle_preproc_include(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    seen_includes: &mut HashSet<String>,
) {
    let Some((header_path, _is_system)) = extract_include_path(node, content) else {
        return;
    };

    if !seen_includes.insert(header_path.clone()) {
        return;
    }

    let span = span_from_node(node);

    // Create a module node for the current file (importer)
    // Extract file name to avoid borrow conflict
    let file_name = helper
        .file_path()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(helper.file_path())
        .to_string();
    let importer_module_id = helper.add_module(&file_name, None);

    // Create an import node for the included header
    let imported_header_id = helper.add_import(&header_path, Some(span));

    // Add import edge: file imports header
    // Note: Include type (system/local) is NOT stored in alias field
    // per EDGE_SCHEMA_CONTRACT - alias is for import renaming only.
    // The include type can be inferred from the header path pattern
    // (e.g., presence of "/" prefix, standard library names, etc.)
    // or stored as node metadata if needed in the future.
    helper.add_import_edge(importer_module_id, imported_header_id);
}

/// Extract the include path from a `preproc_include` node.
///
/// Returns `Some((header_path, is_system))` where:
/// - `header_path` is the normalized path (e.g., "stdio.h", "user.h", "sys/types.h")
/// - `is_system` is true for system includes (`<...>`) and false for local includes (`"..."`)
///
/// Returns `None` if the include cannot be parsed.
fn extract_include_path(node: Node, content: &[u8]) -> Option<(String, bool)> {
    // preproc_include has a path child which can be:
    // - system_lib_string: <stdio.h>
    // - string_literal: "user.h"
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "system_lib_string" => {
                // System include: <stdio.h>
                if let Ok(text) = child.utf8_text(content) {
                    // Strip angle brackets
                    let path = text.trim_start_matches('<').trim_end_matches('>').trim();
                    if !path.is_empty() {
                        return Some((path.to_string(), true));
                    }
                }
            }
            "string_literal" => {
                // Local include: "user.h"
                if let Ok(text) = child.utf8_text(content) {
                    // Strip quotes
                    let path = text.trim_start_matches('"').trim_end_matches('"').trim();
                    if !path.is_empty() {
                        return Some((path.to_string(), false));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<(String, String, usize, Span)>> {
    // Find the calling function context
    let call_context = ast_graph.find_context(call_node);
    let caller_qualified = if let Some(ctx) = call_context {
        ctx.qualified_name()
    } else {
        // Top-level call (e.g., global initializer)
        return Ok(None);
    };

    let Some(function_node) = call_node.child_by_field_name("function") else {
        return Ok(None);
    };

    let callee_text = function_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: span_from_node(call_node),
            reason: "failed to read call expression".to_string(),
        })?
        .trim();

    if callee_text.is_empty() {
        return Ok(None);
    }

    // Extract callee name (handle field expressions, pointers, etc.)
    let callee_name =
        extract_call_target(function_node, content).unwrap_or_else(|_| callee_text.to_string());

    // Count arguments
    let argument_count = count_arguments(call_node);

    let span = span_from_node(call_node);

    Ok(Some((caller_qualified, callee_name, argument_count, span)))
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        sqry_core::graph::node::Position::new(start.row, start.column),
        sqry_core::graph::node::Position::new(end.row, end.column),
    )
}

/// Extract the target of a call (function name or field access)
fn extract_call_target(node: Node, content: &[u8]) -> GraphResult<String> {
    match node.kind() {
        "identifier" => {
            // Simple function call: foo()
            node.utf8_text(content)
                .map(std::string::ToString::to_string)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: Span::from_bytes(node.start_byte(), node.end_byte()),
                    reason: "Invalid UTF-8 in identifier".to_string(),
                })
        }
        "field_expression" => {
            // Struct field access: obj.field() or obj->field()
            if let Some(field) = node.child_by_field_name("field")
                && let Ok(field_name) = field.utf8_text(content)
            {
                return Ok(field_name.to_string());
            }
            Err(GraphBuilderError::ParseError {
                span: Span::from_bytes(node.start_byte(), node.end_byte()),
                reason: "Failed to parse field_expression".to_string(),
            })
        }
        "pointer_expression" => {
            // Dereference: (*fn_ptr)()
            // Extract the argument of the pointer expression
            if let Some(arg) = node.child_by_field_name("argument") {
                return extract_call_target(arg, content);
            }
            Err(GraphBuilderError::ParseError {
                span: Span::from_bytes(node.start_byte(), node.end_byte()),
                reason: "Failed to parse pointer_expression".to_string(),
            })
        }
        _ => {
            // Unknown call target type - try to extract text
            node.utf8_text(content)
                .map(std::string::ToString::to_string)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: Span::from_bytes(node.start_byte(), node.end_byte()),
                    reason: format!("Unknown call target kind: {}", node.kind()),
                })
        }
    }
}

/// Count arguments in a `call_expression`
fn count_arguments(node: Node) -> usize {
    if let Some(args_node) = node.child_by_field_name("arguments") {
        // arguments is an argument_list
        args_node
            .children(&mut args_node.walk())
            .filter(|child| {
                !child.kind().contains('(')
                    && !child.kind().contains(')')
                    && !child.kind().contains(',')
            })
            .count()
    } else {
        0
    }
}

// =============================================================================
// FFI (Foreign Function Interface) Support
// =============================================================================

/// Check if a declaration has extern storage class specifier.
///
/// Returns true for declarations like:
/// - `extern int printf(const char*, ...);`
/// - `extern void *malloc(size_t);`
/// - `extern int errno;`
fn has_extern_storage_class(node: Node, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "storage_class_specifier"
            && let Ok(text) = child.utf8_text(content)
            && text == "extern"
        {
            return true;
        }
    }
    false
}

/// Collect FFI declarations from extern declarations (Pass 1).
///
/// This function walks the entire AST to find all `extern` declarations
/// and populates the FFI registry with function name → (qualified name, convention)
/// mappings. This must be done before processing calls so that FFI calls can be
/// properly linked regardless of source code order.
///
/// C extern declarations always use the C calling convention.
fn collect_ffi_declarations(node: Node<'_>, content: &[u8], ffi_registry: &mut FfiRegistry) {
    if node.kind() == "declaration" && has_extern_storage_class(node, content) {
        // Check if this is a function declaration (not a variable)
        if is_function_declaration(node)
            && let Some(fn_name) = extract_extern_function_name(node, content)
        {
            let qualified = format!("extern::C::{fn_name}");
            ffi_registry.insert(fn_name, (qualified, FfiConvention::C));
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ffi_declarations(child, content, ffi_registry);
    }
}

/// Extract function name from an extern function declaration.
///
/// Handles various forms:
/// - `extern int printf(...);` → "printf"
/// - `extern void *malloc(...);` → "malloc" (pointer return)
fn extract_extern_function_name(node: Node, content: &[u8]) -> Option<String> {
    // Look for function_declarator or pointer_declarator containing one
    if let Some(func_decl) = find_function_declarator(node) {
        return extract_function_name_from_declarator(func_decl, content);
    }
    None
}

/// Extract variable name from an extern variable declaration.
///
/// Handles:
/// - `extern int errno;` → "errno"
/// - `extern char **environ;` → "environ" (pointer types)
fn extract_extern_variable_name(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // Direct identifier: extern int errno;
                return child
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "pointer_declarator" => {
                // Pointer type: extern char **environ;
                // Recursively find the identifier inside pointer_declarator
                if let Some(name) = extract_identifier_from_pointer_declarator(child, content) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract identifier from a `pointer_declarator` (for extern pointer variables).
///
/// Handles nested pointers like `**environ` by recursively descending.
fn extract_identifier_from_pointer_declarator(node: Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                return child
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "pointer_declarator" => {
                // Nested pointer - recurse
                return extract_identifier_from_pointer_declarator(child, content);
            }
            _ => {}
        }
    }
    None
}

/// Build FFI function/variable declarations (Pass 2).
///
/// Handles `extern` declarations:
/// - `extern int func(...)` - FFI function declarations
/// - `extern int var` - FFI static variable declarations
///
/// Creates Function/Constant nodes for FFI declarations. The FFI registry is
/// pre-populated by `collect_ffi_declarations` so FFI calls can be linked properly.
///
/// The `exported_symbols` set is used to avoid duplicate exports.
fn build_ffi_declaration_for_staging(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    exported_symbols: &mut HashSet<String>,
) {
    let span = span_from_node(node);

    if is_function_declaration(node) {
        // FFI function declaration
        if let Some(fn_name) = extract_extern_function_name(node, content)
            && !fn_name.is_empty()
        {
            // Qualify with extern::C:: context
            let qualified = format!("extern::C::{fn_name}");
            // Add as unsafe function (FFI functions are inherently unsafe to call)
            let fn_id = helper.add_function(
                &qualified,
                Some(span),
                false, // not async
                true,  // unsafe (FFI)
            );
            // Export FFI functions so they're visible (with deduplication)
            export_ffi_function(helper, fn_id, &qualified, exported_symbols);
        }
    } else {
        // FFI static variable declaration
        if let Some(var_name) = extract_extern_variable_name(node, content)
            && !var_name.is_empty()
        {
            // Qualify with extern::C:: context
            let qualified = format!("extern::C::{var_name}");
            let var_id = helper.add_constant(&qualified, Some(span));
            // Export FFI statics so they're visible (with deduplication)
            export_ffi_constant(helper, var_id, &qualified, exported_symbols);
        }
    }
}

/// Export an FFI function from the file module (with deduplication).
fn export_ffi_function(
    helper: &mut GraphBuildHelper,
    fn_id: sqry_core::graph::unified::NodeId,
    qualified_name: &str,
    exported_symbols: &mut HashSet<String>,
) {
    // Only export if not already exported
    if exported_symbols.insert(qualified_name.to_string()) {
        let file_name = helper
            .file_path()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(helper.file_path())
            .to_string();
        let module_id = helper.add_module(&file_name, None);
        helper.add_export_edge(module_id, fn_id);
    }
}

/// Export an FFI constant from the file module (with deduplication).
fn export_ffi_constant(
    helper: &mut GraphBuildHelper,
    const_id: sqry_core::graph::unified::NodeId,
    qualified_name: &str,
    exported_symbols: &mut HashSet<String>,
) {
    // Only export if not already exported
    if exported_symbols.insert(qualified_name.to_string()) {
        let file_name = helper
            .file_path()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(helper.file_path())
            .to_string();
        let module_id = helper.add_module(&file_name, None);
        helper.add_export_edge(module_id, const_id);
    }
}

//
// ═══════════════════════════════════════════════════════════════════════════
// TypeOf and Reference Edge Processing
// ═══════════════════════════════════════════════════════════════════════════
//

/// Process function parameters to create `TypeOf` and Reference edges.
///
/// Extracts parameter types from function declarations/definitions and creates:
/// - `TypeOf` edges with Parameter context (including index and name metadata)
/// - Reference edges to all type names referenced in parameter types
fn process_function_parameters(
    func_node: Node,
    func_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Find the function_declarator or pointer_declarator containing parameters
    let Some(declarator) = func_node.child_by_field_name("declarator") else {
        return;
    };

    let Some(params_node) = find_parameter_list(declarator) else {
        return;
    };

    // Collect parameter declarations
    let mut cursor = params_node.walk();
    let param_decls: Vec<Node> = params_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "parameter_declaration")
        .collect();

    // Check for f(void) - single parameter with type "void" and no declarator
    // In C, this means "no parameters" and should not emit parameter edges
    if param_decls.len() == 1 {
        let param = param_decls[0];
        let type_names = extract_type_specifiers_from_declaration(param, content);
        let has_declarator = param.child_by_field_name("declarator").is_some();

        if type_names.len() == 1 && type_names[0] == "void" && !has_declarator {
            // This is f(void) - no parameters, skip processing
            return;
        }
    }

    // Process each parameter
    for (param_index, param_decl) in param_decls.iter().enumerate() {
        process_single_parameter(func_name, *param_decl, param_index, content, helper);
    }
}

/// Find the `parameter_list` node within a declarator tree.
///
/// Handles complex declarators like `pointer_declarator` wrapping `function_declarator`.
fn find_parameter_list(declarator: Node) -> Option<Node> {
    // Check if this is a function_declarator with parameters
    if declarator.kind() == "function_declarator" {
        return declarator.child_by_field_name("parameters");
    }

    // Recurse into nested declarators (e.g., pointer_declarator)
    for i in 0..declarator.child_count() {
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        if let Some(child) = declarator.child(i as u32)
            && let Some(params) = find_parameter_list(child)
        {
            return Some(params);
        }
    }

    None
}

/// Process a single parameter declaration to create `TypeOf` and Reference edges.
fn process_single_parameter(
    func_name: &str,
    param_node: Node,
    param_index: usize,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract parameter name if present
    let param_name = extract_parameter_name(param_node, content);

    // Extract type specifiers from the parameter
    let type_names = extract_type_specifiers_from_declaration(param_node, content);

    // Extract the declarator for additional type info (pointers, arrays, etc.)
    let mut all_types = type_names.clone();
    if let Some(declarator) = param_node.child_by_field_name("declarator") {
        all_types.extend(extract_all_type_names_from_c_type(declarator, content));
    }

    // If we have type information, create edges
    if !type_names.is_empty() {
        create_parameter_edges(
            func_name,
            param_index,
            param_name.as_deref(),
            &type_names.join(" "),
            &all_types,
            helper,
        );
    }
}

/// Extract parameter name from a `parameter_declaration` node.
fn extract_parameter_name(param_node: Node, content: &[u8]) -> Option<String> {
    // Look for the declarator which contains the name
    if let Some(declarator) = param_node.child_by_field_name("declarator") {
        extract_simple_declarator_name(declarator, content)
    } else {
        // Abstract declarator (no name)
        None
    }
}

/// Extract the identifier name from a declarator (handles nested declarators).
fn extract_simple_declarator_name(declarator: Node, content: &[u8]) -> Option<String> {
    match declarator.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            declarator.utf8_text(content).ok().map(String::from)
        }
        "pointer_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator" => {
            // Recurse into nested declarator
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                extract_simple_declarator_name(nested, content)
            } else {
                // Fallback: walk children to find identifier
                let mut cursor = declarator.walk();
                for child in declarator.children(&mut cursor) {
                    if let Some(name) = extract_simple_declarator_name(child, content) {
                        return Some(name);
                    }
                }
                None
            }
        }
        _ => None,
    }
}

/// Create `TypeOf` and Reference edges for a function parameter.
fn create_parameter_edges(
    func_name: &str,
    index: usize,
    name: Option<&str>,
    type_text: &str,
    referenced_types: &[String],
    helper: &mut GraphBuildHelper,
) {
    // Get or create function node
    let func_id = helper.add_function(func_name, None, false, false);

    // Create TypeOf edge: function → parameter type with Parameter context
    let type_id = helper.add_type(type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        u16::try_from(index).ok(),
        name,
    );

    // Create Reference edges to all referenced types
    for ref_type in referenced_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Check if a declarator has indirection (pointers, arrays, or function pointers).
/// For function declarators, this checks if the return type has indirection, not the
/// function itself. This ensures `void f()` is not considered to have indirection,
/// but `void* f()` and `void (*f())()` are.
fn declarator_has_indirection(declarator: Node) -> bool {
    match declarator.kind() {
        "pointer_declarator" | "array_declarator" => true,
        "function_declarator" => {
            // For function_declarator, check if the NESTED declarator has indirection
            // This distinguishes void f() (no indirection) from void (*f())() (has indirection)
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                declarator_has_indirection(nested)
            } else {
                // No nested declarator means this is just a function (e.g., identifier)
                false
            }
        }
        "parenthesized_declarator" => {
            // Check nested declarators
            let mut cursor = declarator.walk();
            for child in declarator.named_children(&mut cursor) {
                if declarator_has_indirection(child) {
                    return true;
                }
            }
            false
        }
        _ => {
            // Recurse into nested declarators
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                declarator_has_indirection(nested)
            } else {
                false
            }
        }
    }
}

/// Process function return type to create `TypeOf` and Reference edges.
fn process_function_returns(
    func_node: Node,
    func_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract return type from type specifiers
    let type_names = extract_type_specifiers_from_declaration(func_node, content);

    // Extract declarator to check for indirection
    let declarator = func_node.child_by_field_name("declarator");

    // Skip only pure void returns (no pointers/arrays/functions)
    // Allow void* and other complex void types like void (**)(...)
    let is_pure_void = type_names.len() == 1
        && type_names[0] == "void"
        && declarator.is_none_or(|d| !declarator_has_indirection(d));

    // Only create edges if we have type information and it's not pure void
    if !type_names.is_empty() && !is_pure_void {
        // Extract declarator for additional type info (referenced types)
        // Skip the top-level function's own parameter list to avoid duplication
        let mut all_types = type_names.clone();
        if let Some(decl) = declarator {
            all_types.extend(extract_return_type_references(decl, content));
        }

        create_return_edges(func_name, &type_names.join(" "), &all_types, helper);
    }
}

/// Extract type references from a return type declarator.
/// Skips the top-level function's own parameters to avoid adding parameter types as return references.
fn extract_return_type_references(declarator: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();

    match declarator.kind() {
        // For return types, skip the function's own parameters
        // Only recurse into nested declarators
        "pointer_declarator" | "array_declarator" | "function_declarator" => {
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                types.extend(extract_declarator_type_references(nested, content));
            }
        }
        "parenthesized_declarator" => {
            // Recurse into parenthesized content
            let mut cursor = declarator.walk();
            for child in declarator.named_children(&mut cursor) {
                types.extend(extract_declarator_type_references(child, content));
            }
        }
        _ => {}
    }

    types
}

/// Extract type references from a declarator (handles pointers, arrays, function pointers).
/// This version DOES include parameter types for function pointers.
fn extract_declarator_type_references(declarator: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();

    match declarator.kind() {
        "pointer_declarator" | "array_declarator" => {
            // Recurse into nested declarator
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                types.extend(extract_declarator_type_references(nested, content));
            }
        }
        "function_declarator" => {
            // Extract parameter types from function pointers
            if let Some(params) = declarator.child_by_field_name("parameters") {
                types.extend(extract_parameter_list_types(params, content));
            }
            // Recurse into nested declarator
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                types.extend(extract_declarator_type_references(nested, content));
            }
        }
        "parenthesized_declarator" => {
            // Recurse into parenthesized content
            let mut cursor = declarator.walk();
            for child in declarator.named_children(&mut cursor) {
                types.extend(extract_declarator_type_references(child, content));
            }
        }
        _ => {}
    }

    types
}

/// Extract all type names from a `parameter_list`.
/// Skips the f(void) pattern (single void parameter with no declarator).
fn extract_parameter_list_types(param_list: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = param_list.walk();

    // Collect all parameter declarations
    let param_decls: Vec<Node> = param_list
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "parameter_declaration")
        .collect();

    // Check for f(void) pattern - single void parameter with no declarator
    if param_decls.len() == 1 {
        let param = param_decls[0];
        let type_names = extract_type_specifiers_from_declaration(param, content);
        let has_declarator = param.child_by_field_name("declarator").is_some();

        if type_names.len() == 1 && type_names[0] == "void" && !has_declarator {
            // This is f(void) - no parameters, skip
            return types;
        }
    }

    // Process parameters normally
    for param in param_decls {
        // Extract type specifiers
        types.extend(extract_type_specifiers_from_declaration(param, content));

        // Extract declarator types
        if let Some(declarator) = param.child_by_field_name("declarator") {
            types.extend(extract_all_type_names_from_c_type(declarator, content));
        }
    }

    types
}

/// Create `TypeOf` and Reference edges for a function return type.
fn create_return_edges(
    func_name: &str,
    type_text: &str,
    referenced_types: &[String],
    helper: &mut GraphBuildHelper,
) {
    // Get or create function node
    let func_id = helper.add_function(func_name, None, false, false);

    // Create TypeOf edge: function → return type with Return context
    let type_id = helper.add_type(type_text, None);
    helper.add_typeof_edge_with_context(func_id, type_id, Some(TypeOfContext::Return), None, None);

    // Create Reference edges to all referenced types
    for ref_type in referenced_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }
}

/// Process variable declarations to create `TypeOf` and Reference edges.
fn process_variable_typeof_edges(decl_node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Extract type specifiers
    let type_names = extract_type_specifiers_from_declaration(decl_node, content);

    if type_names.is_empty() {
        return;
    }

    // Process all declarators in this declaration
    let mut cursor = decl_node.walk();
    for child in decl_node.named_children(&mut cursor) {
        if is_declarator_node(child.kind()) {
            process_single_variable_declarator(child, &type_names, content, helper);
        } else if child.kind() == "init_declarator" {
            // init_declarator contains a declarator and optional initializer
            if let Some(declarator) = child.child_by_field_name("declarator") {
                process_single_variable_declarator(declarator, &type_names, content, helper);
            }
        }
    }
}

/// Check if a node kind represents a declarator.
fn is_declarator_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "pointer_declarator"
            | "array_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
    )
}

/// Process a single variable declarator to create `TypeOf` and Reference edges.
fn process_single_variable_declarator(
    declarator: Node,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract variable name
    let Some(var_name) = extract_simple_declarator_name(declarator, content) else {
        return;
    };

    // Extract all type references from declarator (pointers, arrays, etc.)
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Create variable node
    let var_id = helper.add_variable(
        &var_name,
        Some(Span::from_bytes(
            declarator.start_byte(),
            declarator.end_byte(),
        )),
    );

    // Create TypeOf edge with Variable context
    let type_text = base_type_names.join(" ");
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        var_id,
        type_id,
        Some(TypeOfContext::Variable),
        None,
        Some(&var_name),
    );

    // Create Reference edges for all referenced types
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(var_id, ref_type_id);
    }
}

/// Process struct field declarations to create `TypeOf` and Reference edges.
fn process_struct_fields(
    struct_node: Node,
    struct_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Find field_declaration_list
    let Some(field_list) = struct_node.child_by_field_name("body") else {
        return;
    };

    let mut cursor = field_list.walk();
    for field_decl in field_list.named_children(&mut cursor) {
        if field_decl.kind() == "field_declaration" {
            process_single_struct_field(field_decl, struct_name, content, helper);
        }
    }
}

/// Process a single struct field declaration.
fn process_single_struct_field(
    field_decl: Node,
    struct_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract type specifiers
    let type_names = extract_type_specifiers_from_declaration(field_decl, content);

    if type_names.is_empty() {
        return;
    }

    // Process all field declarators
    // In C, field_declaration contains declarators of various kinds
    let mut cursor = field_decl.walk();
    for child in field_decl.named_children(&mut cursor) {
        // Field declarators can be: field_identifier, pointer_declarator, array_declarator, etc.
        if is_field_declarator_node(child.kind()) {
            process_single_field_declarator(child, struct_name, &type_names, content, helper);
        }
    }
}

/// Check if a node kind represents a field declarator
fn is_field_declarator_node(kind: &str) -> bool {
    matches!(
        kind,
        "field_identifier"
            | "pointer_declarator"
            | "array_declarator"
            | "function_declarator"
            | "field_declarator"
    )
}

/// Process a single field declarator.
///
/// Cluster C / `C_OTHER_PLUGINS` (`BadLiveware` Go-batch DAG, 2026-04-29):
/// every named C struct field is materialised as a `NodeKind::Property`
/// node, parented to the enclosing struct via `Defines` + `Contains`
/// edges. The qualified-name format is `<StructName>.<FieldName>`,
/// matching the bare-name convention the C plugin already uses for
/// struct/union nodes (the C plugin has no module concept), and aligned
/// in shape with Java/Kotlin/Dart/classpath/Go's
/// `<package>.<TypeName>.<FieldName>` cross-language norm.
///
/// The `TypeOf{Field}` edge's source is also migrated from the struct
/// node to the new Property node (mirroring Go's `C_EDGE_MIGRATE`
/// pattern). Aggregate "all fields of this struct" queries continue to
/// resolve via the `Defines` / `Contains` parenting; only the edge
/// source identity changes. The `TypeOfContext::Field` discriminator
/// and the `field_name` metadata are unchanged.
///
/// Visibility and `static` are conservative defaults per the C audit
/// (`docs/development/public-issue-triage/cluster_c_field_audit.md`):
/// C struct members have no public/private discipline (everything is
/// public-by-convention) and `static` at struct-member scope is not a
/// language concept, so we pass `is_static = false` and
/// `visibility = None`.
///
/// Fields whose declarator has no extractable name (e.g. anonymous
/// bit-field padding `int : 4;`) are deliberately skipped — there is
/// no stable qualified name we could synthesise that would be
/// resolvable from CLI / MCP / LSP queries. Reference edges to the
/// type tokens are also skipped in that case so we never emit an
/// orphan edge with no field-side anchor.
fn process_single_field_declarator(
    declarator: Node,
    struct_name: &str,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract field name + AST node so we can attach a line/column-aware
    // span to the new Property node.
    let Some((field_name, name_node)) =
        extract_field_declarator_name_with_node(declarator, content)
    else {
        return;
    };

    // Extract all type references
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Get or create the struct node. (`add_struct` is name-cached, so this
    // is the same id `handle_struct_specifier` already registered.)
    let struct_id = helper.add_struct(struct_name, None);

    // Emit the per-field Property node and parent it to the struct.
    let qualified_field_name = format!("{struct_name}.{field_name}");
    let property_id = helper.add_property_with_static_and_visibility(
        &qualified_field_name,
        Some(span_from_node(name_node)),
        false, // C struct members have no class-level `static`.
        None,  // C has no field-level visibility discipline.
    );
    helper.add_defines_edge(struct_id, property_id);
    helper.add_contains_edge(struct_id, property_id);

    // Create TypeOf edge with Field context, sourced at the new Property
    // node (post-migration shape — mirrors Go's C_EDGE_MIGRATE).
    let type_text = base_type_names.join(" ");
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        property_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(&field_name),
    );

    // Create Reference edges for all referenced types, also sourced at
    // the Property node so the field is the queryable anchor for "what
    // types does this field reference".
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(property_id, ref_type_id);
    }

    // C indirect-call precision (Phase A, U10): if this field is a
    // function pointer, compute its canonical signature via U07's
    // declarator-walking signature builder and stage it under
    // `(struct_tag, field_name)`. U11 interns the three legs and
    // inserts into `CIndirectSideTables::struct_field_fnptr`; U12 reads
    // the table to match callsite signatures against struct slots.
    if is_function_pointer_field_declarator(declarator) {
        // Phase 1 typedef chain is empty — typedef resolution lives in
        // U11's post-commit pass that builds the workspace-level chain
        // from `TypeOf` edges. Phase 1 callers pass an empty chain; U12
        // re-canonicalises against the workspace chain during
        // resolution.
        let typedef_chain = TypedefChain::new();
        // `build_function_signature` needs the **declaration-shaped**
        // parent (so `collect_base_tokens` finds the leading
        // `primitive_type` / `type_identifier`). The bare
        // `function_declarator` exposes only the parameter list, not
        // the return type. Walk up to the enclosing `field_declaration`
        // when available; fall back to passing the declarator directly
        // (the signature builder degrades to "unknown return type" but
        // still produces a non-empty parameter shape).
        let signature_anchor = declarator.parent().unwrap_or(declarator);
        if let Some(signature) = build_function_signature(signature_anchor, content, &typedef_chain)
        {
            helper.push_struct_field_fnptr_signature(struct_name, &field_name, &signature);
        }
    }
}

/// True when `declarator` is a function-pointer field declarator.
///
/// tree-sitter-c shapes `int (*op)(int, int)` as
/// `function_declarator { declarator: parenthesized_declarator {
/// declarator: pointer_declarator { declarator: field_identifier }},
/// parameters: parameter_list }`. The key distinguishing feature is the
/// `pointer_declarator` *inside* the `function_declarator`'s declarator
/// chain — a plain non-pointer function (e.g. an unusual struct-as-
/// method field, which C does not really have) lacks the pointer.
///
/// Plain pointers (`int *p`), arrays of fn-pointers (`int (*table[1])(int)`),
/// and non-pointer field declarators are rejected; U12's resolver
/// handles only the direct fn-pointer slot in Phase A.
fn is_function_pointer_field_declarator(declarator: Node<'_>) -> bool {
    if declarator.kind() != "function_declarator" {
        return false;
    }
    let Some(inner) = declarator.child_by_field_name("declarator") else {
        return false;
    };
    inner_contains_pointer_declarator(inner)
}

/// True when `node` (the inner-declarator chain of a `function_declarator`)
/// reaches a `pointer_declarator` before bottoming out at a
/// `field_identifier` / `identifier`.
fn inner_contains_pointer_declarator(node: Node<'_>) -> bool {
    match node.kind() {
        "pointer_declarator" => true,
        "parenthesized_declarator" | "field_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return inner_contains_pointer_declarator(inner);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if inner_contains_pointer_declarator(child) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Extract the field name **and** its identifier AST node from a
/// `field_declarator`.
///
/// Returns the `field_identifier` `Node` alongside the extracted name so
/// the caller can attach a line/column-aware `Span` to the new Property
/// node (Cluster C / `C_OTHER_PLUGINS`). Anonymous bit-field padding
/// (e.g. `int : 4;`) and other declarators that contain no
/// `field_identifier` return `None`, which the caller treats as
/// "skip Property emission for this field".
fn extract_field_declarator_name_with_node<'tree>(
    declarator: Node<'tree>,
    content: &[u8],
) -> Option<(String, Node<'tree>)> {
    match declarator.kind() {
        "field_identifier" => {
            let name = declarator.utf8_text(content).ok()?.to_string();
            Some((name, declarator))
        }
        "field_declarator" | "function_declarator" => {
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                return extract_field_declarator_name_with_node(nested, content);
            }
            extract_simple_declarator_name_with_node(declarator, content)
        }
        _ => {
            // Check for direct field_identifier child
            for i in 0..declarator.child_count() {
                #[allow(clippy::cast_possible_truncation)]
                // Graph storage: node/edge index counts fit in u32
                if let Some(child) = declarator.child(i as u32)
                    && child.kind() == "field_identifier"
                {
                    let name = child.utf8_text(content).ok()?.to_string();
                    return Some((name, child));
                }
            }
            extract_simple_declarator_name_with_node(declarator, content)
        }
    }
}

/// Sibling of `extract_simple_declarator_name` that also returns the AST
/// node where the identifier was found (for Span construction).
fn extract_simple_declarator_name_with_node<'tree>(
    declarator: Node<'tree>,
    content: &[u8],
) -> Option<(String, Node<'tree>)> {
    match declarator.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            let name = declarator.utf8_text(content).ok()?.to_string();
            Some((name, declarator))
        }
        "pointer_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator" => {
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                extract_simple_declarator_name_with_node(nested, content)
            } else {
                let mut cursor = declarator.walk();
                for child in declarator.children(&mut cursor) {
                    if let Some(found) = extract_simple_declarator_name_with_node(child, content) {
                        return Some(found);
                    }
                }
                None
            }
        }
        _ => None,
    }
}

/// Process union fields (same as struct fields).
fn process_union_fields(
    union_node: Node,
    union_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Unions use the same structure as structs
    // Find field_declaration_list
    let Some(field_list) = union_node.child_by_field_name("body") else {
        return;
    };

    let mut cursor = field_list.walk();
    for field_decl in field_list.named_children(&mut cursor) {
        if field_decl.kind() == "field_declaration" {
            process_single_union_field(field_decl, union_name, content, helper);
        }
    }
}

/// Process a single union field declaration.
fn process_single_union_field(
    field_decl: Node,
    union_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract type specifiers
    let type_names = extract_type_specifiers_from_declaration(field_decl, content);

    if type_names.is_empty() {
        return;
    }

    // Process all field declarators
    let mut cursor = field_decl.walk();
    for child in field_decl.named_children(&mut cursor) {
        if is_field_declarator_node(child.kind()) {
            process_single_union_field_declarator(child, union_name, &type_names, content, helper);
        }
    }
}

/// Process a single union field declarator.
///
/// Unions emit Property nodes per member with the same qualified-name
/// shape as struct fields: `<UnionName>.<MemberName>`. Each `Property`
/// is parented to the union node via `Defines` + `Contains`, and the
/// `TypeOf{Field}` edge is sourced at the Property (mirroring the
/// struct-field migration above). See `process_single_field_declarator`
/// for the full Cluster C / `C_OTHER_PLUGINS` rationale.
fn process_single_union_field_declarator(
    declarator: Node,
    union_name: &str,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract field name + AST node (for line/column-aware span).
    let Some((field_name, name_node)) =
        extract_field_declarator_name_with_node(declarator, content)
    else {
        return;
    };

    // Extract all type references
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Create union node if needed (use add_struct for now, unions are similar)
    let union_id = helper.add_struct(union_name, None);

    // Emit the per-member Property node.
    let qualified_field_name = format!("{union_name}.{field_name}");
    let property_id = helper.add_property_with_static_and_visibility(
        &qualified_field_name,
        Some(span_from_node(name_node)),
        false,
        None,
    );
    helper.add_defines_edge(union_id, property_id);
    helper.add_contains_edge(union_id, property_id);

    // Create TypeOf edge with Field context, sourced at the Property.
    let type_text = base_type_names.join(" ");
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        property_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(&field_name),
    );

    // Create Reference edges for all referenced types, sourced at the
    // Property so the union member is the queryable anchor.
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(property_id, ref_type_id);
    }
}

/// Process typedef declarations to create `TypeOf` edges.
fn process_typedef_edges(typedef_node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Extract the underlying type specifiers
    let type_names = extract_type_specifiers_from_declaration(typedef_node, content);

    if type_names.is_empty() {
        return;
    }

    // Process the declarator field (most common case: typedef int MyInt;)
    if let Some(declarator) = typedef_node.child_by_field_name("declarator") {
        process_single_typedef_declarator(declarator, &type_names, content, helper);
    }

    // Also check for additional declarators that might be children (e.g., typedef int *P, Q;)
    // Note: This handles the case where the tree-sitter grammar supports multiple declarators
    let mut cursor = typedef_node.walk();
    for child in typedef_node.named_children(&mut cursor) {
        // Skip the main declarator field (already processed above)
        if child.kind() == "type_definition" {
            continue;
        }
        if is_declarator_node(child.kind())
            && (typedef_node.child_by_field_name("declarator") != Some(child))
        {
            process_single_typedef_declarator(child, &type_names, content, helper);
        }
    }
}

/// Process a single typedef declarator to create `TypeOf` and Reference edges.
fn process_single_typedef_declarator(
    declarator: Node,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract typedef alias name
    let Some(typedef_name) = extract_simple_declarator_name(declarator, content) else {
        return;
    };

    // Extract all referenced types from the declarator
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Create type node for the typedef alias
    let typedef_id = helper.add_type(&typedef_name, None);

    // Create TypeOf edge from typedef to underlying type
    let underlying_type_text = base_type_names.join(" ");
    let underlying_type_id = helper.add_type(&underlying_type_text, None);
    helper.add_typeof_edge_with_context(
        typedef_id,
        underlying_type_id,
        Some(TypeOfContext::Variable), // Typedef acts like a type alias
        None,
        Some(&typedef_name),
    );

    // Create Reference edges for all referenced types
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(typedef_id, ref_type_id);
    }
}

// =============================================================================
// C indirect-call precision (Phase A, U10) — address-taken classifier walkers.
// =============================================================================

/// Collect every function name *defined* or *declared* in this translation
/// unit into a `HashSet` for the address-taken classifier (DESIGN §2.6
/// footnote).
///
/// Walks `function_definition` nodes (function bodies) and `declaration`
/// nodes whose declarator chain bottoms out at a `function_declarator`
/// (function prototypes). The set is used as a fast predicate by
/// `classify_address_taken_sites` so that only identifiers that actually
/// refer to functions trigger an `ADDRESS_TAKEN` mark — `&g_int` (where
/// `g_int` is a plain variable) is correctly skipped (DESIGN §2.5
/// `nonfunction_taken` negative case).
/// Single combined Phase-1 pre-pass (PERF-280).
///
/// Walks the AST **once** and populates both:
///
/// * `known_fn_names` — every C function name in the file (definitions +
///   prototype declarations), the predicate used by
///   `classify_address_taken_sites`; and
/// * [`TypePermits`] — the function-pointer destination maps (DESIGN §2.6),
///   also consumed by `walk_tree_for_graph`.
///
/// These were historically two independent full-tree recursive walks
/// (`collect_known_function_names` + `build_type_permits`). Profiling of
/// `bench_full_build_linux_fs_subset` (verivus-oss/sqry#280) attributed
/// ~0.46 ms of per-build cost to the known-fn-names walk alone; fusing the
/// two removes one full traversal. The per-node dispatch is identical to
/// the two prior walks (same node kinds → same helper calls, same
/// source-order recursion), so the outputs are unchanged.
fn collect_known_fns_and_type_permits(
    root: Node<'_>,
    content: &[u8],
) -> (HashSet<String>, TypePermits) {
    let mut names: HashSet<String> = HashSet::new();
    let mut permits = TypePermits::default();
    collect_known_fns_and_type_permits_recursive(root, content, &mut names, &mut permits);
    (names, permits)
}

fn collect_known_fns_and_type_permits_recursive(
    node: Node<'_>,
    content: &[u8],
    names: &mut HashSet<String>,
    permits: &mut TypePermits,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = extract_function_name_from_definition(node, content) {
                names.insert(name);
            }
        }
        "struct_specifier" => {
            // Only struct definitions with a body and a named tag
            // contribute. `struct Tag {...}` populates the struct-field
            // maps; bare `struct Tag` references (no body) and anonymous
            // structs are skipped.
            if let Some(name_node) = node.child_by_field_name("name")
                && let Some(body) = node.child_by_field_name("body")
                && let Ok(tag) = name_node.utf8_text(content)
            {
                let tag = tag.trim().to_string();
                if !tag.is_empty() {
                    record_struct_fields(body, &tag, content, permits);
                }
            }
        }
        "declaration" => {
            // Known-fn-names leg: prototype shape `T name(args);` — the
            // declarator chain bottoms out at function_declarator.
            // `T (*name)(args)` (function-pointer variable) also has a
            // function_declarator at some depth but is gated by a
            // `pointer_declarator` parent — those are NOT function
            // declarations and must not enter the known-fn set.
            for proto_name in extract_function_prototype_names(node, content) {
                names.insert(proto_name);
            }
            // Type-permits leg: struct-typed receivers + fnptr arrays.
            record_declaration(node, content, permits);
        }
        "parameter_declaration" => {
            // Function parameters can carry struct-typed receivers
            // (e.g. `void use_(struct S* s)`) and fnptr arrays. Record
            // them in the same maps so `s->cb = fn` inside the function
            // body can resolve through the var → struct-tag lookup.
            record_parameter(node, content, permits);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_known_fns_and_type_permits_recursive(child, content, names, permits);
    }
}

/// Extract the function name from a `function_definition` node.
fn extract_function_name_from_definition(node: Node<'_>, content: &[u8]) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    extract_name_from_function_declarator(declarator, content)
}

/// Extract every function-prototype name from a `declaration` node.
///
/// Walks the declaration's declarator children. A declarator that is
/// directly a `function_declarator` (or wrapped only in
/// `parenthesized_declarator` chains without an intervening
/// `pointer_declarator`) names a function. A
/// `pointer_declarator > function_declarator` names a function-pointer
/// variable, NOT a function — those are excluded so they don't enter
/// the known-fn set.
fn extract_function_prototype_names(decl_node: Node<'_>, content: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() == "function_declarator"
            && let Some(name) = extract_name_from_function_declarator(child, content)
        {
            out.push(name);
        }
    }
    out
}

/// Walk a declarator chain to find the bottom `function_declarator` and
/// extract its identifier name. Skips through `parenthesized_declarator`
/// wrappers; treats a `pointer_declarator` wrapper as "this is a
/// function-pointer variable, not a function definition", returning
/// `None`.
fn extract_name_from_function_declarator(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(content).ok().map(str::to_string),
        "function_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            extract_name_from_function_declarator(inner, content)
        }
        "parenthesized_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_name_from_function_declarator(inner, content);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(name) = extract_name_from_function_declarator(child, content) {
                    return Some(name);
                }
            }
            None
        }
        _ => None,
    }
}

fn mark_identifier_children_address_taken(
    node: Node<'_>,
    content: &[u8],
    known_fn_names: &HashSet<String>,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(name) = child.utf8_text(content)
            && known_fn_names.contains(name)
        {
            helper.mark_function_address_taken_by_name(name);
        }
    }
}

/// Recursive walker covering the DESIGN §2.5 address-taken pattern table.
///
/// For every identifier that appears in an address-taken context AND whose
/// text is in `known_fn_names`, calls
/// `helper.mark_function_address_taken_by_name(name)`. The walker is
/// additive — the existing extractor functions
/// (`extract_designated_initializer_targets`, etc.) still drive binding-
/// plane capture independently. Duplicate marks within a file are
/// tolerated (U11's `mark_address_taken` is idempotent on the
/// `NodeFlags::ADDRESS_TAKEN` bit).
///
/// Patterns covered (one match arm each):
///
/// 1. `unary_expression { operator: '&', argument: identifier }`
///    — `&function_name`. **No type guard** (DESIGN §2.6 row 1) —
///    the syntactic shape `&fn` is sufficient on its own.
/// 2. `argument_list > identifier` — function passed as a call argument.
///    Note: the callee position is `call_expression.function`, NOT an
///    `argument_list` child, so this arm cannot mis-fire on direct calls.
///    **No type guard** (DESIGN §2.6 row 2).
/// 3. `initializer_pair { value: identifier }` — designated initializer
///    RHS. **No type guard** (DESIGN §2.6 row 3): the field name itself
///    selects the slot, and the struct's signature table (captured during
///    struct-field processing) already gates whether the slot is fnptr
///    at the resolution stage. Phase 1 marks unconditionally.
/// 4. `initializer_list > identifier` — positional initializer slot.
///    Filters out direct `initializer_pair` children (those are matched
///    by arm 3) and bare wrapping `parenthesized_expression`. **Type
///    guard** (DESIGN §2.6 row 4): only marks when the declared field
///    type at this slot position is a function pointer.
/// 5. `assignment_expression { left: field_expression | subscript_expression, right: identifier }`
///    — function-pointer slot assignment via field access or array
///    subscript. **Type guard** (DESIGN §2.6 rows 5–6): only marks when
///    the LHS field's declared type (or the LHS array's element type)
///    is a function pointer.
/// 6. `return_statement > identifier` — returning a function name.
///    **No type guard** (DESIGN §2.6 row 7).
/// 7. `init_declarator { value: identifier }` — initializer-declarator
///    RHS (`void (*p)() = function_name;`). **Type guard** (DESIGN §2.6
///    row 8): only marks when the declarator's type is a function
///    pointer (i.e. the declarator chain contains
///    `pointer_declarator > function_declarator`).
fn classify_address_taken_sites(
    root: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    known_fn_names: &HashSet<String>,
    type_permits: &TypePermits,
) {
    classify_address_taken_recursive(root, content, helper, known_fn_names, type_permits);
}

fn classify_address_taken_recursive(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    known_fn_names: &HashSet<String>,
    type_permits: &TypePermits,
) {
    match node.kind() {
        // Pattern 1: `&function_name`. No type guard.
        "pointer_expression" | "unary_expression" => {
            // tree-sitter-c uses `pointer_expression` for `&x` and `*x`;
            // some grammar versions also surface `unary_expression`. The
            // operator is the first unnamed child (or accessible via
            // `child(0)`). For `&`, the argument is the field `argument`
            // or the first named child.
            if let Some(op) = node.child(0)
                && op.utf8_text(content).map(str::trim).unwrap_or("") == "&"
            {
                let target = node
                    .child_by_field_name("argument")
                    .or_else(|| node.named_child(0));
                if let Some(arg) = target
                    && arg.kind() == "identifier"
                    && let Ok(name) = arg.utf8_text(content)
                    && known_fn_names.contains(name)
                {
                    helper.mark_function_address_taken_by_name(name);
                }
            }
        }
        // Pattern 2: identifier in argument list. No type guard — passing
        // a function name in a call argument is itself an address-take
        // (the function decays to a pointer at the call site).
        //
        // Pattern 6: `return identifier`. No type guard — returning a
        // bare function name from any function-returning-fnptr context
        // is itself an address-take.
        "argument_list" | "return_statement" => {
            mark_identifier_children_address_taken(node, content, known_fn_names, helper);
        }
        // Pattern 3: designated initializer RHS. No type guard — the
        // `.field = fn` shape is itself a fnptr-slot designator.
        "initializer_pair" => {
            #[allow(clippy::cast_possible_truncation)]
            let child_count = node.named_child_count() as u32;
            if child_count >= 2
                && let Some(value) = node.named_child(child_count - 1)
                && value.kind() == "identifier"
                && let Ok(name) = value.utf8_text(content)
                && known_fn_names.contains(name)
            {
                helper.mark_function_address_taken_by_name(name);
            }
        }
        // Pattern 4: positional initializer slot (bare identifier child of
        // initializer_list, not an `initializer_pair`).
        //
        // **Type-guarded** (DESIGN §2.6 row 4): per spec, we must verify
        // that the slot's declared field type is a function pointer.
        // We walk the AST up to the enclosing `init_declarator` →
        // `declaration` chain to find the declared variable and its
        // struct type, then index into `type_permits.struct_field_fnptr`
        // by `(struct_tag, field_index)`.
        "initializer_list" => {
            let mut cursor = node.walk();
            // We need a positional index across **named** children that
            // are bare identifiers in this initializer_list. We can't
            // simply use the AST child index because designated pairs
            // and bare positional identifiers interleave — but in the
            // positional-only shape (which is the only shape where this
            // pattern's guard actually applies), the children are
            // ordered identifiers / expressions.
            let mut positional_index: usize = 0;
            for child in node.named_children(&mut cursor) {
                if child.kind() == "initializer_pair" {
                    // Designated entry — does NOT participate in the
                    // positional index. Pattern 3 handles it.
                    continue;
                }
                let is_identifier = child.kind() == "identifier";
                if is_identifier
                    && let Ok(name) = child.utf8_text(content)
                    && known_fn_names.contains(name)
                    && positional_init_slot_is_fnptr(node, positional_index, content, type_permits)
                {
                    helper.mark_function_address_taken_by_name(name);
                }
                positional_index += 1;
            }
        }
        // Pattern 5: `field_expression = identifier` or
        // `subscript_expression = identifier`.
        //
        // **Type-guarded** (DESIGN §2.6 rows 5–6):
        // * `field_expression`: resolve the receiver back to its struct
        //   tag via `type_permits.var_struct_tag`, then look up
        //   `(struct_tag, field_name)` in `type_permits.struct_field_fnptr`.
        // * `subscript_expression`: look up the array's identifier in
        //   `type_permits.var_fnptr_array`.
        "assignment_expression" => {
            let lhs = node.child_by_field_name("left");
            let rhs = node.child_by_field_name("right");
            if let (Some(lhs), Some(rhs)) = (lhs, rhs)
                && rhs.kind() == "identifier"
                && let Ok(name) = rhs.utf8_text(content)
                && known_fn_names.contains(name)
                && lhs_assignment_target_is_fnptr(lhs, content, type_permits)
            {
                helper.mark_function_address_taken_by_name(name);
            }
        }
        // Pattern 7: `init_declarator { value: identifier }`.
        //
        // **Type-guarded** (DESIGN §2.6 row 8): the declarator's type
        // must contain a function-pointer shape, i.e. the declarator
        // chain (left side of `init_declarator`) must reach a
        // `pointer_declarator > function_declarator`.
        "init_declarator" => {
            if let Some(value) = node.child_by_field_name("value")
                && value.kind() == "identifier"
                && let Ok(name) = value.utf8_text(content)
                && known_fn_names.contains(name)
                && init_declarator_is_fnptr(node)
            {
                helper.mark_function_address_taken_by_name(name);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        classify_address_taken_recursive(child, content, helper, known_fn_names, type_permits);
    }
}

// ---------------------------------------------------------------------------
// DESIGN §2.6 — type-permits pre-pass
// ---------------------------------------------------------------------------

/// Function-pointer destination table built once per file (DESIGN §2.6).
///
/// Three lookup maps populated by `collect_known_fns_and_type_permits`
/// in the combined Phase-1 pre-pass over the AST. Consulted by
/// `classify_address_taken_recursive` to gate the four type-guarded rows
/// (positional initializer, field-expression assignment,
/// subscript-expression assignment, init-declarator RHS).
///
/// The pre-pass is intentionally cheap: it only inspects declaration /
/// struct-declaration shapes (a small subset of nodes in a typical C
/// translation unit) and the maps are populated only for declarators
/// the C type system would actually allow to receive a function pointer.
#[derive(Debug, Default)]
struct TypePermits {
    /// `(struct_tag, field_name) → is_fnptr`. Captures the
    /// function-pointer-ness of every named struct field across the
    /// translation unit. Used to gate positional initializer (slot-by-
    /// position via `struct_field_order`) and `s->f = fn` /
    /// `s.f = fn` assignments.
    struct_field_fnptr: HashMap<(String, String), bool>,
    /// `struct_tag → ordered list of (field_name, is_fnptr)`. Mirrors
    /// `struct_field_fnptr` but preserves field order so positional
    /// initializers can index by slot position.
    struct_field_order: HashMap<String, Vec<(String, bool)>>,
    /// `var_name → struct_tag`. Set for variables whose declared type is
    /// `struct <Tag>` with a non-empty tag. Drives the receiver
    /// resolution for `s->cb = fn` and the positional initializer's
    /// enclosing-declaration lookup.
    var_struct_tag: HashMap<String, String>,
    /// `var_name → true` when the declared type is a function-pointer
    /// array (e.g. `void (*table[N])();`). Drives the subscript
    /// assignment guard.
    var_fnptr_array: HashMap<String, bool>,
}

/// Record every field of one `struct Tag { … }` body in both
/// `struct_field_fnptr` (keyed) and `struct_field_order` (ordered).
fn record_struct_fields(
    body: Node<'_>,
    struct_tag: &str,
    content: &[u8],
    permits: &mut TypePermits,
) {
    let mut order = Vec::new();
    let mut cursor = body.walk();
    for field_decl in body.named_children(&mut cursor) {
        if field_decl.kind() != "field_declaration" {
            continue;
        }
        // A `field_declaration` can have multiple declarators
        // (e.g. `int (*f)(int), x;`). Walk every declarator child.
        let mut inner = field_decl.walk();
        for child in field_decl.named_children(&mut inner) {
            if !is_field_declarator_node(child.kind()) {
                continue;
            }
            let Some((field_name, _)) = extract_field_declarator_name_with_node(child, content)
            else {
                continue;
            };
            let is_fnptr = declarator_is_function_pointer_shape(child);
            permits
                .struct_field_fnptr
                .insert((struct_tag.to_string(), field_name.clone()), is_fnptr);
            order.push((field_name, is_fnptr));
        }
    }
    if !order.is_empty() {
        permits
            .struct_field_order
            .insert(struct_tag.to_string(), order);
    }
}

/// Record per-declaration variable info: struct-type tags + fnptr
/// arrays. Scalar function pointers like `void (*p)()` are reflected via
/// `init_declarator_is_fnptr` (consulted directly at the call site), so
/// we don't need a separate `var_fnptr` map here.
fn record_declaration(decl: Node<'_>, content: &[u8], permits: &mut TypePermits) {
    let type_node = decl.child_by_field_name("type");
    let struct_tag = type_node.and_then(|t| {
        if t.kind() == "struct_specifier" {
            t.child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        }
    });

    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        match child.kind() {
            "init_declarator" => {
                if let Some((name, _)) = extract_declarator_name(child, content) {
                    if let Some(tag) = &struct_tag {
                        permits.var_struct_tag.insert(name.clone(), tag.clone());
                    }
                    if init_declarator_is_fnptr_array(child) {
                        permits.var_fnptr_array.insert(name, true);
                    }
                }
            }
            "declarator"
            | "array_declarator"
            | "pointer_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
            | "identifier" => {
                if let Some((name, _)) = extract_declarator_name(child, content) {
                    if let Some(tag) = &struct_tag {
                        permits.var_struct_tag.insert(name.clone(), tag.clone());
                    }
                    if bare_declarator_is_fnptr_array(child) {
                        permits.var_fnptr_array.insert(name, true);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Record a `parameter_declaration` (function parameter) into the
/// type-permits maps.
///
/// Captures two shapes:
/// * `struct Tag *p` (or `struct Tag p`) → records `p` in
///   `var_struct_tag`, so `p->field = fn` inside the function body
///   can resolve the field's declared type.
/// * `void (*p[N])()` → records `p` in `var_fnptr_array` so
///   `p[i] = fn` is permitted.
fn record_parameter(param: Node<'_>, content: &[u8], permits: &mut TypePermits) {
    let type_node = param.child_by_field_name("type");
    let struct_tag = type_node.and_then(|t| {
        if t.kind() == "struct_specifier" {
            t.child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        }
    });

    let Some(decl) = param.child_by_field_name("declarator") else {
        return;
    };
    let Some((name, _)) = extract_declarator_name(decl, content) else {
        return;
    };
    if let Some(tag) = struct_tag {
        permits.var_struct_tag.insert(name.clone(), tag);
    }
    if bare_declarator_is_fnptr_array(decl) {
        permits.var_fnptr_array.insert(name, true);
    }
}

/// True when a struct-field declarator (`field_identifier`, possibly
/// wrapped in `pointer_declarator`/`function_declarator`/etc.) is a
/// function-pointer shape (`pointer_declarator > function_declarator`,
/// in any wrapping order).
///
/// Reuses the existing `is_function_pointer_field_declarator` shape
/// check but additionally peels through `pointer_declarator` /
/// `parenthesized_declarator` wrappers so a declarator like
/// `(*name)(args)` (where the outer node is `pointer_declarator`, not
/// `function_declarator`) still classifies as fnptr.
fn declarator_is_function_pointer_shape(decl: Node<'_>) -> bool {
    if is_function_pointer_field_declarator(decl) {
        return true;
    }
    match decl.kind() {
        "pointer_declarator" | "parenthesized_declarator" | "field_declarator" => {
            if let Some(inner) = decl.child_by_field_name("declarator")
                && declarator_is_function_pointer_shape(inner)
            {
                return true;
            }
            let mut cursor = decl.walk();
            for child in decl.named_children(&mut cursor) {
                if declarator_is_function_pointer_shape(child) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// True when an `init_declarator`'s declarator chain reaches a
/// `pointer_declarator > function_declarator` shape — i.e. the variable
/// being declared is a scalar function pointer (`void (*p)() = …;`).
fn init_declarator_is_fnptr(init: Node<'_>) -> bool {
    let Some(decl) = init.child_by_field_name("declarator") else {
        return false;
    };
    declarator_is_function_pointer_shape(decl)
}

/// True when an `init_declarator`'s declarator chain reaches a
/// `pointer_declarator > array_declarator > function_declarator` shape
/// — i.e. the variable is a function-pointer array (`void (*t[N])()`).
fn init_declarator_is_fnptr_array(init: Node<'_>) -> bool {
    let Some(decl) = init.child_by_field_name("declarator") else {
        return false;
    };
    bare_declarator_is_fnptr_array(decl)
}

/// True for `function_declarator > parenthesized_declarator >
/// pointer_declarator > array_declarator > identifier` shapes (the
/// tree-sitter-c shape of `void (*table[N])(args)`) — i.e. arrays of
/// function pointers, where assignment `table[i] = fn` is a legitimate
/// fnptr-slot assignment.
fn bare_declarator_is_fnptr_array(decl: Node<'_>) -> bool {
    fn has_array(node: Node<'_>) -> bool {
        match node.kind() {
            "array_declarator" => true,
            "parenthesized_declarator"
            | "pointer_declarator"
            | "field_declarator"
            | "function_declarator" => {
                if let Some(inner) = node.child_by_field_name("declarator") {
                    return has_array(inner);
                }
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if has_array(child) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
    declarator_is_function_pointer_shape(decl) && has_array(decl)
}

/// True when the slot at `slot_index` (0-based) of `init_list`'s
/// positional children — i.e. the enclosing declaration's struct type's
/// `slot_index`-th field — is a function pointer.
///
/// Walks up from `init_list` to the enclosing `init_declarator` to find
/// the declared variable, then looks up its struct tag in
/// `type_permits.var_struct_tag` and indexes into
/// `type_permits.struct_field_order`.
///
/// Returns `false` (conservative) when:
/// * there is no enclosing `init_declarator` (e.g. nested initializer
///   lists, compound literals);
/// * the declared variable's type tag is not in `var_struct_tag`;
/// * the slot index is out of range for the struct's field order;
/// * the struct tag has no recorded field order (anonymous struct).
fn positional_init_slot_is_fnptr(
    init_list: Node<'_>,
    slot_index: usize,
    content: &[u8],
    type_permits: &TypePermits,
) -> bool {
    let mut node = init_list;
    while let Some(parent) = node.parent() {
        if parent.kind() == "init_declarator" {
            // Find the declared variable name by walking the declarator
            // chain of this `init_declarator`.
            let Some(decl) = parent.child_by_field_name("declarator") else {
                return false;
            };
            let Some((var_name, _)) = extract_declarator_name(decl, content) else {
                return false;
            };
            let Some(tag) = type_permits.var_struct_tag.get(&var_name) else {
                return false;
            };
            let Some(fields) = type_permits.struct_field_order.get(tag) else {
                return false;
            };
            return fields
                .get(slot_index)
                .is_some_and(|(_, is_fnptr)| *is_fnptr);
        }
        // Stop walking up if we leave the declaration entirely (e.g. we
        // hit a `function_definition` or `translation_unit`) — the
        // initializer_list isn't part of a top-level variable
        // declaration, so we can't apply the field-type guard.
        if matches!(
            parent.kind(),
            "function_definition" | "translation_unit" | "compound_statement"
        ) {
            return false;
        }
        node = parent;
    }
    false
}

/// True when assignment LHS (a `field_expression` or
/// `subscript_expression`) resolves to a function-pointer destination
/// (DESIGN §2.6 rows 5–6).
///
/// * `field_expression`: resolves the receiver back to its struct tag
///   via `type_permits.var_struct_tag`, then looks up
///   `(struct_tag, field_name)` in `type_permits.struct_field_fnptr`.
///   Returns `true` only when the lookup yields `Some(true)`.
/// * `subscript_expression`: walks the argument back to a bare
///   identifier and consults `type_permits.var_fnptr_array`.
///   Conservative — gives up (returns `false`) for nested or
///   computed-index shapes.
fn lhs_assignment_target_is_fnptr(
    lhs: Node<'_>,
    content: &[u8],
    type_permits: &TypePermits,
) -> bool {
    match lhs.kind() {
        "field_expression" => {
            let field_name = lhs
                .child_by_field_name("field")
                .and_then(|n| n.utf8_text(content).ok())
                .map(str::to_string);
            let receiver = lhs.child_by_field_name("argument");
            let receiver_name = receiver.and_then(|n| {
                if n.kind() == "identifier" {
                    n.utf8_text(content).ok().map(str::to_string)
                } else {
                    None
                }
            });
            if let (Some(field), Some(recv)) = (field_name, receiver_name)
                && let Some(tag) = type_permits.var_struct_tag.get(&recv)
                && let Some(&is_fnptr) = type_permits.struct_field_fnptr.get(&(tag.clone(), field))
            {
                return is_fnptr;
            }
            false
        }
        "subscript_expression" => {
            let arg = lhs.child_by_field_name("argument");
            if let Some(arg) = arg
                && arg.kind() == "identifier"
                && let Ok(name) = arg.utf8_text(content)
            {
                return type_permits
                    .var_fnptr_array
                    .get(name)
                    .copied()
                    .unwrap_or(false);
            }
            false
        }
        _ => false,
    }
}
