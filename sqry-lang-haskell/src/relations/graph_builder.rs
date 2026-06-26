//! `GraphBuilder` implementation for Haskell
//!
//! Builds the unified `CodeGraph` for Haskell files by:
//! 1. Extracting function definitions (top-level value declarations)
//! 2. Detecting function applications (calls, qualified calls, operator applications)
//! 3. Creating call edges between caller and callee
//! 4. Detecting FFI declarations (foreign import statements)
//!
//! ## Supported Patterns
//! - Top-level function bindings: `functionName args = body`
//! - Function applications: `functionName arg1 arg2`
//! - Qualified calls: `Module.functionName args`
//! - Operator applications: `(+) a b`, `a + b`
//! - FFI declarations: `foreign import ccall "exp" c_exp :: Double -> Double`
//!
//! ## FFI Patterns Detected
//! - Static ccall: `foreign import ccall "symbol" func :: Type`
//! - Dynamic ccall: `foreign import ccall "dynamic" mkFun :: ...`
//! - Wrapper: `foreign import ccall "wrapper" createCB :: ...`
//! - Address-of: `foreign import ccall "&symbol" ptr :: ...`
//! - Stdcall: `foreign import stdcall "Win32Func" func :: ...`
//! - CAPI: `foreign import capi "header.h symbol" func :: ...`
//!
//! ## Limitations
//! - Type class dispatch: Not tracked (requires type inference)
//! - Higher-order call resolution: Not tracked (runtime-dependent)
//! - Lazy evaluation paths: Not tracked (semantic analysis beyond scope)
//! - Template Haskell: Not tracked (compile-time only)
//! - FFI patterns deferred: `cplusplus`, `prim`, `javascript` (future phases)

use std::sync::OnceLock;
use std::{collections::HashMap, path::Path};

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::GraphBuildHelper;
use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use tree_sitter::{Node, Tree};

use crate::relations::type_extractor::extract_type_names_from_haskell_type;
#[cfg(test)]
use sqry_core::graph::unified::storage::NodeEntry;

// ============================================================================
// FFI Support Data Structures
// ============================================================================

/// FFI declaration metadata extracted from foreign import statements
#[derive(Debug, Clone)]
struct FfiDeclaration {
    /// Haskell wrapper function name (e.g., `c_exp`)
    wrapper_name: String,
    /// Foreign symbol (e.g., "exp", "dynamic", "&errno")
    foreign_symbol: String,
    /// FFI calling convention
    convention: FfiConvention,
    /// Safety modifier (propagated to graph metadata)
    safety: FfiSafety,
    /// Span in source file
    span: (usize, usize),
}

/// FFI safety modifiers (tracked for potential future use)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FfiSafety {
    Unsafe,
    Safe,
    Default, // Same as Safe per GHC spec
}

/// Maps Haskell wrapper function name to FFI declaration
type FfiRegistry = HashMap<String, FfiDeclaration>;

// ============================================================================
// GraphBuilder Implementation
// ============================================================================

/// `GraphBuilder` for Haskell files using manual tree walking approach
#[derive(Debug, Clone, Copy)]
pub struct HaskellGraphBuilder {
    max_scope_depth: usize,
}

impl Default for HaskellGraphBuilder {
    fn default() -> Self {
        Self {
            max_scope_depth: 3, // Haskell: module -> function -> nested where/let
        }
    }
}

impl HaskellGraphBuilder {
    #[must_use]
    pub fn new(max_scope_depth: usize) -> Self {
        Self { max_scope_depth }
    }
}

impl GraphBuilder for HaskellGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        let mut helper = GraphBuildHelper::new(staging, file, Language::Haskell);

        // Extract module name and exports if present
        let module_name = extract_module_name(tree, content);
        let module_node_name = module_name.as_deref().unwrap_or("<module>");
        let module_id = helper.add_module(module_node_name, None);

        // Extract export list to determine visibility
        // None = no export list (all functions public)
        // Some(list) = explicit exports (only listed functions are public)
        let module_exports = extract_module_exports(tree, content);

        // Build AST graph to track callable contexts
        let ast_graph = ASTGraph::from_tree(tree, content, self.max_scope_depth).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            }
        })?;

        // Create function nodes for all contexts with visibility metadata
        let mut context_to_node: HashMap<String, NodeId> = HashMap::new();
        for context in ast_graph.contexts() {
            let qualified_name = if let Some(module) = &module_name {
                format!("{}.{}", module, context.qualified_name)
            } else {
                context.qualified_name.clone()
            };

            // Determine visibility based on export list
            let visibility = match &module_exports {
                None => "public", // No export list = all functions public
                Some(exports) => {
                    // Check if this function is in the export list
                    if exports.contains(&context.qualified_name) {
                        "public"
                    } else {
                        "private"
                    }
                }
            };

            let span = Some(Span::from_bytes(context.span.0, context.span.1));
            let node_id = helper.add_function_with_visibility(
                &qualified_name,
                span,
                false,
                false,
                Some(visibility),
            );
            context_to_node.insert(context.qualified_name.clone(), node_id);
        }

        // Create export edges based on module export list
        match &module_exports {
            None => {
                // No explicit export list = export all top-level functions (Haskell default)
                for (context_name, &node_id) in &context_to_node {
                    // Only export top-level functions (no dots in unqualified name)
                    if !context_name.contains('.') {
                        helper.add_export_edge(module_id, node_id);
                    }
                }
            }
            Some(exports) => {
                // Explicit export list = only export listed functions
                for export_name in exports {
                    if let Some(&node_id) = context_to_node.get(export_name) {
                        helper.add_export_edge(module_id, node_id);
                    }
                }
            }
        }

        // Extract import edges from import declarations
        extract_import_edges(tree.root_node(), content, module_id, &mut helper);

        // Extract FFI declarations (foreign import statements)
        let mut ffi_registry = FfiRegistry::new();
        collect_ffi_declarations(tree.root_node(), content, &mut ffi_registry);

        // Build FFI edges (must happen before regular call detection)
        build_ffi_edges(
            &ffi_registry,
            module_name.as_deref(),
            module_id,
            &mut helper,
        );

        // Extract TypeOf edges from type signatures, data types, and class declarations
        extract_typeof_edges(
            tree.root_node(),
            content,
            &context_to_node,
            module_name.as_deref(),
            &mut helper,
        );

        // Walk the tree to find function applications and build call edges
        visit_node_for_calls(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &context_to_node,
            module_name.as_ref(),
        );

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Haskell
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(haskell_shape_mapping())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract module name from module declaration
/// Pattern: module `ModuleName` where
fn extract_module_name(tree: &Tree, content: &[u8]) -> Option<String> {
    // Traverse the tree looking for module declaration
    // AST structure: haskell -> header -> module (keyword) -> module (name node) -> module_id
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "header"
            && let Some(name) = extract_module_name_from_header(child, content)
        {
            return Some(name);
        }
    }

    None
}

/// Extract module export list from module declaration
/// Returns:
/// - `None` if no export list (all symbols are public by default)
/// - `Some(exports)` if explicit export list exists
/// - Pattern: module `ModuleName` (`export1`, `export2`) where
fn extract_module_exports(tree: &Tree, content: &[u8]) -> Option<Vec<String>> {
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "header" {
            return extract_exports_from_header(child, content);
        }
    }

    None
}

fn extract_exports_from_header(header: Node<'_>, content: &[u8]) -> Option<Vec<String>> {
    let mut header_cursor = header.walk();

    for header_child in header.children(&mut header_cursor) {
        if header_child.kind() == "exports" {
            // Found exports list - extract all exported symbols
            let mut exports = Vec::new();
            let mut exports_cursor = header_child.walk();

            for export_node in header_child.children(&mut exports_cursor) {
                // Export nodes can be various types: variable, type, etc.
                if let Ok(text) = export_node.utf8_text(content) {
                    let trimmed = text.trim().trim_matches(&['(', ')', ','][..]);
                    if !trimmed.is_empty() && trimmed != "exports" {
                        exports.push(trimmed.to_string());
                    }
                }

                // Also check children of export nodes for nested identifiers
                let mut inner_cursor = export_node.walk();
                for inner_child in export_node.children(&mut inner_cursor) {
                    if matches!(
                        inner_child.kind(),
                        "variable" | "type" | "constructor" | "operator"
                    ) && let Ok(inner_text) = inner_child.utf8_text(content)
                    {
                        let trimmed = inner_text.trim();
                        if !trimmed.is_empty() && !exports.contains(&trimmed.to_string()) {
                            exports.push(trimmed.to_string());
                        }
                    }
                }
            }

            return Some(exports);
        }
    }

    // No exports list found - all symbols are public by default
    None
}

fn extract_module_name_from_header(header: Node<'_>, content: &[u8]) -> Option<String> {
    let mut header_cursor = header.walk();
    for header_child in header.children(&mut header_cursor) {
        if matches!(header_child.kind(), "module" | "module_id")
            && let Ok(text) = header_child.utf8_text(content)
            && text != "module"
        {
            return Some(text.to_string());
        }
    }
    header
        .utf8_text(content)
        .ok()
        .and_then(parse_module_name_from_text)
}

fn parse_module_name_from_text(text: &str) -> Option<String> {
    let mut tokens = text.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "module"
            && let Some(name_token) = tokens.next()
        {
            let trimmed = name_token.trim_end_matches(['(', ';']);
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

// ============================================================================
// FFI Support Functions
// ============================================================================

/// Parse calling convention from foreign import node child
#[allow(
    clippy::match_same_arms,
    reason = "ccall and capi are distinct patterns that map to same convention for documentation clarity"
)]
fn parse_calling_convention(node: Node, content: &[u8]) -> Option<FfiConvention> {
    let text = node.utf8_text(content).ok()?;
    match text {
        "ccall" => Some(FfiConvention::C),
        "stdcall" => Some(FfiConvention::Stdcall),
        "capi" => Some(FfiConvention::C),
        // DEFERRED: cplusplus, prim, javascript (future phases)
        // These are explicitly not supported yet and return None
        _ => None,
    }
}

/// Extract text from string literal node, removing quotes
fn extract_string_literal(node: Node, content: &[u8]) -> Option<String> {
    if node.kind() == "string" || node.kind() == "string_literal" {
        let text = node.utf8_text(content).ok()?;
        // Remove surrounding quotes: "exp" → exp
        Some(text.trim_matches('"').to_string())
    } else {
        None
    }
}

/// Parse a single `foreign_import` node into `FfiDeclaration`
fn parse_foreign_import(node: Node, content: &[u8]) -> Option<FfiDeclaration> {
    // 1. Extract calling convention (required)
    let convention = {
        let mut cursor = node.walk();
        let mut convention_opt = None;

        for child in node.children(&mut cursor) {
            if let Some(conv) = parse_calling_convention(child, content) {
                convention_opt = Some(conv);
                break;
            }
        }
        convention_opt?
    };

    // 2. Extract safety modifier (optional)
    let safety = {
        let mut cursor = node.walk();
        let mut safety_opt = FfiSafety::Default;

        for child in node.children(&mut cursor) {
            // Check if this is a safety node (contains unsafe/safe as text)
            if child.kind() == "safety"
                && let Ok(text) = child.utf8_text(content)
            {
                match text {
                    "unsafe" => {
                        safety_opt = FfiSafety::Unsafe;
                        break;
                    }
                    "safe" => {
                        safety_opt = FfiSafety::Safe;
                        break;
                    }
                    _ => {}
                }
            }
            // Also check direct node kind for backward compatibility
            else if child.kind() == "unsafe" {
                safety_opt = FfiSafety::Unsafe;
                break;
            } else if child.kind() == "safe" {
                safety_opt = FfiSafety::Safe;
                break;
            }
        }
        safety_opt
    };

    // 3. Extract foreign symbol (entity string literal)
    // The string is nested inside an "entity" node, so we need to check its children
    let foreign_symbol = {
        let mut cursor = node.walk();
        let mut symbol_opt = None;

        for child in node.children(&mut cursor) {
            // Check if this is the entity node
            if child.kind() == "entity" {
                let mut entity_cursor = child.walk();
                for entity_child in child.children(&mut entity_cursor) {
                    if entity_child.kind() == "string" || entity_child.kind() == "string_literal" {
                        symbol_opt = extract_string_literal(entity_child, content);
                        break;
                    }
                }
                if symbol_opt.is_some() {
                    break;
                }
            }

            // Also check direct children (in case grammar varies)
            if child.kind() == "string" || child.kind() == "string_literal" {
                symbol_opt = extract_string_literal(child, content);
                break;
            }
        }
        symbol_opt?
    };

    // 4. Extract Haskell wrapper name (inside signature node)
    let wrapper_name = {
        let mut cursor = node.walk();
        let mut name_opt = None;

        for child in node.children(&mut cursor) {
            // The wrapper name is inside the "signature" node
            if child.kind() == "signature" {
                let mut sig_cursor = child.walk();
                for sig_child in child.children(&mut sig_cursor) {
                    if matches!(sig_child.kind(), "variable" | "identifier" | "name")
                        && let Ok(text) = sig_child.utf8_text(content)
                    {
                        name_opt = Some(text.to_string());
                        break;
                    }
                }
                if name_opt.is_some() {
                    break;
                }
            }

            // Also check direct children (in case grammar varies)
            if matches!(child.kind(), "variable" | "identifier" | "name")
                && let Ok(text) = child.utf8_text(content)
            {
                name_opt = Some(text.to_string());
                break;
            }
        }
        name_opt?
    };

    // 5. Calculate span
    let span = (node.start_byte(), node.end_byte());

    Some(FfiDeclaration {
        wrapper_name,
        foreign_symbol,
        convention,
        safety,
        span,
    })
}

/// Walk AST to collect all `foreign_import` declarations
fn collect_ffi_declarations(node: Node, content: &[u8], ffi_registry: &mut FfiRegistry) {
    // Check if current node is a foreign import
    if node.kind() == "foreign_import"
        && let Some(decl) = parse_foreign_import(node, content)
    {
        ffi_registry.insert(decl.wrapper_name.clone(), decl);
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ffi_declarations(child, content, ffi_registry);
    }
}

/// Create graph nodes and edges for FFI declarations
fn build_ffi_edges(
    ffi_registry: &FfiRegistry,
    module_name: Option<&str>,
    module_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    for (wrapper_name, decl) in ffi_registry {
        // Create qualified name for wrapper function
        let qualified_name = if let Some(module) = module_name {
            format!("{module}.{wrapper_name}")
        } else {
            wrapper_name.clone()
        };

        // Create Function node for Haskell wrapper
        let span = Some(Span::from_bytes(decl.span.0, decl.span.1));
        let is_unsafe = matches!(decl.safety, FfiSafety::Unsafe);
        let wrapper_node = helper.add_function(
            &qualified_name,
            span,
            false,     // is_async (always false for FFI)
            is_unsafe, // is_unsafe (propagate from FFI safety modifier)
        );

        // Create Function node for foreign target
        // Use "ffi::<convention>::<symbol>" naming pattern
        let convention_str = match decl.convention {
            FfiConvention::C => "C",
            FfiConvention::Stdcall => "stdcall",
            _ => "unknown",
        };
        let ffi_target_name = format!("ffi::{convention_str}::{}", decl.foreign_symbol);
        let ffi_target_node = helper.ensure_callee(
            &ffi_target_name,
            Span::from_bytes(decl.span.0, decl.span.1),
            CalleeKindHint::Function,
        );

        // Create FfiCall edge from wrapper to foreign symbol
        helper.add_ffi_edge(wrapper_node, ffi_target_node, decl.convention);

        // Add Contains edge from module to wrapper
        helper.add_contains_edge(module_id, wrapper_node);
    }
}

/// Extract import edges from Haskell import declarations.
///
/// AST patterns (tree-sitter-haskell):
/// - `import Data.List` → `import` node with `module` child
/// - `import qualified Data.Map as M` → `qualified` + `module` + `as` alias
/// - `import Data.List (sort, nub)` → `import_list` present (wildcard=false)
/// - `import Data.List hiding (sort)` → `hiding` present (wildcard=false)
///
/// Handles various import patterns:
/// - `import Data.List` - simple import
/// - `import qualified Data.Map as M` - qualified import with alias
/// - `import Data.List (sort, nub)` - import with explicit list
/// - `import Data.List hiding (sort)` - import with hiding list
fn extract_import_edges(
    root: Node<'_>,
    content: &[u8],
    module_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = root.walk();

    // Look for import declarations in the tree
    // Haskell AST structure: haskell -> imports -> import
    for child in root.children(&mut cursor) {
        if child.kind() == "imports" {
            // Found imports section
            let mut imports_cursor = child.walk();
            for import_node in child.children(&mut imports_cursor) {
                if import_node.kind() == "import" {
                    build_import_edge(import_node, content, module_id, helper);
                }
            }
        } else if child.kind() == "import" {
            // Top-level import (not in imports section)
            build_import_edge(child, content, module_id, helper);
        }
    }
}

/// Build an import edge from a single import declaration
fn build_import_edge(
    import_node: Node<'_>,
    content: &[u8],
    module_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut module_name: Option<String> = None;
    let mut alias_name: Option<String> = None;
    let mut is_qualified = false;
    let mut has_hiding = false;
    let mut has_explicit_list = false;
    let mut seen_as = false;

    let mut cursor = import_node.walk();
    for child in import_node.children(&mut cursor) {
        match child.kind() {
            "qualified" => {
                is_qualified = true;
            }
            "as" => {
                // Mark that we've seen "as" - next module is the alias
                seen_as = true;
            }
            "module" | "module_id" => {
                if let Ok(text) = child.utf8_text(content)
                    && text != "import"
                    && text != "qualified"
                    && text != "as"
                    && text != "hiding"
                {
                    if seen_as {
                        // This module is the alias (after "as")
                        alias_name = Some(text.to_string());
                    } else if module_name.is_none() {
                        // This is the imported module name
                        module_name = Some(text.to_string());
                    }
                }
            }
            "module_alias" | "alias" => {
                // Alias after "as" (for grammars that use explicit alias node)
                if let Ok(text) = child.utf8_text(content) {
                    alias_name = Some(text.to_string());
                }
            }
            "import_list" | "exports" | "explicit_list" => {
                // Explicit import list: (foo, bar)
                has_explicit_list = true;
            }
            "hidden_list" | "hiding" => {
                // Hiding list: hiding (foo, bar)
                has_hiding = true;
            }
            _ => {
                // Also check children for module name if not found yet
                if module_name.is_none() {
                    let mut inner_cursor = child.walk();
                    for inner_child in child.children(&mut inner_cursor) {
                        if (inner_child.kind() == "module" || inner_child.kind() == "module_id")
                            && let Ok(text) = inner_child.utf8_text(content)
                        {
                            module_name = Some(text.to_string());
                            break;
                        }
                    }
                }
            }
        }
    }

    // Also try extracting module name directly from import node text (fallback)
    if module_name.is_none()
        && let Ok(import_text) = import_node.utf8_text(content)
    {
        // Parse "import [qualified] Module.Name [as Alias] [(...)]"
        let parts: Vec<&str> = import_text.split_whitespace().collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == "import" || *part == "qualified" || *part == "as" || *part == "hiding" {
                continue;
            }
            // Skip the alias if we're after "as"
            if i > 0 && parts.get(i - 1) == Some(&"as") {
                if alias_name.is_none() {
                    alias_name = Some((*part).to_string());
                }
                continue;
            }
            // First non-keyword is the module name
            if module_name.is_none() && !part.starts_with('(') {
                module_name = Some((*part).to_string());
            }
        }
    }

    // Create the import edge if we found a module name
    if let Some(imported_module) = module_name {
        let span = Span::from_bytes(import_node.start_byte(), import_node.end_byte());

        // Prefix for qualified imports to distinguish semantic meaning
        let import_name = if is_qualified {
            format!("qualified:{imported_module}")
        } else {
            imported_module.clone()
        };

        let import_id = helper.add_import(&import_name, Some(span));

        // Determine if this is a wildcard import:
        // - Simple import (import Data.List) = true (brings all symbols into scope)
        // - import with explicit list = false
        // - import with hiding = false
        // - qualified import = false (symbols require qualifier, not directly in scope)
        let is_wildcard = !has_explicit_list && !has_hiding && !is_qualified;

        // Always use add_import_edge_full to correctly set metadata
        helper.add_import_edge_full(module_id, import_id, alias_name.as_deref(), is_wildcard);
    }
}

/// Extract function name and argument count from apply node
/// Haskell AST: apply represents function application with currying
/// Structure: (apply (apply func arg1) arg2) for func arg1 arg2
fn extract_apply(app_node: Node<'_>, content: &[u8]) -> (String, usize) {
    // Count total arguments by unrolling nested apply nodes
    let mut arg_count = 0;
    let mut current = app_node;
    let mut function_name = String::new();

    // Walk up the apply chain to count arguments
    loop {
        if current.child_by_field_name("argument").is_some() {
            arg_count += 1;
        }
        if let Some(function) = current.child_by_field_name("function") {
            if function.kind() == "apply" {
                current = function;
                continue;
            }
            if function_name.is_empty() {
                function_name = strip_backticks(function.utf8_text(content).unwrap_or(""));
            }
            break;
        }

        let mut cursor = current.walk();
        let mut found_nested_apply = false;

        for child in current.children(&mut cursor) {
            match child.kind() {
                "apply" => {
                    // Nested apply - continue unwrapping
                    current = child;
                    found_nested_apply = true;
                    arg_count += 1;
                    break;
                }
                "variable" | "constructor" | "qualified_variable" | "qualified_constructor" => {
                    // Found the function name (strip backticks for backtick operators)
                    if function_name.is_empty() {
                        function_name = strip_backticks(child.utf8_text(content).unwrap_or(""));
                    } else {
                        arg_count += 1;
                    }
                }
                "literal" | "parens" => {
                    // Argument
                    arg_count += 1;
                }
                _ => {
                    // Other arguments
                    if !function_name.is_empty() {
                        arg_count += 1;
                    }
                }
            }
        }

        if !found_nested_apply {
            break;
        }
    }

    (function_name, arg_count)
}

/// Extract operator and argument count from infix node
/// Pattern: `a + b` or `` a `mod` b ``
fn extract_infix(app_node: Node<'_>, content: &[u8]) -> (String, usize) {
    let mut cursor = app_node.walk();
    let mut operator_name = String::new();

    // Look for the operator child — includes backtick-wrapped identifiers
    // which appear as variable/qualified_variable nodes
    for child in app_node.children(&mut cursor) {
        match child.kind() {
            "operator" | "variable_operator" | "constructor_operator" => {
                operator_name = child.utf8_text(content).unwrap_or("").to_string();
                break;
            }
            "variable" | "qualified_variable" | "constructor" | "qualified_constructor" => {
                // Backtick operators: `div` appears as a variable node
                operator_name = strip_backticks(child.utf8_text(content).unwrap_or(""));
                break;
            }
            "infix_id" | "prefix_id" => {
                // Some grammar versions wrap backtick operators in infix_id
                operator_name = strip_backticks(child.utf8_text(content).unwrap_or(""));
                break;
            }
            _ => {}
        }
    }

    // Infix operators always have 2 arguments (binary)
    (operator_name, 2)
}

/// Strip surrounding backticks from Haskell backtick operator names.
/// `` `div` `` becomes `div`, plain names pass through unchanged.
fn strip_backticks(name: &str) -> String {
    if name.len() > 2 && name.starts_with('`') && name.ends_with('`') {
        name[1..name.len() - 1].to_string()
    } else {
        name.to_string()
    }
}

/// Convert a tree-sitter node to a Span
fn span_from_node(node: Node<'_>) -> Span {
    Span::from_bytes(node.start_byte(), node.end_byte())
}

/// Visit nodes recursively to find function applications and create call edges
fn visit_node_for_calls(
    node: Node<'_>,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&String>,
) {
    // Check if this node is a function application
    match node.kind() {
        "apply" | "infix" | "negate" => {
            // Get the context for this call
            if let Some(context) = ast_graph.get_callable_context(node.id())
                && let Some(&caller_id) = context_to_node.get(&context.qualified_name)
            {
                // Extract callee information
                let (callee_name, arg_count) = match node.kind() {
                    "apply" => extract_apply(node, content),
                    "infix" => extract_infix(node, content),
                    "negate" => (String::from("negate"), 1),
                    _ => return, // Skip unknown node types
                };

                if !callee_name.is_empty() {
                    // Qualify the callee name if it doesn't already contain a module prefix
                    let qualified_callee = if callee_name.contains('.') {
                        callee_name.clone()
                    } else if let Some(module) = module_name {
                        format!("{module}.{callee_name}")
                    } else {
                        callee_name.clone()
                    };

                    // Get or create callee node
                    let callee_id = helper.add_function(&qualified_callee, None, false, false);

                    // Create call edge
                    let argument_count = u8::try_from(arg_count).unwrap_or(u8::MAX);
                    let call_span = span_from_node(node);
                    helper.add_call_edge_full_with_span(
                        caller_id,
                        callee_id,
                        argument_count,
                        false,
                        vec![call_span],
                    );
                }
            }
        }
        _ => {}
    }

    // Recursively visit children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node_for_calls(
            child,
            content,
            ast_graph,
            helper,
            context_to_node,
            module_name,
        );
    }
}

// ============================================================================
// TypeOf Edge Extraction
// ============================================================================

/// Orchestrate `TypeOf` edge extraction from type signatures, data types,
/// newtypes, type synonyms, and typeclass declarations.
fn extract_typeof_edges(
    node: Node<'_>,
    content: &[u8],
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    match node.kind() {
        "signature" => {
            process_type_signature(node, content, context_to_node, module_name, helper);
        }
        "data_type" => {
            process_data_type(node, content, module_name, helper);
        }
        "newtype" => {
            process_newtype(node, content, module_name, helper);
        }
        // Grammar typo in tree-sitter-haskell v0.23.1: "type_synomym" not "type_alias"
        "type_synomym" => {
            process_type_synonym(node, content, module_name, helper);
        }
        "class" => {
            // Class declarations handle their own child signatures via
            // process_class_declarations → find_and_process_class_signatures.
            // Do NOT recurse into class children to avoid double-processing
            // method signatures through the generic process_type_signature path,
            // which could leak class method types onto top-level functions.
            process_class_declarations(node, content, context_to_node, module_name, helper);
            return;
        }
        _ => {}
    }

    // Recurse into children (skipped for class nodes — see above)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_typeof_edges(child, content, context_to_node, module_name, helper);
    }
}

/// Extract type text from a type node, unwrapping wrappers like
/// `quantified_type`, `lazy_field`, `strict_field`, `parens`.
/// Returns the trimmed text of the innermost meaningful type node.
fn extract_type_text(node: Node<'_>, content: &[u8]) -> Option<String> {
    match node.kind() {
        // Wrapper nodes — descend into first named child
        "quantified_type" | "lazy_field" | "strict_field" => {
            // Try to find the inner type child
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                // Skip non-type children like `forall`, `context` wrappers
                if matches!(child.kind(), "forall" | "forall_required" | "context") {
                    continue;
                }
                return extract_type_text(child, content);
            }
            // Fallback to full text
            node.utf8_text(content).ok().map(|t| t.trim().to_string())
        }
        "parens" => {
            // Unwrap parentheses — get inner content
            if let Some(inner) = node.named_child(0) {
                return extract_type_text(inner, content);
            }
            node.utf8_text(content).ok().map(|t| t.trim().to_string())
        }
        // Leaf or compound type nodes — return full text
        _ => node.utf8_text(content).ok().map(|t| t.trim().to_string()),
    }
}

/// Flatten a Haskell function type `A -> B -> C` into parameter types and return type.
/// Handles both `function` (normal `->`) and `linear_function` (`%1 ->`).
///
/// For `Int -> String -> Bool`:
/// - params: `["Int", "String"]`
/// - return: `"Bool"`
fn flatten_function_type(node: Node<'_>, content: &[u8]) -> (Vec<String>, String) {
    let mut params = Vec::new();
    let mut current = node;

    loop {
        // Unwrap quantified_type and parens wrappers if present
        let inner = unwrap_parens(unwrap_quantified_type(current));

        if matches!(inner.kind(), "function" | "linear_function") {
            // Collect parameter type
            if let Some(param_node) = inner.child_by_field_name("parameter")
                && let Some(param_text) = extract_type_text(param_node, content)
            {
                params.push(param_text);
            }
            // Recurse into result
            if let Some(result_node) = inner.child_by_field_name("result") {
                current = result_node;
                continue;
            }
            // No result field — treat full text as return type
            let ret = inner
                .utf8_text(content)
                .map(|t| t.trim().to_string())
                .unwrap_or_default();
            return (params, ret);
        }

        // Not a function type — this is the return type
        let ret = extract_type_text(inner, content).unwrap_or_default();
        return (params, ret);
    }
}

/// Unwrap a `quantified_type` wrapper to get the inner type node.
/// If the node is not a `quantified_type`, returns it unchanged.
///
/// **Important**: This does NOT unwrap `forall` nodes directly. The `forall`
/// unwrapping is handled separately by `unwrap_forall()` in the signature
/// processors. Unwrapping `forall` here would cause `flatten_function_type`
/// to incorrectly decompose rank-2 types like `a -> forall b. b -> a` into
/// two parameters instead of treating the `forall` return as an opaque type.
fn unwrap_quantified_type(node: Node<'_>) -> Node<'_> {
    if node.kind() == "quantified_type" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // Skip `forall`, `context` wrappers — find the actual type
            if !matches!(
                child.kind(),
                "forall" | "forall_required" | "context" | "constraints"
            ) {
                return child;
            }
        }
    }
    node
}

/// Unwrap a `forall` or `forall_required` wrapper to get the inner type.
///
/// Handles `forall a. a -> a` (inner = `function`) and
/// `forall a. Show a => a -> String` (inner = `context` wrapping `function`).
fn unwrap_forall(node: Node<'_>) -> Node<'_> {
    if matches!(node.kind(), "forall" | "forall_required") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // Skip `quantified_variables` — return the first actual type child
            if child.kind() != "quantified_variables" {
                return child;
            }
        }
    }
    node
}

/// Unwrap a `parens` wrapper to get the inner type node.
///
/// Handles `(Show a => a -> String)` → `context(Show a => ...)` and
/// `(a -> String)` → `function(a -> String)`.
fn unwrap_parens(node: Node<'_>) -> Node<'_> {
    if node.kind() == "parens"
        && let Some(inner) = node.named_child(0)
    {
        return inner;
    }
    node
}

/// Extract function names from a `signature` node.
/// Handles both single name (`signature.name`) and multi-name (`signature.names` → `binding_list`).
fn extract_signature_names<'a>(sig_node: Node<'a>, content: &'a [u8]) -> Vec<String> {
    let mut names = Vec::new();

    // Try single name field first
    if let Some(name_node) = sig_node.child_by_field_name("name")
        && let Ok(text) = name_node.utf8_text(content)
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            names.push(trimmed.to_string());
        }
    }

    // Try multi-name field (binding_list)
    if let Some(names_node) = sig_node.child_by_field_name("names") {
        let mut cursor = names_node.walk();
        for child in names_node.children_by_field_name("name", &mut cursor) {
            if let Ok(text) = child.utf8_text(content) {
                let trimmed = text.trim();
                if !trimmed.is_empty() && !names.contains(&trimmed.to_string()) {
                    names.push(trimmed.to_string());
                }
            }
        }
    }

    names
}

/// Result of unwrapping a Haskell type signature through forall, parens, and context layers.
///
/// Used by both `process_type_signature` and `process_class_method_signature` to avoid
/// duplicating the unwrap chain logic.
struct UnwrappedSignature<'a> {
    /// The inner type node after stripping forall/parens/context wrappers.
    actual_type_node: Node<'a>,
    /// The constraint text (e.g., `"Show a"`) if a context or constraint was found.
    constraint_text: Option<String>,
    /// The constraint AST node for extracting References edges from constraint class names.
    constraint_node: Option<Node<'a>>,
}

/// Unwrap a signature type node through `forall → parens → forall → context` layers.
///
/// This shared helper normalizes the unwrapping logic used by both top-level and
/// class method signature processors. Returns the actual type node, optional constraint
/// text for `TypeOf` edges, and optional constraint AST node for `References` edges.
fn unwrap_signature_type<'a>(
    type_node: Node<'a>,
    sig_node: Node<'a>,
    content: &[u8],
) -> UnwrappedSignature<'a> {
    // Unwrap `forall` wrapper: `forall a. <inner_type>`
    let type_node = unwrap_forall(type_node);

    // Unwrap `parens` wrapper: `(Show a => a -> String)` → `Show a => a -> String`
    let type_node = unwrap_parens(type_node);

    // Handle `(forall a. ...)` shape: parens wrapping forall. The first
    // `unwrap_forall` was a no-op on the `parens` node, so after unwrapping
    // parens we may now see a `forall` that needs unwrapping.
    let type_node = unwrap_forall(type_node);

    // Handle `context` wrapper: `Show a => a -> String`
    if type_node.kind() == "context" {
        let constraint_node = type_node.child_by_field_name("context");
        let constraint_text = constraint_node
            .and_then(|c| c.utf8_text(content).ok())
            .map(|t| t.trim().to_string());
        let inner = type_node.child_by_field_name("type").unwrap_or(type_node);
        UnwrappedSignature {
            actual_type_node: inner,
            constraint_text,
            constraint_node,
        }
    } else {
        // Fallback: check for `constraint` field directly on signature node
        // for grammar-shape robustness across tree-sitter-haskell versions
        let constraint_node = sig_node.child_by_field_name("constraint");
        let constraint_text = constraint_node
            .and_then(|c| c.utf8_text(content).ok())
            .map(|t| t.trim().to_string());
        UnwrappedSignature {
            actual_type_node: type_node,
            constraint_text,
            constraint_node,
        }
    }
}

/// Emit `References` edges from a source node to all type constructors extracted from
/// a type AST node. Optionally also extracts from a constraint AST node, deduplicating
/// names already emitted from the type node.
fn emit_references_edges(
    source_id: NodeId,
    type_node: Node<'_>,
    constraint_node: Option<Node<'_>>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) {
    let mut seen = std::collections::HashSet::new();
    emit_references_edges_dedup(
        source_id,
        type_node,
        constraint_node,
        content,
        helper,
        &mut seen,
    );
}

/// Emit `References` edges with cross-call deduplication via a shared `seen` set.
///
/// This variant is used by functions that call the emitter multiple times for the same
/// source symbol (e.g., `process_record_fields` iterating over fields). The `seen` set
/// ensures at most one `References` edge per `(source, target_type_name)` pair.
fn emit_references_edges_dedup(
    source_id: NodeId,
    type_node: Node<'_>,
    constraint_node: Option<Node<'_>>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    seen: &mut std::collections::HashSet<String>,
) {
    let ref_names = extract_type_names_from_haskell_type(type_node, content);
    for ref_name in &ref_names {
        if seen.insert(ref_name.clone()) {
            let ref_type_id = helper.add_type(ref_name, None);
            helper.add_reference_edge(source_id, ref_type_id);
        }
    }
    // References from constraint class names (deduplicated against type names + seen set)
    if let Some(constraint_ast) = constraint_node {
        for ref_name in extract_type_names_from_haskell_type(constraint_ast, content) {
            if seen.insert(ref_name.clone()) {
                let ref_type_id = helper.add_type(&ref_name, None);
                helper.add_reference_edge(source_id, ref_type_id);
            }
        }
    }
}

/// Process a type signature and create `TypeOf` and `References` edges for parameters,
/// return type, and constraints.
///
/// Handles: `foo :: Int -> String -> Bool`, `foo, bar :: Int -> Int`, `process :: Show a => a -> String`
fn process_type_signature(
    sig_node: Node<'_>,
    content: &[u8],
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    let func_names = extract_signature_names(sig_node, content);
    if func_names.is_empty() {
        return;
    }

    // Extract type from signature
    let Some(type_node) = sig_node.child_by_field_name("type") else {
        return;
    };

    // Unwrap forall/parens/context layers using shared helper
    let unwrapped = unwrap_signature_type(type_node, sig_node, content);

    // Flatten the function type to get parameters and return type
    let (param_types, return_type) = flatten_function_type(unwrapped.actual_type_node, content);

    // Create edges for each function name
    for func_name in &func_names {
        let node_id = if let Some(&id) = context_to_node.get(func_name) {
            id
        } else if let Some(module) = module_name {
            // Try with module qualification
            let qualified = format!("{module}.{func_name}");
            if let Some(&id) = context_to_node.get(&qualified) {
                id
            } else {
                // Function might not have a definition (e.g., class method signature)
                // Skip — class methods are handled by process_class_declarations
                continue;
            }
        } else {
            continue;
        };

        // Create parameter TypeOf edges
        for (idx, param_type) in param_types.iter().enumerate() {
            let type_id = helper.add_type(param_type, None);
            #[allow(clippy::cast_possible_truncation)]
            helper.add_typeof_edge_with_context(
                node_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(idx as u16),
                None, // Haskell signatures don't name parameters
            );
        }

        // Create return type TypeOf edge
        if !return_type.is_empty() {
            let return_type_id = helper.add_type(&return_type, None);
            helper.add_typeof_edge_with_context(
                node_id,
                return_type_id,
                Some(TypeOfContext::Return),
                Some(0),
                None,
            );
        }

        // Create constraint TypeOf edge if present
        if let Some(ref constraint) = unwrapped.constraint_text {
            let constraint_type_id = helper.add_type(constraint, None);
            helper.add_typeof_edge_with_context(
                node_id,
                constraint_type_id,
                Some(TypeOfContext::Constraint),
                None,
                None,
            );
        }

        // References edges to all individual type constructors (deduplicated)
        emit_references_edges(
            node_id,
            unwrapped.actual_type_node,
            unwrapped.constraint_node,
            content,
            helper,
        );
    }
}

/// Process a data type declaration and create `TypeOf` edges for record fields,
/// prefix constructor arguments, infix constructor operands, and GADT constructors.
fn process_data_type(
    data_node: Node<'_>,
    content: &[u8],
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    // Extract data type name
    let Some(name_node) = data_node.child_by_field_name("name") else {
        return;
    };
    let Some(type_name) = name_node.utf8_text(content).ok() else {
        return;
    };
    let type_name = type_name.trim();
    if type_name.is_empty() {
        return;
    }

    let qualified_name = if let Some(module) = module_name {
        format!("{module}.{type_name}")
    } else {
        type_name.to_string()
    };

    let span = Some(Span::from_bytes(
        data_node.start_byte(),
        data_node.end_byte(),
    ));
    let data_type_id = helper.add_type(&qualified_name, span);

    // Process constructors
    let Some(constructors_node) = data_node.child_by_field_name("constructors") else {
        return;
    };

    // Shared dedup state across all constructors of this data type
    let mut ref_seen = std::collections::HashSet::new();

    match constructors_node.kind() {
        "data_constructors" => {
            process_data_constructors(
                constructors_node,
                content,
                data_type_id,
                &qualified_name,
                helper,
                &mut ref_seen,
            );
        }
        "gadt_constructors" => {
            process_gadt_constructors(
                constructors_node,
                content,
                data_type_id,
                &qualified_name,
                helper,
                &mut ref_seen,
            );
        }
        _ => {}
    }
}

/// Process regular data constructors (record, prefix, infix).
/// `ref_seen` is shared across all constructors of the same data type.
///
/// `data_type_qualified_name` is the package-qualified name of the
/// enclosing data type (e.g. `MyModule.Person`) — passed through so
/// `process_record_fields` can mint the per-field `Constant` qualified
/// names as `<Module>.<TypeName>.<FieldName>` (Cluster C /
/// `C_OTHER_PLUGINS`, mirrors the Java/Kotlin/Dart/classpath/Go
/// cross-language norm).
fn process_data_constructors(
    constructors_node: Node<'_>,
    content: &[u8],
    data_type_id: NodeId,
    data_type_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    ref_seen: &mut std::collections::HashSet<String>,
) {
    let mut cursor = constructors_node.walk();
    for data_ctor in constructors_node.named_children(&mut cursor) {
        if data_ctor.kind() != "data_constructor" {
            continue;
        }
        // The constructor field holds the actual constructor node (record | prefix | infix)
        if let Some(ctor_node) = data_ctor.child_by_field_name("constructor") {
            match ctor_node.kind() {
                "record" => {
                    process_record_fields(
                        ctor_node,
                        content,
                        data_type_id,
                        data_type_qualified_name,
                        helper,
                        ref_seen,
                    );
                }
                "prefix" => {
                    process_prefix_constructor(ctor_node, content, data_type_id, helper, ref_seen);
                }
                "infix" => {
                    process_infix_constructor(ctor_node, content, data_type_id, helper, ref_seen);
                }
                _ => {}
            }
        }
    }
}

/// Process record fields: `{ name :: String, age :: Int }` or `{ x, y :: Int }`
///
/// Cluster C / `C_OTHER_PLUGINS` (`BadLiveware` Go-batch DAG, 2026-04-29):
/// every named Haskell record field is materialised as a
/// `NodeKind::Constant` node, parented to the enclosing data-type node
/// via `Defines` + `Contains` edges. The qualified-name format is
/// `<Module>.<TypeName>.<FieldName>` (or `<TypeName>.<FieldName>` when
/// the file has no module header), matching the
/// Java/Kotlin/Dart/classpath/Go cross-language norm.
///
/// **Why `Constant` and not `Property`:** Haskell record fields are
/// immutable by language definition (the `data`/`newtype` syntax has
/// no mutation), so we mirror the Java convention of emitting
/// `NodeKind::Constant` for `final` fields. The audit document
/// (`docs/development/public-issue-triage/cluster_c_field_audit.md`)
/// records this recommendation explicitly.
///
/// The `TypeOf{Field}` edge's source is also migrated from the data-type
/// node to the new `Constant` node (mirroring Go's `C_EDGE_MIGRATE`
/// pattern). Aggregate "all fields of this data type" queries continue
/// to resolve via the `Defines` / `Contains` parenting; only the edge
/// source identity changes. The `TypeOfContext::Field` discriminator,
/// the `field_index`, and the `field_name` metadata are unchanged.
///
/// Visibility defaults to `None`. Haskell's per-symbol visibility is
/// driven by the module's export list, which is not yet plumbed through
/// to record-field emission; tightening visibility to honour
/// `module Foo (Bar(field)) where ...` exports is left to a follow-up
/// that consults the parsed export list directly at field-emit time.
///
/// **Anonymous record fields** (the `field_index`-only branch below,
/// `x :: Int` with no `name` child — currently a tree-sitter quirk on
/// malformed input) are deliberately skipped for `Constant` emission:
/// without a stable field name we cannot synthesise a qualified name
/// that resolves from CLI / MCP / LSP queries. The legacy
/// `TypeOf{Field}` edge for those positions still fires so the
/// type-flow graph stays complete; only the per-field node is omitted.
fn process_record_fields(
    record_node: Node<'_>,
    content: &[u8],
    data_type_id: NodeId,
    data_type_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    ref_seen: &mut std::collections::HashSet<String>,
) {
    // The `record` node has a `fields` child (kind=fields) that contains the `field` children.
    // Access via `child_by_field_name("fields")` to get to the container node.
    let fields_container = record_node
        .child_by_field_name("fields")
        .unwrap_or(record_node);

    let mut field_index: u16 = 0;
    let mut cursor = fields_container.walk();
    for child in fields_container.named_children(&mut cursor) {
        if child.kind() != "field" {
            continue;
        }

        // Extract field type node and text
        let field_type_node = child
            .child_by_field_name("type")
            .or_else(|| child.child_by_field_name("parameter"));

        let Some(type_node) = field_type_node else {
            continue;
        };
        let Some(type_text) = extract_type_text(type_node, content) else {
            continue;
        };

        // Extract all field-name AST nodes (handles `x, y :: Int` — each
        // name is its own child of the `field` node). We keep the
        // `Node` (not just the text) so each Constant gets a span
        // pointing at its own identifier.
        let mut name_cursor = child.walk();
        let name_nodes: Vec<Node<'_>> = child
            .children_by_field_name("name", &mut name_cursor)
            .collect();

        if name_nodes.is_empty() {
            // Field without explicit name — emit only the legacy
            // `TypeOf{Field}` edge keyed by `field_index`. No Constant
            // node is materialised because there is no stable name to
            // qualify it with (see function-level doc comment).
            let type_id = helper.add_type(&type_text, None);
            helper.add_typeof_edge_with_context(
                data_type_id,
                type_id,
                Some(TypeOfContext::Field),
                Some(field_index),
                None,
            );
            field_index += 1;

            // References edges still source from the data type for
            // anonymous-field positions — the data type is the only
            // anchor we have.
            emit_references_edges_dedup(data_type_id, type_node, None, content, helper, ref_seen);
        } else {
            // One Constant + one TypeOf{Field} edge per declared name.
            for name_node in &name_nodes {
                let Ok(name_text) = name_node.utf8_text(content) else {
                    field_index += 1;
                    continue;
                };
                let name = name_text.trim();
                if name.is_empty() {
                    field_index += 1;
                    continue;
                }

                let qualified_field_name = format!("{data_type_qualified_name}.{name}");
                let constant_id = helper.add_constant_with_static_and_visibility(
                    &qualified_field_name,
                    Some(span_from_node(*name_node)),
                    false, // Haskell record fields have no class-level `static`.
                    None,  // Visibility comes from the module export list — out of scope.
                );
                helper.add_defines_edge(data_type_id, constant_id);
                helper.add_contains_edge(data_type_id, constant_id);

                let type_id = helper.add_type(&type_text, None);
                helper.add_typeof_edge_with_context(
                    constant_id,
                    type_id,
                    Some(TypeOfContext::Field),
                    Some(field_index),
                    Some(name),
                );

                // References edges sourced at the Constant so the
                // field is the queryable anchor for "what types does
                // this record field reference".
                emit_references_edges_dedup(
                    constant_id,
                    type_node,
                    None,
                    content,
                    helper,
                    ref_seen,
                );

                field_index += 1;
            }
        }
    }
}

/// Process prefix constructor arguments: `Bar a | Baz`
/// `prefix` node has `field` children (type | `strict_field` | `lazy_field`)
fn process_prefix_constructor(
    prefix_node: Node<'_>,
    content: &[u8],
    data_type_id: NodeId,
    helper: &mut GraphBuildHelper,
    ref_seen: &mut std::collections::HashSet<String>,
) {
    let mut cursor = prefix_node.walk();
    let mut param_index: u16 = 0;
    for child in prefix_node.children_by_field_name("field", &mut cursor) {
        if let Some(type_text) = extract_type_text(child, content) {
            let type_id = helper.add_type(&type_text, None);
            helper.add_typeof_edge_with_context(
                data_type_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(param_index),
                None,
            );
            param_index += 1;
            // References edges with cross-field deduplication
            emit_references_edges_dedup(data_type_id, child, None, content, helper, ref_seen);
        }
    }
}

/// Process infix constructor operands: ``a `Pair` b``
/// `infix` node has `left_operand` and `right_operand` fields
fn process_infix_constructor(
    infix_node: Node<'_>,
    content: &[u8],
    data_type_id: NodeId,
    helper: &mut GraphBuildHelper,
    ref_seen: &mut std::collections::HashSet<String>,
) {
    if let Some(left) = infix_node.child_by_field_name("left_operand")
        && let Some(type_text) = extract_type_text(left, content)
    {
        let type_id = helper.add_type(&type_text, None);
        helper.add_typeof_edge_with_context(
            data_type_id,
            type_id,
            Some(TypeOfContext::Parameter),
            Some(0),
            None,
        );
        emit_references_edges_dedup(data_type_id, left, None, content, helper, ref_seen);
    }
    if let Some(right) = infix_node.child_by_field_name("right_operand")
        && let Some(type_text) = extract_type_text(right, content)
    {
        let type_id = helper.add_type(&type_text, None);
        helper.add_typeof_edge_with_context(
            data_type_id,
            type_id,
            Some(TypeOfContext::Parameter),
            Some(1),
            None,
        );
        emit_references_edges_dedup(data_type_id, right, None, content, helper, ref_seen);
    }
}

/// Process GADT constructors.
/// Each `gadt_constructor` has a `type` field that may contain a function type.
fn process_gadt_constructors(
    gadt_node: Node<'_>,
    content: &[u8],
    data_type_id: NodeId,
    data_type_qualified_name: &str,
    helper: &mut GraphBuildHelper,
    ref_seen: &mut std::collections::HashSet<String>,
) {
    let mut cursor = gadt_node.walk();
    for ctor in gadt_node.named_children(&mut cursor) {
        if ctor.kind() != "gadt_constructor" {
            continue;
        }
        // GADT constructor type field holds the constructor type signature
        if let Some(type_node) = ctor.child_by_field_name("type") {
            // For record GADT constructors, process record fields
            if type_node.kind() == "record" {
                process_record_fields(
                    type_node,
                    content,
                    data_type_id,
                    data_type_qualified_name,
                    helper,
                    ref_seen,
                );
            } else {
                // GADT: `Lit :: Int -> Expr Int`
                // The type field may be a `prefix` node wrapping a `function` node.
                // Unwrap: prefix → prefix.type → function
                let inner_type = if type_node.kind() == "prefix" {
                    type_node.child_by_field_name("type").unwrap_or(type_node)
                } else {
                    type_node
                };
                let (params, _return_type) = flatten_function_type(inner_type, content);
                for (idx, param_type) in params.iter().enumerate() {
                    let type_id = helper.add_type(param_type, None);
                    #[allow(clippy::cast_possible_truncation)]
                    helper.add_typeof_edge_with_context(
                        data_type_id,
                        type_id,
                        Some(TypeOfContext::Parameter),
                        Some(idx as u16),
                        None,
                    );
                }
                // References edges with cross-constructor deduplication
                emit_references_edges_dedup(
                    data_type_id,
                    inner_type,
                    None,
                    content,
                    helper,
                    ref_seen,
                );
            }
        }
    }
}

/// Process a newtype declaration.
/// `newtype Wrapped = Wrapped Int`
fn process_newtype(
    newtype_node: Node<'_>,
    content: &[u8],
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    // Extract newtype name
    let Some(name_node) = newtype_node.child_by_field_name("name") else {
        return;
    };
    let Some(type_name) = name_node.utf8_text(content).ok() else {
        return;
    };
    let type_name = type_name.trim();
    if type_name.is_empty() {
        return;
    }

    let qualified_name = if let Some(module) = module_name {
        format!("{module}.{type_name}")
    } else {
        type_name.to_string()
    };

    let span = Some(Span::from_bytes(
        newtype_node.start_byte(),
        newtype_node.end_byte(),
    ));
    let newtype_type_id = helper.add_type(&qualified_name, span);

    // Get the constructor's field
    let Some(ctor_node) = newtype_node.child_by_field_name("constructor") else {
        return;
    };

    // newtype_constructor has a `field` field
    if let Some(field_node) = ctor_node.child_by_field_name("field") {
        if field_node.kind() == "record" {
            // Record-style newtype: `newtype X = X { unX :: Int }`
            let mut ref_seen = std::collections::HashSet::new();
            process_record_fields(
                field_node,
                content,
                newtype_type_id,
                &qualified_name,
                helper,
                &mut ref_seen,
            );
        } else {
            // Simple newtype: `newtype Wrapped = Wrapped Int`
            // The `field` node wraps a type. Try extracting the type text directly.
            // For named fields (e.g., `field` with `type` sub-field), extract both name and type.
            let type_ast_node = field_node
                .child_by_field_name("type")
                .or_else(|| field_node.child_by_field_name("parameter"));

            let field_type = type_ast_node
                .and_then(|t| extract_type_text(t, content))
                .or_else(|| extract_type_text(field_node, content));

            let name_node = field_node.child_by_field_name("name");
            let field_name = name_node
                .and_then(|n| n.utf8_text(content).ok())
                .map(|t| t.trim().to_string())
                .filter(|n| !n.is_empty());

            if let Some(type_text) = field_type {
                // Cluster C / `C_OTHER_PLUGINS`: when the simple-newtype
                // form carries a field name (e.g. record-style sugar
                // `newtype Wrapper = Wrapper { unwrap :: Int }` that
                // surfaced through this branch), emit a Constant for it
                // and source the TypeOf{Field} edge from the Constant.
                // For positional newtype fields (`newtype Wrapped =
                // Wrapped Int`) keep the legacy data-type-sourced edge
                // — there is no name to anchor a per-field node on.
                if let (Some(name), Some(name_node)) = (field_name.as_deref(), name_node) {
                    let qualified_field_name = format!("{qualified_name}.{name}");
                    let constant_id = helper.add_constant_with_static_and_visibility(
                        &qualified_field_name,
                        Some(span_from_node(name_node)),
                        false,
                        None,
                    );
                    helper.add_defines_edge(newtype_type_id, constant_id);
                    helper.add_contains_edge(newtype_type_id, constant_id);

                    let type_id = helper.add_type(&type_text, None);
                    helper.add_typeof_edge_with_context(
                        constant_id,
                        type_id,
                        Some(TypeOfContext::Field),
                        Some(0),
                        Some(name),
                    );
                    let ref_node = type_ast_node.unwrap_or(field_node);
                    emit_references_edges(constant_id, ref_node, None, content, helper);
                } else {
                    let type_id = helper.add_type(&type_text, None);
                    helper.add_typeof_edge_with_context(
                        newtype_type_id,
                        type_id,
                        Some(TypeOfContext::Field),
                        Some(0),
                        field_name.as_deref(),
                    );
                    // References edges from newtype to wrapped type constructors
                    let ref_node = type_ast_node.unwrap_or(field_node);
                    emit_references_edges(newtype_type_id, ref_node, None, content, helper);
                }
            }
        }
    }
}

/// Process a type synonym declaration.
/// `type Alias = Int`
/// Note: tree-sitter-haskell v0.23.1 uses `type_synomym` (grammar typo).
fn process_type_synonym(
    syn_node: Node<'_>,
    content: &[u8],
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    // Extract synonym name
    let Some(name_node) = syn_node.child_by_field_name("name") else {
        return;
    };
    let Some(type_name) = name_node.utf8_text(content).ok() else {
        return;
    };
    let type_name = type_name.trim();
    if type_name.is_empty() {
        return;
    }

    let qualified_name = if let Some(module) = module_name {
        format!("{module}.{type_name}")
    } else {
        type_name.to_string()
    };

    let span = Some(Span::from_bytes(syn_node.start_byte(), syn_node.end_byte()));
    let alias_id = helper.add_type(&qualified_name, span);

    // Extract target type
    let Some(type_node) = syn_node.child_by_field_name("type") else {
        return;
    };
    let Some(target_text) = extract_type_text(type_node, content) else {
        return;
    };

    let target_id = helper.add_type(&target_text, None);
    helper.add_typeof_edge_with_context(
        alias_id,
        target_id,
        Some(TypeOfContext::TypeParameter),
        None,
        None,
    );

    // References edges from type synonym to all type constructors in RHS
    emit_references_edges(alias_id, type_node, None, content, helper);
}

/// Process typeclass declarations to extract method signature `TypeOf` edges.
/// `class Run a where run :: a -> Int`
fn process_class_declarations(
    class_node: Node<'_>,
    content: &[u8],
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    // Extract class name
    let class_name = class_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .map(|t| t.trim().to_string());
    let Some(class_name) = class_name else {
        return;
    };

    // Get declarations
    let Some(decls_node) = class_node.child_by_field_name("declarations") else {
        return;
    };

    // Recursively find signature nodes inside declarations
    // Path: class_declarations → class_decl (supertype) → decl (supertype) → signature
    find_and_process_class_signatures(
        decls_node,
        content,
        &class_name,
        context_to_node,
        module_name,
        helper,
    );
}

/// Recursively walk class declaration children to find `signature` nodes.
/// Handles the supertype chain: `class_decl → decl → signature`.
fn find_and_process_class_signatures(
    node: Node<'_>,
    content: &[u8],
    class_name: &str,
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    if node.kind() == "signature" {
        // Found a method signature — process it
        process_class_method_signature(
            node,
            content,
            class_name,
            context_to_node,
            module_name,
            helper,
        );
        return;
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        find_and_process_class_signatures(
            child,
            content,
            class_name,
            context_to_node,
            module_name,
            helper,
        );
    }
}

/// Process a typeclass method signature and create `TypeOf` and `References` edges.
/// The method name is qualified with the class name (e.g., `Run.run`).
fn process_class_method_signature(
    sig_node: Node<'_>,
    content: &[u8],
    class_name: &str,
    context_to_node: &HashMap<String, NodeId>,
    module_name: Option<&str>,
    helper: &mut GraphBuildHelper,
) {
    let method_names = extract_signature_names(sig_node, content);
    if method_names.is_empty() {
        return;
    }

    let Some(type_node) = sig_node.child_by_field_name("type") else {
        return;
    };

    // Unwrap forall/parens/context layers using shared helper
    let unwrapped = unwrap_signature_type(type_node, sig_node, content);

    let (param_types, return_type) = flatten_function_type(unwrapped.actual_type_node, content);

    for method_name in &method_names {
        // Build qualified method name: ClassName.methodName
        let qualified_method = if let Some(module) = module_name {
            format!("{module}.{class_name}.{method_name}")
        } else {
            format!("{class_name}.{method_name}")
        };

        // Resolve method ID: prefer qualified name to avoid collisions with top-level functions.
        // Fall back to creating a new function node if not found.
        let method_id = if let Some(&id) = context_to_node.get(&qualified_method) {
            id
        } else {
            // Method may not have a separate definition — create a function node
            let span = Some(Span::from_bytes(sig_node.start_byte(), sig_node.end_byte()));
            helper.add_function(&qualified_method, span, false, false)
        };

        // Parameter edges
        for (idx, param_type) in param_types.iter().enumerate() {
            let type_id = helper.add_type(param_type, None);
            #[allow(clippy::cast_possible_truncation)]
            helper.add_typeof_edge_with_context(
                method_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(idx as u16),
                None,
            );
        }

        // Return type edge
        if !return_type.is_empty() {
            let return_type_id = helper.add_type(&return_type, None);
            helper.add_typeof_edge_with_context(
                method_id,
                return_type_id,
                Some(TypeOfContext::Return),
                Some(0),
                None,
            );
        }

        // Constraint edge
        if let Some(ref constraint) = unwrapped.constraint_text {
            let constraint_type_id = helper.add_type(constraint, None);
            helper.add_typeof_edge_with_context(
                method_id,
                constraint_type_id,
                Some(TypeOfContext::Constraint),
                None,
                None,
            );
        }

        // References edges to all individual type constructors (deduplicated)
        emit_references_edges(
            method_id,
            unwrapped.actual_type_node,
            unwrapped.constraint_node,
            content,
            helper,
        );
    }
}

// ============================================================================
// AST Graph - Tracks callable contexts
// ============================================================================

#[derive(Debug)]
struct ASTGraph {
    contexts: Vec<CallContext>,
    node_to_context: HashMap<usize, usize>,
}

impl ASTGraph {
    fn from_tree(tree: &Tree, content: &[u8], _max_depth: usize) -> Result<Self, String> {
        let mut contexts = Vec::new();
        let mut node_to_context = HashMap::new();

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        // Extract function definitions by traversing the tree
        // Haskell top-level functions: signature + binding
        // Pattern: functionName :: Type
        //          functionName args = body
        let root = tree.root_node();
        extract_functions_recursive(
            root,
            content,
            &mut contexts,
            &mut node_to_context,
            &mut guard,
        )?;

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

/// Recursively extract function definitions from AST
///
/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
fn extract_functions_recursive(
    node: Node<'_>,
    content: &[u8],
    contexts: &mut Vec<CallContext>,
    node_to_context: &mut HashMap<usize, usize>,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), String> {
    guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    match node.kind() {
        // Function definition: calculate x y = body
        "function" => {
            // First child should be the function name (variable)
            if let Some(name) = extract_function_name_from_function(node, content) {
                let context_idx = contexts.len();
                contexts.push(CallContext {
                    qualified_name: name,
                    span: (node.start_byte(), node.end_byte()),
                });

                // Map all descendant nodes to this context
                map_descendants_to_context(&node, context_idx, node_to_context);
            }
        }
        // Bind definition: main = body
        "bind" => {
            // First child should be the variable name
            if let Some(name) = extract_function_name_from_bind(node, content) {
                let context_idx = contexts.len();
                contexts.push(CallContext {
                    qualified_name: name,
                    span: (node.start_byte(), node.end_byte()),
                });

                // Map all descendant nodes to this context
                map_descendants_to_context(&node, context_idx, node_to_context);
            }
        }
        // Process children
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_functions_recursive(child, content, contexts, node_to_context, guard)?;
            }
        }
    }

    guard.exit();
    Ok(())
}

/// Extract function name from a function node
fn extract_function_name_from_function(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable"
            && let Ok(name) = child.utf8_text(content)
        {
            return Some(name.to_string());
        }
    }
    None
}

/// Extract function name from a bind node
fn extract_function_name_from_bind(node: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable"
            && let Ok(name) = child.utf8_text(content)
        {
            return Some(name.to_string());
        }
    }
    None
}

/// Recursively map all descendant nodes to a context index
fn map_descendants_to_context(node: &Node, context_idx: usize, map: &mut HashMap<usize, usize>) {
    map.insert(node.id(), context_idx);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        map_descendants_to_context(&child, context_idx, map);
    }
}

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: (usize, usize),
}

impl CallContext {
    #[allow(dead_code)] // Reserved for future context queries
    fn qualified_name(&self) -> String {
        self.qualified_name.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::StringId;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::Path;

    /// Helper to extract Import edges from staging operations
    fn extract_import_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Imports { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    /// Helper to build a `StringId` → String map from staged `InternString` operations.
    /// This allows tests to assert the exact alias values.
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

    /// Helper to resolve a `StringId` to its string value using the staging operations.
    fn resolve_alias(
        alias: Option<&StringId>,
        string_map: &HashMap<StringId, String>,
    ) -> Option<String> {
        alias.and_then(|id| string_map.get(id).cloned())
    }

    fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
        let mut nodes = HashMap::new();
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
                && let Some(name) = staging.resolve_node_display_name(Language::Haskell, entry)
            {
                nodes.insert(*node_id, (name, entry.kind));
            }
        }
        nodes
    }

    fn build_node_canonical_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
        let mut nodes = HashMap::new();
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(node_id),
            } = op
                && let Some(name) = staging.resolve_node_canonical_name(entry)
            {
                nodes.insert(*node_id, (name.to_string(), entry.kind));
            }
        }
        nodes
    }

    fn has_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
        let nodes = build_node_lookup(staging);
        nodes
            .values()
            .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
    }

    #[allow(clippy::similar_names)] // Domain variable naming is intentional
    fn has_call_edge(
        staging: &StagingGraph,
        caller: Option<&str>,
        #[allow(clippy::similar_names)] // AST node variables
        callee: &str,
        arg_count: Option<u8>,
    ) -> bool {
        let nodes = build_node_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                let UnifiedEdgeKind::Calls { argument_count, .. } = kind else {
                    continue;
                };
                if let Some(expected) = arg_count
                    && *argument_count != expected
                {
                    continue;
                }
                let source_name = nodes.get(source).map(|(name, _)| name.as_str());
                let target_name = nodes.get(target).map(|(name, _)| name.as_str());
                if target_name != Some(callee) {
                    continue;
                }
                if let Some(expected_caller) = caller {
                    if source_name == Some(expected_caller) {
                        return true;
                    }
                } else {
                    return true;
                }
            }
        }
        false
    }

    fn parse_haskell(source: &str) -> (Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_haskell::LANGUAGE.into())
            .expect("Failed to load Haskell grammar");

        let content = source.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    fn print_tree(node: Node, source: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let text = node.utf8_text(source).unwrap_or("<invalid>");
        let text_preview = if text.len() > 50 {
            format!("{}...", &text[..50])
        } else {
            text.to_string()
        };

        eprintln!("{}{}  {:?}", indent, node.kind(), text_preview);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_tree(child, source, depth + 1);
        }
    }

    #[test]
    #[ignore = "Debug-only test for AST visualization - use in development only"]
    fn test_debug_ast() {
        let source = r"
import qualified Data.Map as M
        ";

        let (tree, content) = parse_haskell(source);
        eprintln!("\n=== AST Structure ===");
        print_tree(tree.root_node(), &content, 0);
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = r"
calculate :: Int -> Int -> Int
calculate x y = x + y
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "calculate", NodeKind::Function),
            "Expected to find 'calculate' function"
        );
    }

    #[test]
    fn test_function_application() {
        let source = r"
add :: Int -> Int -> Int
add x y = x + y

main :: IO ()
main = print (add 10 20)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_call_edge(&staging, Some("main"), "add", None),
            "Expected call edge from main to add"
        );
    }

    #[test]
    fn test_qualified_call() {
        let source = r"
import qualified Data.Text as T

process :: String -> Text
process input = T.pack input
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let nodes = build_node_lookup(&staging);
        let has_pack_call = staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge { target, kind, .. } = op {
                if !matches!(kind, UnifiedEdgeKind::Calls { .. }) {
                    return false;
                }
                let target_name = nodes.get(target).map(|(name, _)| name.as_str());
                return target_name.is_some_and(|name| name.contains("pack"));
            }
            false
        });
        assert!(has_pack_call, "Expected call edge to qualified pack");
    }

    #[test]
    fn test_operator_application() {
        let source = r"
sum :: Int -> Int -> Int
sum a b = a + b

difference :: Int -> Int -> Int
difference a b = (-) a b
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            staging.operations().iter().any(|op| {
                if let StagingOp::AddEdge { kind, .. } = op {
                    return matches!(
                        kind,
                        UnifiedEdgeKind::Calls {
                            argument_count: 2,
                            ..
                        }
                    );
                }
                false
            }),
            "Expected binary operator call"
        );
    }

    #[test]
    fn test_argument_counting() {
        let source = r"
calculate :: Int -> Int -> Int -> Int
calculate x y z = x + y + z

main :: IO ()
main = print (calculate 1 2 3)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_call_edge(&staging, None, "calculate", Some(3)),
            "Expected call with 3 arguments to calculate"
        );
    }

    #[test]
    fn test_zero_argument_call() {
        let source = r"
getValue :: Int
getValue = 42

main :: IO ()
main = print getValue
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "getValue", NodeKind::Function),
            "Expected to find 'getValue' function"
        );
    }

    #[test]
    fn test_module_header_creates_module_node() {
        let source = r#"
module Demo.Module where

main :: IO ()
main = print "ok"
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("Main.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "Demo.Module", NodeKind::Module),
            "Expected module node with name 'Demo.Module'"
        );
    }

    #[test]
    fn test_where_clause_local_function() {
        let source = r"
process :: Int -> Int
process x = helper x * 2
  where
    helper y = y + 1
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "process", NodeKind::Function),
            "Expected to find 'process' function"
        );
    }

    #[test]
    fn test_backtick_operator() {
        let source = r"
divide :: Int -> Int -> Int
divide x y = x `div` y
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            staging.operations().iter().any(|op| matches!(
                op,
                StagingOp::AddEdge {
                    kind: UnifiedEdgeKind::Calls { .. },
                    ..
                }
            )),
            "Expected to find operator call edges"
        );
    }

    #[test]
    fn test_partial_application_section() {
        let source = r"
addOne :: Int -> Int
addOne = (+ 1)

mulTwo :: Int -> Int
mulTwo = (2 *)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "addOne", NodeKind::Function),
            "Expected to find 'addOne' function"
        );
        assert!(
            has_node(&staging, "mulTwo", NodeKind::Function),
            "Expected to find 'mulTwo' function"
        );
    }

    #[test]
    fn test_let_binding_local_function() {
        let source = r"
compute :: Int -> Int
compute x =
  let helper y = y * 2
  in helper x + 1
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "compute", NodeKind::Function),
            "Expected to find 'compute' function"
        );
    }

    #[test]
    fn test_qualified_operator() {
        let source = r"
import qualified Data.List as L

combine :: [Int] -> [Int] -> [Int]
combine xs ys = xs L.++ ys
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_node(&staging, "combine", NodeKind::Function),
            "Expected to find 'combine' function"
        );
    }

    // ============================================================================
    // Import Edge Tests (Wave 7)
    // ============================================================================

    #[test]
    fn test_import_edge_simple() {
        let source = r"
import Data.List
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected at least one import edge"
        );

        // Simple import without explicit list should be wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { alias, is_wildcard } = edge {
            assert!(*is_wildcard, "Simple import should be wildcard");
            assert!(alias.is_none(), "Simple import should not have alias");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_import_edge_qualified() {
        let source = r"
import qualified Data.Map as M
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected qualified import edge");

        // Build string map to resolve alias values
        let string_map = build_string_map(&staging);

        // Qualified import with `as M` should have alias and NOT be wildcard
        // (qualified imports don't put symbols directly in scope, they require qualifier)
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { alias, is_wildcard } = edge {
            assert!(
                !*is_wildcard,
                "Qualified import should NOT be wildcard (requires qualifier)"
            );
            // Assert the exact alias value
            let alias_value = resolve_alias(alias.as_ref(), &string_map);
            assert_eq!(
                alias_value,
                Some("M".to_string()),
                "Qualified import alias should be 'M'"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_import_edge_with_list() {
        let source = r"
import Data.Text (pack, unpack)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Expected import edge with explicit list"
        );

        // Import with explicit list should NOT be wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(
                !*is_wildcard,
                "Import with explicit list should NOT be wildcard"
            );
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_import_edge_hiding() {
        let source = r"
import Data.Maybe hiding (fromJust)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert!(!import_edges.is_empty(), "Expected import edge with hiding");

        // Import with hiding clause should NOT be wildcard
        let edge = import_edges[0];
        if let UnifiedEdgeKind::Imports { is_wildcard, .. } = edge {
            assert!(!*is_wildcard, "Import with hiding should NOT be wildcard");
        } else {
            panic!("Expected Imports edge kind");
        }
    }

    #[test]
    fn test_multiple_imports() {
        let source = r"
module Test where

import Data.List
import qualified Data.Map as M
import Data.Text (pack, unpack)
import Data.Maybe hiding (fromJust)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let import_edges = extract_import_edges(&staging);
        assert_eq!(import_edges.len(), 4, "Expected 4 import edges");

        // Verify all are EdgeKind::Imports
        for edge in &import_edges {
            assert!(
                matches!(edge, UnifiedEdgeKind::Imports { .. }),
                "All edges should be Imports"
            );
        }
    }

    // ========================================================================
    // FFI Tests
    // ========================================================================

    /// Helper to extract FFI edges from staging graph
    fn extract_ffi_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::FfiCall { .. })
                {
                    Some(kind)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Helper to get node metadata by qualified name pattern
    fn get_node_metadata(staging: &StagingGraph, name_pattern: &str) -> Option<NodeEntry> {
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op
                && staging
                    .resolve_node_display_name(Language::Haskell, entry)
                    .is_some_and(|name| name.contains(name_pattern))
            {
                return Some(entry.clone());
            }
        }
        None
    }

    /// Helper to check if FFI edge exists with specific convention and symbol
    fn has_ffi_edge(
        staging: &StagingGraph,
        convention: FfiConvention,
        target_symbol: &str,
    ) -> bool {
        let node_lookup = build_node_canonical_lookup(staging);

        // Build expected FFI target name prefix based on convention
        let convention_str = match convention {
            FfiConvention::C => "C",
            FfiConvention::Stdcall => "stdcall",
            _ => panic!("Unsupported FFI convention in has_ffi_edge: {convention:?}"),
        };
        let ffi_prefix = format!("ffi::{convention_str}::");

        staging.operations().iter().any(|op| {
            if let StagingOp::AddEdge { target, kind, .. } = op
                && let UnifiedEdgeKind::FfiCall {
                    convention: edge_conv,
                } = kind
            {
                *edge_conv == convention
                    && node_lookup.get(target).is_some_and(|(name, _)| {
                        let Some(foreign_symbol) = name.strip_prefix(&ffi_prefix) else {
                            return false;
                        };
                        let stripped_symbol =
                            foreign_symbol.strip_prefix('&').unwrap_or(foreign_symbol);

                        // Haskell stores the raw foreign entity string in the canonical
                        // FFI target name, so normalize the semantic symbol for matching:
                        // - `&errno` -> `errno`
                        // - `stdio.h printf` -> `printf`
                        // - `math.h sin` -> `sin`
                        let normalized_symbol = stripped_symbol
                            .split_ascii_whitespace()
                            .next_back()
                            .unwrap_or(stripped_symbol);

                        normalized_symbol == target_symbol
                    })
            } else {
                false
            }
        })
    }

    #[test]
    fn test_ffi_ccall_static() {
        let source = r#"
module FFI where
import Foreign.C.Types
foreign import ccall "exp" c_exp :: Double -> Double
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert!(!ffi_edges.is_empty(), "Expected FFI edge");
        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "exp"),
            "Expected FfiCall edge to 'exp' with C convention"
        );
    }

    #[test]
    fn test_ffi_ccall_dynamic() {
        let source = r#"
module FFI where
import Foreign.Ptr
foreign import ccall "dynamic" mkFun :: FunPtr (Int -> IO ()) -> (Int -> IO ())
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "dynamic"),
            "Expected FfiCall edge to 'dynamic' with C convention"
        );
    }

    #[test]
    fn test_ffi_ccall_wrapper() {
        let source = r#"
module FFI where
import Foreign.Ptr
foreign import ccall "wrapper" createCB :: (Int -> IO ()) -> IO (FunPtr (Int -> IO ()))
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "wrapper"),
            "Expected FfiCall edge to 'wrapper' with C convention"
        );
    }

    #[test]
    fn test_ffi_ccall_address_of() {
        let source = r#"
module FFI where
import Foreign.Ptr
foreign import ccall "&errno" errno_ptr :: Ptr Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "errno"),
            "Expected FfiCall edge to '&errno' with C convention"
        );
    }

    #[test]
    fn test_ffi_stdcall() {
        let source = r#"
module FFI where
foreign import stdcall "MessageBoxA" msgBox :: Int -> IO Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::Stdcall, "MessageBoxA"),
            "Expected FfiCall edge to 'MessageBoxA' with Stdcall convention"
        );
    }

    #[test]
    fn test_ffi_capi() {
        let source = r#"
module FFI where
foreign import capi "stdio.h printf" my_printf :: IO Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "printf"),
            "Expected FfiCall edge to 'stdio.h printf' with C convention (CAPI maps to C)"
        );
    }

    #[test]
    fn test_ffi_unsafe_modifier() {
        let source = r#"
module FFI where
foreign import ccall unsafe "fast" fast :: Int -> Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Verify FFI edge exists
        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "fast"),
            "Expected FfiCall edge for unsafe FFI"
        );

        // Verify is_unsafe metadata is set correctly on the wrapper function
        let node =
            get_node_metadata(&staging, "FFI.fast").expect("Expected wrapper node 'FFI.fast'");

        assert!(
            node.is_unsafe,
            "Expected is_unsafe=true for unsafe FFI wrapper, but got is_unsafe=false"
        );
    }

    #[test]
    fn test_ffi_safe_modifier() {
        let source = r#"
module FFI where
foreign import ccall safe "blocking" blocking :: Int -> IO Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Verify FFI edge exists
        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "blocking"),
            "Expected FfiCall edge for safe FFI"
        );

        // Verify is_unsafe metadata is NOT set for safe FFI wrapper
        let node = get_node_metadata(&staging, "FFI.blocking").expect("Expected wrapper node");
        assert!(
            !node.is_unsafe,
            "Expected is_unsafe=false for safe FFI wrapper"
        );
    }

    #[test]
    fn test_ffi_multiple_declarations() {
        let source = r#"
module FFI where
foreign import ccall "exp" c_exp :: Double -> Double
foreign import ccall "log" c_log :: Double -> Double
foreign import stdcall "Win" win :: Int -> IO Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 3, "Expected 3 FFI edges");

        assert!(has_ffi_edge(&staging, FfiConvention::C, "exp"));
        assert!(has_ffi_edge(&staging, FfiConvention::C, "log"));
        assert!(has_ffi_edge(&staging, FfiConvention::Stdcall, "Win"));
    }

    #[test]
    fn test_ffi_complex_types() {
        let source = r#"
module FFI where
import Foreign.Ptr
import Foreign.C.Types
foreign import ccall "complex" cfunc :: Ptr CInt -> CSize -> IO (Ptr CDouble)
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "complex"),
            "Expected FfiCall edge despite complex types"
        );
    }

    #[test]
    fn test_ffi_mixed_conventions() {
        let source = r#"
module FFI where
foreign import ccall "printf" c_printf :: IO ()
foreign import stdcall "WinAPI" winapi :: IO ()
foreign import capi "math.h sin" capi_sin :: Double -> Double
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 3, "Expected 3 FFI edges");

        assert!(has_ffi_edge(&staging, FfiConvention::C, "printf"));
        assert!(has_ffi_edge(&staging, FfiConvention::Stdcall, "WinAPI"));
        assert!(has_ffi_edge(&staging, FfiConvention::C, "sin"));
    }

    #[test]
    fn test_no_ffi_regular_function() {
        let source = r"
module NoFFI where
regularFunc :: Int -> Int
regularFunc x = x + 1
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 0, "Expected NO FFI edges for regular code");
    }

    #[test]
    fn test_no_ffi_comment() {
        let source = r#"
module NoFFI where
-- foreign import ccall "fake" fake :: Int -> Int
regularFunc :: Int -> Int
regularFunc x = x
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Expected NO FFI edges when foreign is in comment"
        );
    }

    #[test]
    fn test_no_ffi_string_literal() {
        let source = r#"
module NoFFI where
message :: String
message = "foreign import ccall"
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(
            ffi_edges.len(),
            0,
            "Expected NO FFI edges when foreign is in string"
        );
    }

    #[test]
    fn test_ffi_empty_file() {
        let source = "";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 0, "Expected NO FFI edges in empty file");
    }

    #[test]
    fn test_ffi_multiline_signature() {
        let source = r#"
module FFI where
foreign import ccall "complex_func" complexFunc ::
    Int ->
    Double ->
    IO Int
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "complex_func"),
            "Expected FfiCall edge despite multiline signature"
        );
    }

    #[test]
    fn test_ffi_with_module_name() {
        let source = r#"
module Math.FFI where
foreign import ccall "sin" c_sin :: Double -> Double
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Check that wrapper function has qualified Haskell display name
        let has_qualified = staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                staging
                    .resolve_node_display_name(Language::Haskell, entry)
                    .is_some_and(|name| name == "Math.FFI.c_sin")
            } else {
                false
            }
        });

        assert!(has_qualified, "Expected qualified wrapper function name");
        assert!(has_ffi_edge(&staging, FfiConvention::C, "sin"));
    }

    #[test]
    #[should_panic(expected = "Unsupported FFI convention in has_ffi_edge")]
    fn test_has_ffi_edge_panics_on_unsupported_convention() {
        let staging = StagingGraph::new();
        let _ = has_ffi_edge(&staging, FfiConvention::Cdecl, "printf");
    }

    #[test]
    fn test_ffi_with_funptr() {
        let source = r#"
module FFI where
import Foreign.Ptr
foreign import ccall "signal" c_signal :: Int -> FunPtr (Int -> IO ()) -> IO (FunPtr (Int -> IO ()))
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_ffi_edge(&staging, FfiConvention::C, "signal"),
            "Expected FfiCall edge with FunPtr types"
        );
    }

    #[test]
    fn test_ffi_with_cstring() {
        let source = r#"
module FFI where
import Foreign.C.String
foreign import ccall "strlen" c_strlen :: CString -> IO Int
foreign import ccall "strcpy" c_strcpy :: CString -> CString -> IO CString
        "#;

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let ffi_edges = extract_ffi_edges(&staging);
        assert_eq!(ffi_edges.len(), 2, "Expected 2 FFI edges with CString");

        assert!(has_ffi_edge(&staging, FfiConvention::C, "strlen"));
        assert!(has_ffi_edge(&staging, FfiConvention::C, "strcpy"));
    }

    /// End-to-end test: Build a complete `CodeGraph` and verify `is_unsafe` persists
    /// through the full staging → commit → query cycle.
    #[test]
    fn test_ffi_unsafe_persists_in_code_graph() {
        use sqry_core::graph::unified::concurrent::CodeGraph;

        let source = r#"
module FFI where
foreign import ccall unsafe "fast_sqrt" fast_sqrt :: Double -> Double
        "#;

        // 1. Parse and build staging graph
        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // 2. Build a complete CodeGraph by committing the staging graph
        let mut graph = CodeGraph::new();

        // Register file
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("test.hs"), Some(builder.language()))
            .expect("Failed to register file");

        // Apply file ID to staging operations
        staging.apply_file_id(file_id);

        // Commit strings
        let string_remap = staging
            .commit_strings(graph.strings_mut())
            .expect("Failed to commit strings");

        // Apply string remap
        staging
            .apply_string_remap(&string_remap)
            .expect("Failed to apply string remap");

        // Commit nodes
        let node_id_mapping = staging
            .commit_nodes(graph.nodes_mut())
            .expect("Failed to commit nodes");

        // Update indices (collect data first to avoid borrow conflicts)
        let index_entries: Vec<_> = node_id_mapping
            .values()
            .filter_map(|&actual_id| {
                graph.nodes().get(actual_id).map(|entry| {
                    (
                        actual_id,
                        entry.kind,
                        entry.name,
                        entry.qualified_name,
                        entry.file,
                    )
                })
            })
            .collect();

        for (node_id, kind, name, qualified_name, file) in index_entries {
            graph
                .indices_mut()
                .add(node_id, kind, name, qualified_name, file);
        }

        // Commit edges (simplified - we don't need edges for this test)
        // The edges are committed in the entrypoint, but we only care about node metadata here

        // 3. Query the graph for the FFI wrapper node and verify is_unsafe=true
        // The wrapper function should be named "FFI.fast_sqrt"
        let wrapper_name_str = "fast_sqrt";

        // Find the node by searching through all nodes
        let mut found_unsafe_node = false;
        for (node_id, entry) in graph.nodes().iter() {
            if let Some(name) = graph.strings().resolve(entry.name)
                && name.contains(wrapper_name_str)
                && entry.is_unsafe
            {
                found_unsafe_node = true;
                eprintln!(
                    "Found unsafe FFI wrapper node: id={:?} name={} is_unsafe={}",
                    node_id, name, entry.is_unsafe
                );
                break;
            }
        }

        assert!(
            found_unsafe_node,
            "Expected to find wrapper node with is_unsafe=true in committed CodeGraph"
        );
    }

    // ========================================================================
    // TypeOf Edge Tests
    // ========================================================================

    use sqry_core::graph::unified::edge::kind::TypeOfContext;

    /// Helper to check if a `TypeOf` edge exists with the expected context, index, and name.
    /// `source_name` uses substring matching (e.g., `Some("foo")` matches `"Module.foo"`).
    /// For exact matching, use `has_typeof_edge_exact`.
    fn has_typeof_edge(
        staging: &StagingGraph,
        source_name: Option<&str>,
        target_type: &str,
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<&str>,
    ) -> bool {
        let nodes = build_node_lookup(staging);
        let string_map = build_string_map(staging);

        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                let UnifiedEdgeKind::TypeOf {
                    context: edge_ctx,
                    index: edge_idx,
                    name: edge_name,
                } = kind
                else {
                    continue;
                };

                // Check context
                if *edge_ctx != context {
                    continue;
                }

                // Check index
                if *edge_idx != index {
                    continue;
                }

                // Check name
                let resolved_name = edge_name.and_then(|id| string_map.get(&id).cloned());
                let expected_name = name.map(String::from);
                if resolved_name != expected_name {
                    continue;
                }

                // Check target type name
                let target_name = nodes.get(target).map(|(n, _)| n.as_str());
                if !target_name.is_some_and(|n| n.contains(target_type)) {
                    continue;
                }

                // Check source name (if specified)
                if let Some(expected_source) = source_name {
                    let src_name = nodes.get(source).map(|(n, _)| n.as_str());
                    if !src_name.is_some_and(|n| n.contains(expected_source)) {
                        continue;
                    }
                }

                return true;
            }
        }
        false
    }

    /// Like `has_typeof_edge` but uses exact source name matching (not substring).
    /// Use this for negative assertions to avoid false positives with qualified names.
    fn has_typeof_edge_exact(
        staging: &StagingGraph,
        source_name: &str,
        target_type: &str,
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<&str>,
    ) -> bool {
        let nodes = build_node_lookup(staging);
        let string_map = build_string_map(staging);

        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                let UnifiedEdgeKind::TypeOf {
                    context: edge_ctx,
                    index: edge_idx,
                    name: edge_name,
                } = kind
                else {
                    continue;
                };

                if *edge_ctx != context {
                    continue;
                }
                if *edge_idx != index {
                    continue;
                }

                let resolved_name = edge_name.and_then(|id| string_map.get(&id).cloned());
                let expected_name = name.map(String::from);
                if resolved_name != expected_name {
                    continue;
                }

                let target_name = nodes.get(target).map(|(n, _)| n.as_str());
                if !target_name.is_some_and(|n| n.contains(target_type)) {
                    continue;
                }

                // Exact source name match
                let src_name = nodes.get(source).map(|(n, _)| n.as_str());
                if src_name.is_some_and(|n| n == source_name) {
                    return true;
                }
            }
        }
        false
    }

    /// Like `has_typeof_edge_exact` but uses exact matching on both source name
    /// and target type name. Use this for negative assertions where substring
    /// matching on either side could produce false positives.
    fn has_typeof_edge_full_exact(
        staging: &StagingGraph,
        source_name: &str,
        target_type: &str,
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<&str>,
    ) -> bool {
        let nodes = build_node_lookup(staging);
        let string_map = build_string_map(staging);

        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                let UnifiedEdgeKind::TypeOf {
                    context: edge_ctx,
                    index: edge_idx,
                    name: edge_name,
                } = kind
                else {
                    continue;
                };

                if *edge_ctx != context {
                    continue;
                }
                if *edge_idx != index {
                    continue;
                }

                let resolved_name = edge_name.and_then(|id| string_map.get(&id).cloned());
                let expected_name = name.map(String::from);
                if resolved_name != expected_name {
                    continue;
                }

                // Exact target type match
                let target_name = nodes.get(target).map(|(n, _)| n.as_str());
                if target_name.is_none_or(|n| n != target_type) {
                    continue;
                }

                // Exact source name match
                let src_name = nodes.get(source).map(|(n, _)| n.as_str());
                if src_name.is_some_and(|n| n == source_name) {
                    return true;
                }
            }
        }
        false
    }

    /// Helper to count `TypeOf` edges in staging graph
    fn count_typeof_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: UnifiedEdgeKind::TypeOf { .. },
                        ..
                    }
                )
            })
            .count()
    }

    #[test]
    fn test_typeof_simple_signature() {
        let source = r"
foo :: Int -> Int
foo x = x + 1
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("foo"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, idx=0) for foo"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("foo"),
                "Int",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(Int, idx=0) for foo"
        );
    }

    #[test]
    fn test_typeof_multi_param() {
        let source = r"
calc :: Int -> String -> Bool
calc x y = True
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("calc"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, idx=0)"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("calc"),
                "String",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(String, idx=1)"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("calc"),
                "Bool",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(Bool, idx=0)"
        );
    }

    #[test]
    fn test_typeof_no_params() {
        let source = r"
value :: Int
value = 42
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Should have Return(Int) only, no Parameter edges
        assert!(
            has_typeof_edge(
                &staging,
                Some("value"),
                "Int",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(Int) for value"
        );
        assert!(
            !has_typeof_edge(
                &staging,
                Some("value"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Should NOT have Parameter edge for non-function signature"
        );
    }

    #[test]
    fn test_typeof_data_record_fields() {
        let source = r"
data Rec = Rec { name :: String, age :: Int }
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("Rec"),
                "String",
                Some(TypeOfContext::Field),
                Some(0),
                Some("name")
            ),
            "Expected Field(String, idx=0, name='name')"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Rec"),
                "Int",
                Some(TypeOfContext::Field),
                Some(1),
                Some("age")
            ),
            "Expected Field(Int, idx=1, name='age')"
        );
    }

    #[test]
    fn test_typeof_data_prefix_constructor() {
        let source = r"
data Wrapper = Wrap Int String
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("Wrapper"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, idx=0) for prefix constructor"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Wrapper"),
                "String",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(String, idx=1) for prefix constructor"
        );
    }

    #[test]
    fn test_typeof_newtype() {
        let source = r"
newtype Wrapped = Wrapped Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("Wrapped"),
                "Int",
                Some(TypeOfContext::Field),
                Some(0),
                None
            ),
            "Expected Field(Int, idx=0) for newtype"
        );
    }

    #[test]
    fn test_typeof_type_synonym() {
        let source = r"
type Alias = Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("Alias"),
                "Int",
                Some(TypeOfContext::TypeParameter),
                None,
                None
            ),
            "Expected TypeParameter(Int) for type synonym"
        );
    }

    #[test]
    fn test_typeof_class_method() {
        let source = r"
class Run a where
  run :: a -> Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("run"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, idx=0) for class method"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("run"),
                "Int",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(Int) for class method"
        );
    }

    #[test]
    fn test_typeof_complex_types() {
        let source = r"
process :: IO String -> Maybe Int
process x = Nothing
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // IO String should be preserved as full compound type text
        assert!(
            has_typeof_edge(
                &staging,
                Some("process"),
                "IO String",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter('IO String') for complex type"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("process"),
                "Maybe Int",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return('Maybe Int') for complex type"
        );
    }

    #[test]
    fn test_typeof_qualified_module() {
        let source = r"
module Demo where

demo :: Int -> Int
demo x = x
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Function should be qualified as Demo.demo
        assert!(
            has_typeof_edge(
                &staging,
                Some("Demo.demo"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter edge for module-qualified function"
        );
    }

    #[test]
    fn test_typeof_no_edges_without_signature() {
        let source = r"
bar = 42
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        let typeof_count = count_typeof_edges(&staging);
        assert_eq!(
            typeof_count, 0,
            "Expected no TypeOf edges for function without signature"
        );
    }

    #[test]
    fn test_typeof_multi_name_signature() {
        let source = r"
foo, bar :: Int -> Int
foo x = x + 1
bar x = x - 1
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Both foo and bar should get TypeOf edges
        assert!(
            has_typeof_edge(
                &staging,
                Some("foo"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int) for foo in multi-name signature"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("bar"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int) for bar in multi-name signature"
        );
    }

    #[test]
    fn test_typeof_constraint() {
        let source = r"
showIt :: Show a => a -> String
showIt x = show x
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Should have a Constraint edge for "Show a"
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for constrained signature"
        );
        // Should also have regular Parameter/Return edges
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for constrained signature"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for constrained signature"
        );
    }

    #[test]
    fn test_typeof_multi_field_name() {
        let source = r"
data Point = Point { x, y :: Int }
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        assert!(
            has_typeof_edge(
                &staging,
                Some("Point"),
                "Int",
                Some(TypeOfContext::Field),
                Some(0),
                Some("x")
            ),
            "Expected Field(Int, name='x') for multi-field-name record"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Point"),
                "Int",
                Some(TypeOfContext::Field),
                Some(1),
                Some("y")
            ),
            "Expected Field(Int, name='y') for multi-field-name record"
        );
    }

    #[test]
    fn test_typeof_infix_constructor() {
        let source = r"
data IntPair = Int :+: Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Infix constructor should produce Parameter edges for both operands
        assert!(
            has_typeof_edge(
                &staging,
                Some("IntPair"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, idx=0) for left infix operand"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("IntPair"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(Int, idx=1) for right infix operand"
        );
    }

    #[test]
    fn test_typeof_linear_function() {
        let source = r"
{-# LANGUAGE LinearTypes #-}
linear :: a %1 -> b -> b
linear _ y = y
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // linear_function: `a %1 -> b -> b` → Parameter(a, idx=0), Parameter(b, idx=1), Return(b, idx=0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("linear"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, idx=0) for linear function"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("linear"),
                "b",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(b, idx=1) for linear function"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("linear"),
                "b",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(b, idx=0) for linear function"
        );
    }

    #[test]
    fn test_typeof_gadt_constructor() {
        let source = r"
{-# LANGUAGE GADTs #-}
data Expr a where
  Lit :: Int -> Expr Int
  Add :: Expr Int -> Expr Int -> Expr Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // GADT constructors should produce TypeOf edges for their parameter types
        // Lit :: Int -> Expr Int — parameter Int (idx=0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("Expr"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, idx=0) from Lit constructor"
        );
        // Add :: Expr Int -> Expr Int -> Expr Int — two parameters
        assert!(
            has_typeof_edge(
                &staging,
                Some("Expr"),
                "Expr Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Expr Int, idx=0) from Add constructor"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Expr"),
                "Expr Int",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(Expr Int, idx=1) from Add constructor"
        );
        // Exactly 3 GADT TypeOf edges: Lit(Int,0) + Add(Expr Int,0) + Add(Expr Int,1)
        let typeof_count = count_typeof_edges(&staging);
        assert_eq!(
            typeof_count, 3,
            "Expected exactly 3 TypeOf edges from GADT constructors, got {typeof_count}"
        );
    }

    #[test]
    fn test_typeof_class_method_name_collision() {
        // Test that class method TypeOf edges don't collide with top-level function of same name
        let source = r"
run :: String -> IO ()
run s = putStrLn s

class Runner a where
  run :: a -> Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Top-level `run` should have Parameter(String) and Return(IO ())
        assert!(
            has_typeof_edge(
                &staging,
                Some("run"),
                "String",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(String) for top-level run"
        );
        // Top-level `run` should NOT have class method types leaked onto it.
        // Use exact matching to distinguish "run" from "Runner.run".
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "run",
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Top-level run should NOT have Parameter(a) from class method"
        );
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "run",
                "Int",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Top-level run should NOT have Return(Int) from class method"
        );
        // Class method `Runner.run` should also have edges
        assert!(
            has_typeof_edge(
                &staging,
                Some("Runner.run"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for class method Runner.run"
        );
    }

    #[test]
    fn test_typeof_constrained_class_method() {
        let source = r"
class Displayable a where
  display :: Show a => a -> String
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Should have Constraint edge for "Show a"
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for constrained class method"
        );
        // Should have Parameter(a) and Return(String)
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for constrained class method"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for constrained class method"
        );
    }

    #[test]
    fn test_typeof_module_qualified_class_collision() {
        // Test module-qualified class method naming: M.Class.method
        let source = r"
module M where

run :: String -> IO ()
run s = putStrLn s

class Runner a where
  run :: a -> Int
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // Top-level `M.run` should have Parameter(String)
        assert!(
            has_typeof_edge(
                &staging,
                Some("M.run"),
                "String",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(String) for module-qualified top-level M.run"
        );
        // Class method should be `M.Runner.run`
        assert!(
            has_typeof_edge(
                &staging,
                Some("M.Runner.run"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for module-qualified class method M.Runner.run"
        );
        // Top-level M.run should NOT have class method types
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "M.run",
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "M.run should NOT have Parameter(a) from class method"
        );
    }

    #[test]
    fn test_typeof_forall_signature() {
        // `forall a.` wraps the function type — must be unwrapped
        let source = r"
identity :: forall a. a -> a
identity x = x

constant :: forall a b. a -> b -> a
constant x _ = x
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // identity: Parameter(a, 0) + Return(a, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("identity"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for identity"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("identity"),
                "a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(a) for identity"
        );

        // constant: Parameter(a, 0) + Parameter(b, 1) + Return(a, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("constant"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for constant"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("constant"),
                "b",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(b, 1) for constant"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("constant"),
                "a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(a) for constant"
        );

        // Negative: no forall text leaking into type names
        assert!(
            !has_typeof_edge_full_exact(
                &staging,
                "identity",
                "forall a. a -> a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "identity should NOT have Return('forall a. a -> a') — forall must be unwrapped"
        );
    }

    #[test]
    fn test_typeof_forall_constraint_signature() {
        // `forall a. Show a =>` combines forall + constraint wrapping
        let source = r"
display :: forall a. Show a => a -> String
display = show

render :: forall a b. (Show a, Ord b) => a -> b -> String
render x y = show x ++ show y
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // display: Constraint(Show a) + Parameter(a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("display"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for display"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("display"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a) for display"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("display"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for display"
        );

        // render: Constraint((Show a, Ord b)) + Parameter(a, 0) + Parameter(b, 1) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("render"),
                "Show a, Ord b",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint for render with multiple constraints"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("render"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for render"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("render"),
                "b",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "Expected Parameter(b, 1) for render"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("render"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for render"
        );
    }

    #[test]
    fn test_typeof_forall_class_method() {
        // Class method with forall + constraint — tests both unwrapping paths
        let source = r"
class Container f where
  extract :: forall a. Show a => f a -> String
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // extract should be qualified as Container.extract
        assert!(
            has_typeof_edge(
                &staging,
                Some("Container.extract"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for Container.extract"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Container.extract"),
                "f a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(f a) for Container.extract"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Container.extract"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for Container.extract"
        );

        // Negative: no bare "extract" should have these edges
        assert!(
            !has_typeof_edge_full_exact(
                &staging,
                "extract",
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Bare 'extract' should NOT have Constraint — only Container.extract should"
        );
    }

    #[allow(clippy::too_many_lines)] // Haskell type extraction covers all AST forms
    #[test]
    fn test_typeof_rank2_forall_not_decomposed() {
        // Regression guard: `forall` in return position must NOT be unwrapped
        // by `flatten_function_type`. The `forall b. b -> a` return type should
        // be treated as an opaque return type, not decomposed into additional
        // parameters of `foo`.
        //
        // This also serves as a regression test for the `quantified_type` +
        // `context` grammar shape recommended by Codex iter5 review: it
        // verifies that `unwrap_quantified_type` does not inadvertently strip
        // `forall`/`context` wrappers in positions where they represent
        // meaningful type structure (rank-2 types).
        let source = r"
foo :: a -> forall b. b -> a
foo x _ = x

bar :: Int -> forall a. Show a => a -> String
bar _ = show

baz :: a -> (forall b. b -> a)
baz x _ = x
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // foo: should have exactly 1 parameter (a) and 1 return (forall b. b -> a)
        assert!(
            has_typeof_edge(
                &staging,
                Some("foo"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for foo"
        );
        // Return type should preserve the forall wrapper
        assert!(
            has_typeof_edge(
                &staging,
                Some("foo"),
                "forall b. b -> a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return('forall b. b -> a') for foo — forall in return position must not be unwrapped"
        );
        // Negative: b should NOT appear as a parameter of foo
        assert!(
            !has_typeof_edge_full_exact(
                &staging,
                "foo",
                "b",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "foo should NOT have Parameter(b, 1) — rank-2 forall must not be decomposed"
        );

        // bar: should have 1 parameter (Int) and 1 return (forall a. Show a => a -> String)
        // The constraint is inside the forall return type, NOT a top-level constraint of bar
        assert!(
            has_typeof_edge(
                &staging,
                Some("bar"),
                "Int",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(Int, 0) for bar"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("bar"),
                "forall a. Show a => a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return('forall a. Show a => a -> String') for bar"
        );
        // Negative: Show a should NOT appear as a top-level constraint of bar
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "bar",
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "bar should NOT have Constraint(Show a) — constraint is inside forall return type"
        );
        // Negative: a should NOT appear as parameter of bar
        assert!(
            !has_typeof_edge_full_exact(
                &staging,
                "bar",
                "a",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "bar should NOT have Parameter(a, 1) — rank-2 forall must not be decomposed"
        );

        // baz: parenthesized rank-2 return `(forall b. b -> a)`
        // The parens wrapper is stripped by `extract_type_text`, preserving the inner forall.
        assert!(
            has_typeof_edge(
                &staging,
                Some("baz"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for baz"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("baz"),
                "forall b. b -> a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return('forall b. b -> a') for baz — parens stripped, forall preserved"
        );
        // Negative: b should NOT appear as a parameter of baz
        assert!(
            !has_typeof_edge_full_exact(
                &staging,
                "baz",
                "b",
                Some(TypeOfContext::Parameter),
                Some(1),
                None
            ),
            "baz should NOT have Parameter(b, 1) — parenthesized rank-2 forall must not be decomposed"
        );
    }

    #[test]
    fn test_typeof_signature_type_field_is_not_quantified_type() {
        // Canary test: tree-sitter-haskell v0.23.1 defines `signature.type` as
        // `quantified_type` (a supertype), but in practice emits concrete subtypes
        // (`forall`, `context`, `function`, etc.) directly. If a future grammar
        // version starts emitting `quantified_type` as a concrete wrapper node,
        // this test will fail — signaling that `process_type_signature` needs
        // updating to handle `quantified_type` at the signature level (extract
        // nested `context` constraints before unwrapping).
        let source = r"
plain :: Int -> String
plain = show

withForall :: forall a. a -> a
withForall x = x

withConstraint :: Show a => a -> String
withConstraint = show

withBoth :: forall a. Show a => a -> String
withBoth = show
        ";

        let (tree, _content) = parse_haskell(source);
        let root = tree.root_node();

        // Collect the `type` field node kind for each signature
        let mut type_kinds = Vec::new();
        let mut cursor = root.walk();
        for decl in root
            .child_by_field_name("children")
            .unwrap_or(root)
            .children(&mut cursor)
        {
            if decl.kind() == "signature"
                && let Some(type_node) = decl.child_by_field_name("type")
            {
                type_kinds.push(type_node.kind().to_string());
            }
        }

        // Walk declarations (may be nested under haskell → declarations)
        if type_kinds.is_empty() {
            let decls = root.named_child(0).unwrap_or(root);
            let mut cursor2 = decls.walk();
            for decl in decls.children(&mut cursor2) {
                if decl.kind() == "signature"
                    && let Some(type_node) = decl.child_by_field_name("type")
                {
                    type_kinds.push(type_node.kind().to_string());
                }
            }
        }

        assert!(
            !type_kinds.is_empty(),
            "Should have found at least one signature"
        );

        // None of the signature type fields should be `quantified_type` in v0.23.1.
        // If this assertion fails, the grammar has changed and `process_type_signature`
        // needs to handle `quantified_type` at the signature level.
        for kind in &type_kinds {
            assert_ne!(
                kind, "quantified_type",
                "signature.type emitted 'quantified_type' — grammar changed! \
                 Update process_type_signature to handle quantified_type at signature level. \
                 Found kinds: {type_kinds:?}"
            );
        }

        // Verify expected concrete kinds
        assert!(
            type_kinds.contains(&"function".to_string()),
            "Expected 'function' for 'Int -> String', got: {type_kinds:?}"
        );
        assert!(
            type_kinds.contains(&"forall".to_string()),
            "Expected 'forall' for 'forall a. ...', got: {type_kinds:?}"
        );
        assert!(
            type_kinds.contains(&"context".to_string()),
            "Expected 'context' for 'Show a => ...', got: {type_kinds:?}"
        );
    }

    #[test]
    fn test_typeof_parenthesized_constrained_signature() {
        // Regression: `(Show a => a -> String)` wraps the entire constrained
        // type in parens. The `parens` must be unwrapped before the `context`
        // check so that constraint, parameters, and return type are extracted.
        // Also tests `(Show a, Ord a) => (a -> String)` where only the return
        // type is parenthesized.
        let source = r"
showIt :: (Show a => a -> String)
showIt = show

showBoth :: (Show a, Ord a) => (a -> String)
showBoth = show
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // showIt: Constraint(Show a) + Parameter(a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for showIt — parens must be unwrapped to find context"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for showIt"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showIt"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for showIt"
        );

        // showBoth: Constraint((Show a, Ord a)) + Parameter(a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("showBoth"),
                "Show a, Ord a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint for showBoth with multiple constraints"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showBoth"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for showBoth — parens on return type must be unwrapped"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showBoth"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String) for showBoth"
        );

        // Negative: no fallback opaque return edge should be emitted.
        // Without parens unwrapping, the raw text "Show a => a -> String" would
        // be emitted as an opaque return type instead of proper decomposition.
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "showIt",
                "Show a => a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "showIt should NOT have opaque Return('Show a => a -> String') — parens must be unwrapped"
        );
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "showBoth",
                "a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "showBoth should NOT have opaque Return('a -> String') — parenthesized return must be decomposed"
        );
    }

    #[test]
    fn test_typeof_parenthesized_forall_signature() {
        // Regression: `foo :: (forall a. a -> a)` has the `parens` wrapper hiding
        // the `forall` node. The unwrapping order must handle parens→forall:
        // unwrap_forall (no-op) → unwrap_parens (strips parens) → unwrap_forall
        // (now strips forall) → flatten_function_type decomposes the function.
        let source = r"
idParens :: (forall a. a -> a)
idParens = id
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // idParens: should decompose into Parameter(a, 0) + Return(a, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("idParens"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for idParens — parens→forall must be unwrapped"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("idParens"),
                "a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(a, 0) for idParens"
        );
        // Negative: should NOT have the full opaque type as return
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "idParens",
                "forall a. a -> a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "idParens should NOT have opaque Return('forall a. a -> a') — forall must be unwrapped at top level"
        );
    }

    #[test]
    fn test_typeof_parenthesized_forall_constrained_signature() {
        // Regression: top-level signature with `(forall a. Show a => a -> String)`
        // exercises the full parens → forall → context unwrapping chain.
        let source = r"
showParens :: (forall a. Show a => a -> String)
showParens = show
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // showParens: Constraint(Show a) + Parameter(a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("showParens"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for showParens — parens→forall→context chain"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showParens"),
                "a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(a, 0) for showParens"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("showParens"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String, 0) for showParens"
        );
        // Negative: no opaque return with full wrapper text
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "showParens",
                "forall a. Show a => a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "showParens should NOT have opaque Return — full chain must unwrap"
        );
    }

    #[test]
    fn test_typeof_parenthesized_forall_class_method() {
        // Regression: class method with `(forall a. a -> a)` shape tests
        // the parens→forall unwrapping in `process_class_method_signature`.
        let source = r"
class Wrapper f where
  unwrap :: (forall a. f a -> a)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // unwrap should be qualified as Wrapper.unwrap
        assert!(
            has_typeof_edge(
                &staging,
                Some("Wrapper.unwrap"),
                "f a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(f a, 0) for Wrapper.unwrap — parens→forall must be unwrapped"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Wrapper.unwrap"),
                "a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(a, 0) for Wrapper.unwrap"
        );
        // Negative: should NOT have the full opaque type as return
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "Wrapper.unwrap",
                "forall a. f a -> a",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Wrapper.unwrap should NOT have opaque Return — parens→forall must be unwrapped in class methods too"
        );
    }

    #[test]
    fn test_typeof_parenthesized_constrained_class_method() {
        // Regression: class method with `(Show a => f a -> String)` shape tests
        // the parens→context unwrapping in `process_class_method_signature`,
        // mirroring test_typeof_parenthesized_constrained_signature for symmetry.
        let source = r"
class Displayable f where
  display :: (Show a => f a -> String)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // display should be qualified as Displayable.display
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for Displayable.display — parens must be unwrapped to find context"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "f a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(f a, 0) for Displayable.display"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Displayable.display"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String, 0) for Displayable.display"
        );
        // Negative: should NOT have the opaque return with constraint text
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "Displayable.display",
                "Show a => f a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Displayable.display should NOT have opaque Return — parens must be unwrapped to expose context"
        );
    }

    #[test]
    fn test_typeof_parenthesized_forall_constrained_class_method() {
        // Regression: class method with `(forall a. Show a => f a -> String)` shape
        // exercises the full unwrapping chain: parens → forall → context.
        // Also tests multi-constraint with parenthesized return:
        // `(Show a, Ord a) => (f a -> String)`.
        let source = r"
class Formatter f where
  format :: (forall a. Show a => f a -> String)
  formatOrd :: (Show a, Ord a) => (f a -> String)
        ";

        let (tree, content) = parse_haskell(source);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();

        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();

        // format: Constraint(Show a) + Parameter(f a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.format"),
                "Show a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected Constraint(Show a) for Formatter.format — parens→forall→context chain"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.format"),
                "f a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(f a, 0) for Formatter.format"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.format"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String, 0) for Formatter.format"
        );
        // Negative: no opaque return with full wrapper text
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "Formatter.format",
                "forall a. Show a => f a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Formatter.format should NOT have opaque Return — full chain must unwrap"
        );

        // formatOrd: Constraint((Show a, Ord a)) + Parameter(f a, 0) + Return(String, 0)
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.formatOrd"),
                "Show a, Ord a",
                Some(TypeOfContext::Constraint),
                None,
                None
            ),
            "Expected multi-Constraint for Formatter.formatOrd"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.formatOrd"),
                "f a",
                Some(TypeOfContext::Parameter),
                Some(0),
                None
            ),
            "Expected Parameter(f a, 0) for Formatter.formatOrd — parenthesized return decomposed"
        );
        assert!(
            has_typeof_edge(
                &staging,
                Some("Formatter.formatOrd"),
                "String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Expected Return(String, 0) for Formatter.formatOrd"
        );
        // Negative: no opaque return with parenthesized function text
        assert!(
            !has_typeof_edge_exact(
                &staging,
                "Formatter.formatOrd",
                "f a -> String",
                Some(TypeOfContext::Return),
                Some(0),
                None
            ),
            "Formatter.formatOrd should NOT have opaque Return('f a -> String') — parens return must decompose"
        );
    }

    // ========================================================================
    // References edge helpers and tests
    // ========================================================================

    /// Check whether a `References` edge exists from source to target type.
    fn has_reference_edge(staging: &StagingGraph, source_name: &str, target_type: &str) -> bool {
        let nodes = build_node_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                if !matches!(kind, UnifiedEdgeKind::References) {
                    continue;
                }
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

    /// Count the number of `References` edges from a given source name.
    fn count_reference_edges(staging: &StagingGraph, source_name: &str) -> usize {
        let nodes = build_node_lookup(staging);
        staging
            .operations()
            .iter()
            .filter(|op| {
                if let StagingOp::AddEdge {
                    source,
                    kind: UnifiedEdgeKind::References,
                    ..
                } = op
                {
                    nodes
                        .get(source)
                        .is_some_and(|(n, _)| n.contains(source_name))
                } else {
                    false
                }
            })
            .count()
    }

    #[test]
    fn test_references_simple_signature() {
        let code = r"
module Ref where
foo :: Int -> Int
foo x = x
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(
            has_reference_edge(&staging, "foo", "Int"),
            "foo should reference Int"
        );
        // Int appears twice in signature but should produce only one References edge
        assert_eq!(
            count_reference_edges(&staging, "foo"),
            1,
            "Deduplication: Int -> Int should produce exactly 1 References edge"
        );
    }

    #[test]
    fn test_references_multi_param() {
        let code = r"
module Ref where
calc :: Int -> String -> Bool
calc x y = True
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "calc", "Int"));
        assert!(has_reference_edge(&staging, "calc", "String"));
        assert!(has_reference_edge(&staging, "calc", "Bool"));
        assert_eq!(count_reference_edges(&staging, "calc"), 3);
    }

    #[test]
    fn test_references_complex_type() {
        let code = r"
module Ref where
proc :: IO String -> Maybe Int -> Bool
proc x y = True
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "proc", "IO"));
        assert!(has_reference_edge(&staging, "proc", "String"));
        assert!(has_reference_edge(&staging, "proc", "Maybe"));
        assert!(has_reference_edge(&staging, "proc", "Int"));
        assert!(has_reference_edge(&staging, "proc", "Bool"));
        assert_eq!(count_reference_edges(&staging, "proc"), 5);
    }

    #[test]
    fn test_references_no_type_vars() {
        let code = r"
module Ref where
identity :: a -> a
identity x = x
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert_eq!(
            count_reference_edges(&staging, "identity"),
            0,
            "Type variables should not produce References edges"
        );
    }

    #[test]
    fn test_references_constraint() {
        let code = r"
module Ref where
showIt :: Show a => a -> String
showIt x = show x
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "showIt", "Show"));
        assert!(has_reference_edge(&staging, "showIt", "String"));
        assert_eq!(count_reference_edges(&staging, "showIt"), 2);
    }

    #[test]
    fn test_references_multi_constraint() {
        let code = r"
module Ref where
f :: (Show a, Ord a) => a -> String
f x = show x
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "f", "Show"));
        assert!(has_reference_edge(&staging, "f", "Ord"));
        assert!(has_reference_edge(&staging, "f", "String"));
        assert_eq!(count_reference_edges(&staging, "f"), 3);
    }

    #[test]
    fn test_references_qualified_type() {
        let code = r"
module Ref where
f :: Data.Map.Map String Int -> Bool
f x = True
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "f", "Data.Map.Map"));
        assert!(has_reference_edge(&staging, "f", "String"));
        assert!(has_reference_edge(&staging, "f", "Int"));
        assert!(has_reference_edge(&staging, "f", "Bool"));
        assert_eq!(count_reference_edges(&staging, "f"), 4);
    }

    #[test]
    fn test_references_data_record() {
        let code = r"
module Ref where
data Rec = Rec { name :: String, age :: Int }
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Rec", "String"));
        assert!(has_reference_edge(&staging, "Rec", "Int"));
    }

    #[test]
    fn test_references_data_prefix() {
        let code = r"
module Ref where
data Wrapper = Wrapper Int String
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Wrapper", "Int"));
        assert!(has_reference_edge(&staging, "Wrapper", "String"));
    }

    #[test]
    fn test_references_newtype() {
        let code = r"
module Ref where
newtype Wrapped = Wrapped Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Wrapped", "Int"));
    }

    #[test]
    fn test_references_type_synonym() {
        let code = r"
module Ref where
type Table = Map String Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Table", "Map"));
        assert!(has_reference_edge(&staging, "Table", "String"));
        assert!(has_reference_edge(&staging, "Table", "Int"));
    }

    #[test]
    fn test_references_class_method() {
        let code = r"
module Ref where
class Container f where
  extract :: f a -> Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Container.extract", "Int"));
    }

    #[test]
    fn test_references_no_edges_without_sig() {
        let code = r"
module Ref where
bar = 42
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert_eq!(
            count_reference_edges(&staging, "bar"),
            0,
            "Functions without type signatures should not have References edges"
        );
    }

    #[test]
    fn test_references_forall() {
        let code = r"
module Ref where
foo :: forall a. a -> Int
foo x = 42
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "foo", "Int"));
        assert_eq!(count_reference_edges(&staging, "foo"), 1);
    }

    #[test]
    fn test_references_gadt() {
        let code = r"
module Ref where
data Expr a where
  Lit :: Int -> Expr Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Expr", "Int"));
    }

    #[test]
    fn test_references_strict_lazy_fields() {
        let code = r"
module Ref where
data Strict = Strict !Int ~String
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Strict", "Int"));
        assert!(has_reference_edge(&staging, "Strict", "String"));
    }

    #[test]
    fn test_references_rank2_boundary() {
        // Rank-2 forall in return position: `Int -> forall a. Show a => a -> String`
        // TypeOf treats the forall return as opaque, but References extracts ALL
        // type constructors from the full signature type node, including those
        // inside the rank-2 return. This is correct behavior.
        let code = r"
module Ref where
foo :: Int -> forall a. Show a => a -> String
foo x = show
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(
            has_reference_edge(&staging, "foo", "Int"),
            "Int parameter should be referenced"
        );
        assert!(
            has_reference_edge(&staging, "foo", "Show"),
            "Show from rank-2 constraint should be referenced"
        );
        assert!(
            has_reference_edge(&staging, "foo", "String"),
            "String from rank-2 return should be referenced"
        );
        assert_eq!(
            count_reference_edges(&staging, "foo"),
            3,
            "Expected exactly 3 References edges: Int, Show, String"
        );
    }

    #[test]
    fn test_references_dedup_repeated_type() {
        // Same type appears in multiple positions; exactly one References edge per unique type
        let code = r"
module Ref where
swap :: (Int, Int) -> (Int, Int)
swap (a, b) = (b, a)
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "swap", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "swap"),
            1,
            "Int appears 4 times but should produce exactly 1 References edge"
        );
    }

    #[test]
    fn test_references_dedup_record_shared_type() {
        // Multiple record fields with the same type should produce one References edge
        let code = r"
module Ref where
data Rec = Rec { name :: Int, age :: Int }
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Rec", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "Rec"),
            1,
            "Record with two Int fields should produce exactly 1 Rec -> Int References edge"
        );
    }

    #[test]
    fn test_references_dedup_prefix_shared_type() {
        // Multiple prefix constructor fields with same type
        let code = r"
module Ref where
data Pair = Pair Int Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "Pair", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "Pair"),
            1,
            "Prefix constructor with two Int fields should produce exactly 1 References edge"
        );
    }

    #[test]
    fn test_references_dedup_multi_constructor_prefix() {
        // Multiple constructors of the same data type sharing a type should produce one edge
        // data T = A Int | B Int  →  exactly one T -> Int References edge
        let code = r"
module Ref where
data T = A Int | B Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "T", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "T"),
            1,
            "data T = A Int | B Int should produce exactly 1 T -> Int References edge"
        );
    }

    #[test]
    fn test_references_dedup_multi_constructor_record() {
        // Multiple record constructors sharing the same type
        // data T = A { x :: Int } | B { y :: Int }  →  exactly one T -> Int References edge
        let code = r"
module Ref where
data T = A { x :: Int } | B { y :: Int }
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "T", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "T"),
            1,
            "data T = A {{ x :: Int }} | B {{ y :: Int }} should produce exactly 1 T -> Int References edge"
        );
    }

    #[test]
    fn test_references_dedup_multi_constructor_gadt() {
        // Multi-constructor GADT with repeated types across constructors
        // data E a where Lit :: Int -> E Int; Add :: Int -> Int -> E Int
        // Int appears in both constructors → exactly one E -> Int edge (dedup)
        // E appears in return types → one E -> E self-reference edge (GADTs always self-ref)
        let code = r"
module Ref where
data E a where
  Lit :: Int -> E Int
  Add :: Int -> Int -> E Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "E", "Int"));
        // E self-reference from GADT return types (Int -> E Int)
        assert!(has_reference_edge(&staging, "E", "E"));
        assert_eq!(
            count_reference_edges(&staging, "E"),
            2,
            "GADT: 1 for Int (deduped across constructors) + 1 for E (self-ref in return type)"
        );
    }

    #[test]
    fn test_references_dedup_multi_constructor_infix() {
        // Multiple infix constructors sharing the same type across constructors
        // data T = Int `A` Int | Int `B` Int  →  exactly one T -> Int References edge
        let code = r"
module Ref where
data T = Int `A` Int | Int `B` Int
";
        let (tree, content) = parse_haskell(code);
        let mut staging = StagingGraph::new();
        let builder = HaskellGraphBuilder::default();
        builder
            .build_graph(&tree, &content, Path::new("test.hs"), &mut staging)
            .unwrap();
        assert!(has_reference_edge(&staging, "T", "Int"));
        assert_eq!(
            count_reference_edges(&staging, "T"),
            1,
            "data T = Int `A` Int | Int `B` Int should produce exactly 1 T -> Int References edge"
        );
    }
}

/// Per-language [`ShapeMapping`] for Haskell (identifier-blind body-shape
/// feature).
///
/// Precomputed `kind_id -> CfBucket` table built once from the tree-sitter-haskell
/// grammar. Haskell control flow is expression-based: `if`/then/else is a
/// `conditional`, guards are `guards`, `case ... of` plus its `alternative` arms
/// are pattern matches, lambdas are closures, list comprehensions carry a
/// `generator` per `<-` qualifier, and `let`/`where` bindings (`bind`) are the
/// binding form. `error`/`throw` are ordinary applications, so they stay in the
/// `Call` bucket.
pub struct HaskellShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl HaskellShapeMapping {
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_haskell::LANGUAGE.into();
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
                *slot = cf_bucket_for_haskell_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for HaskellShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        // A function-equation declaration (`name p1 p2 ... = body`) exposes its
        // formal parameters through the `patterns` field; each named child is one
        // positional argument. Type-signature `function` arrow nodes have no
        // `patterns` field, so they read as the default empty signature.
        let mut shape = SignatureShape::default();
        if let Some(patterns) = fn_node.child_by_field_name("patterns") {
            let mut cursor = patterns.walk();
            let count = patterns.named_children(&mut cursor).count();
            shape.arity_positional = u16::try_from(count).unwrap_or(u16::MAX);
        }
        shape
    }
}

/// Map one tree-sitter-haskell node-kind name to its canonical control-flow
/// bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_haskell_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "conditional" | "guards" | "guard" => CfBucket::Branch,
        "case" | "alternative" | "multi_way_if" => CfBucket::Match,
        // Each `<-` qualifier of a list comprehension is a generator loop.
        "generator" => CfBucket::Loop,
        "list_comprehension" => CfBucket::Comprehension,
        "lambda" | "lambda_case" => CfBucket::Closure,
        "apply" | "infix" => CfBucket::Call,
        // `let`/`where` value bindings.
        "bind" => CfBucket::Assign,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Haskell shape mapping, built once on first use.
#[must_use]
pub fn haskell_shape_mapping() -> &'static HaskellShapeMapping {
    static MAPPING: OnceLock<HaskellShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(HaskellShapeMapping::build)
}

#[cfg(test)]
mod shape_tests {
    //! Coverage for the Haskell [`ShapeMapping`]. Consumes the hand-written
    //! control-flow fixture so the test is load-bearing.

    use super::{cf_bucket_for_haskell_kind, haskell_shape_mapping};
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, ShapeMapping, compute_shape_descriptor,
    };
    use tree_sitter::{Node, Parser, Tree};

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/dynamic/sample.hs"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_haskell::LANGUAGE.into())
            .expect("load haskell grammar");
        parser.parse(src, None).expect("parse haskell")
    }

    /// First function-equation declaration (`classify value label rest = ...`):
    /// the first `function` node that carries a `patterns` field (which the
    /// type-signature arrow `function` nodes do not).
    fn first_equation<'t>(tree: &'t Tree) -> Node<'t> {
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function" && node.child_by_field_name("patterns").is_some() {
                return node;
            }
            let mut cursor = node.walk();
            let mut children: Vec<Node> = node.named_children(&mut cursor).collect();
            children.reverse();
            stack.extend(children);
        }
        panic!("no function equation in haskell fixture");
    }

    #[test]
    fn mapping_is_non_empty_and_covers_real_kinds() {
        assert_eq!(
            cf_bucket_for_haskell_kind("conditional"),
            Some(CfBucket::Branch)
        );
        assert_eq!(cf_bucket_for_haskell_kind("guards"), Some(CfBucket::Branch));
        assert_eq!(cf_bucket_for_haskell_kind("case"), Some(CfBucket::Match));
        assert_eq!(
            cf_bucket_for_haskell_kind("alternative"),
            Some(CfBucket::Match)
        );
        assert_eq!(
            cf_bucket_for_haskell_kind("generator"),
            Some(CfBucket::Loop)
        );
        assert_eq!(
            cf_bucket_for_haskell_kind("list_comprehension"),
            Some(CfBucket::Comprehension)
        );
        assert_eq!(
            cf_bucket_for_haskell_kind("lambda"),
            Some(CfBucket::Closure)
        );
        assert_eq!(cf_bucket_for_haskell_kind("apply"), Some(CfBucket::Call));
        assert_eq!(cf_bucket_for_haskell_kind("bind"), Some(CfBucket::Assign));
        assert_eq!(cf_bucket_for_haskell_kind("nope"), None);

        let lang: tree_sitter::Language = tree_sitter_haskell::LANGUAGE.into();
        let id = (0..lang.node_kind_count())
            .map(|i| i as u16)
            .find(|&i| lang.node_kind_is_named(i) && lang.node_kind_for_id(i) == Some("case"))
            .expect("grammar exposes named case");
        assert_eq!(haskell_shape_mapping().cf_bucket(id), Some(CfBucket::Match));
    }

    #[test]
    fn descriptor_covers_fixture_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_equation(&tree);
        let descriptor = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            haskell_shape_mapping(),
            &ShapeBudget::default(),
        );
        let hist = descriptor.cf_histogram;
        assert!(hist[CfBucket::Branch.index()] >= 1, "conditional/guards");
        assert!(hist[CfBucket::Match.index()] >= 1, "case/alternative");
        assert!(
            hist[CfBucket::Comprehension.index()] >= 1,
            "list comprehension"
        );
        assert!(hist[CfBucket::Closure.index()] >= 1, "lambda");
        assert!(hist[CfBucket::Call.index()] >= 1, "apply");
        assert!(hist[CfBucket::Assign.index()] >= 1, "let/where bind");
    }

    #[test]
    fn signature_shape_reads_pattern_arity() {
        let tree = parse(SAMPLE);
        let func = first_equation(&tree);
        let shape = haskell_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // `classify value label rest = ...` binds three patterns.
        assert_eq!(shape.arity_positional, 3, "value + label + rest");
    }
}
