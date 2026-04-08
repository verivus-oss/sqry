use std::collections::{HashMap, HashSet};
use std::path::Path;

use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::{FfiConvention, GraphBuildHelper, GraphSnapshot, StagingGraph};
use sqry_core::graph::{CodeEdge, GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

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
) -> GraphResult<()> {
    match node.kind() {
        "function_definition" => {
            handle_function_node(node, content, ast_graph, helper, exported_symbols);
        }
        "declaration" => {
            handle_declaration(node, content, ast_graph, helper, exported_symbols);
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
) {
    if has_extern_storage_class(node, content) {
        build_ffi_declaration_for_staging(node, content, helper, exported_symbols);
        return;
    }

    if is_function_declaration(node) {
        handle_function_node(node, content, ast_graph, helper, exported_symbols);
        return;
    }

    handle_variable_declaration(node, content, helper, exported_symbols);

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

    let caller_function_id = helper.ensure_function(&caller_qualified, None, false, false);

    if let Some((ffi_qualified, ffi_convention)) = ffi_registry.get(&target_qualified) {
        let ffi_target_id = helper.ensure_function(ffi_qualified, None, false, true);
        helper.add_ffi_edge(caller_function_id, ffi_target_id, *ffi_convention);
        return;
    }

    let target_function_id = helper.ensure_function(&target_qualified, None, false, false);
    let argument_count = u8::try_from(argument_count).unwrap_or(u8::MAX);
    helper.add_call_edge_full_with_span(
        caller_function_id,
        target_function_id,
        argument_count,
        false,
        vec![span],
    );
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
        extract_designated_initializer_targets(node, content, &name, var_id, helper);
    }
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

            extract_initializer_list_targets(init_child, content, var_id, helper);
        }

        // Found the matching declarator — no need to continue
        break;
    }
}

/// Extract function pointer targets from an `initializer_list` node.
fn extract_initializer_list_targets(
    list_node: Node,
    content: &[u8],
    var_id: sqry_core::graph::unified::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut pair_cursor = list_node.walk();
    for pair in list_node.children(&mut pair_cursor) {
        if pair.kind() != "initializer_pair" {
            continue;
        }

        // The value is typically the last named child (after the designator)
        // tree-sitter C: initializer_pair has designator(s) then value
        #[allow(clippy::cast_possible_truncation)]
        // Graph storage: node/edge index counts fit in u32
        let child_count = pair.named_child_count() as u32;
        if child_count < 2 {
            continue;
        }

        let Some(value_node) = pair.named_child(child_count - 1) else {
            continue;
        };

        // Only process identifier values (function name references)
        // Skip numeric literals, string literals, nested initializers, etc.
        if value_node.kind() != "identifier" {
            continue;
        }

        let Ok(func_name) = value_node.utf8_text(content) else {
            continue;
        };

        // Skip common non-function values (NULL, 0, macros that look like constants)
        if func_name == "NULL"
            || func_name == "nullptr"
            || func_name.chars().all(|c| c.is_uppercase() || c == '_')
        {
            continue;
        }

        let target_id = helper.ensure_function(func_name, None, false, false);
        helper.add_reference_edge(var_id, target_id);
    }
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
fn process_single_field_declarator(
    declarator: Node,
    struct_name: &str,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract field name
    let Some(field_name) = extract_field_declarator_name(declarator, content) else {
        return;
    };

    // Extract all type references
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Create struct node if needed
    let struct_id = helper.add_struct(struct_name, None);

    // Create TypeOf edge with Field context
    let type_text = base_type_names.join(" ");
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        struct_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(&field_name),
    );

    // Create Reference edges for all referenced types
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(struct_id, ref_type_id);
    }
}

/// Extract field name from a `field_declarator`.
fn extract_field_declarator_name(declarator: Node, content: &[u8]) -> Option<String> {
    // field_declarator can be:
    // - field_identifier (simple field)
    // - pointer_declarator, array_declarator, function_declarator (complex)

    match declarator.kind() {
        "field_identifier" => {
            // Direct field identifier
            declarator.utf8_text(content).ok().map(String::from)
        }
        "field_declarator" => {
            // Descend into field_declarator wrapper
            if let Some(nested) = declarator.child_by_field_name("declarator") {
                return extract_field_declarator_name(nested, content);
            }
            // Fallback to simple extraction
            extract_simple_declarator_name(declarator, content)
        }
        "function_declarator" => {
            if let Some(decl) = declarator.child_by_field_name("declarator") {
                return extract_field_declarator_name(decl, content);
            }
            extract_simple_declarator_name(declarator, content)
        }
        _ => {
            // Check for direct field_identifier child
            for i in 0..declarator.child_count() {
                #[allow(clippy::cast_possible_truncation)]
                // Graph storage: node/edge index counts fit in u32
                if let Some(child) = declarator.child(i as u32)
                    && child.kind() == "field_identifier"
                {
                    return child.utf8_text(content).ok().map(String::from);
                }
            }

            // Recurse for nested declarators
            extract_simple_declarator_name(declarator, content)
        }
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
fn process_single_union_field_declarator(
    declarator: Node,
    union_name: &str,
    base_type_names: &[String],
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    // Extract field name
    let Some(field_name) = extract_field_declarator_name(declarator, content) else {
        return;
    };

    // Extract all type references
    let mut all_types = base_type_names.to_vec();
    all_types.extend(extract_all_type_names_from_c_type(declarator, content));

    // Create union node if needed (use add_struct for now, unions are similar)
    let union_id = helper.add_struct(union_name, None);

    // Create TypeOf edge with Field context
    let type_text = base_type_names.join(" ");
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        union_id,
        type_id,
        Some(TypeOfContext::Field),
        None,
        Some(&field_name),
    );

    // Create Reference edges for all referenced types
    for ref_type in &all_types {
        let ref_type_id = helper.add_type(ref_type, None);
        helper.add_reference_edge(union_id, ref_type_id);
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
