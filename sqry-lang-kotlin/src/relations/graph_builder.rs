//! Kotlin `GraphBuilder` implementation for code graph construction.
//!
//! Extracts Kotlin-specific relationships:
//! - Class definitions (regular, data, sealed, objects, companion objects)
//! - Function definitions (regular, suspend, inline, extension functions)
//! - Call expressions (regular calls, method calls, extension calls)
//!
//! # Multi-Pass Strategy
//!
//! 1. **Pass 1**: Extract class/object definitions → Create Class nodes
//! 2. **Pass 2**: Extract function/property definitions → Create Function nodes
//! 3. **Pass 3**: Extract call expressions → Create Call edges

use sqry_core::graph::unified::build::helper::CalleeKindHint;
use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::unified::{GraphBuildHelper, StagingGraph};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language, Span};
use std::sync::OnceLock;
use std::{collections::HashMap, path::Path};
use tree_sitter::{Node, Tree};

use crate::relations::local_scopes::{self, KotlinScopeTree, ResolutionOutcome};
use crate::relations::type_extractor::{extract_all_type_names_from_kotlin_type, is_type_node};
use sqry_core::graph::unified::node::NodeKind;

/// Kotlin-specific `GraphBuilder` implementation.
///
/// Performs multi-pass analysis:
/// 1. Extract class and object definitions
/// 2. Extract function and property definitions
/// 3. Extract call expressions
///
/// # Example
///
/// ```no_run
/// use sqry_lang_kotlin::relations::KotlinGraphBuilder;
/// use sqry_core::graph::GraphBuilder;
/// use sqry_core::graph::unified::StagingGraph;
/// use tree_sitter::Parser;
///
/// let mut parser = Parser::new();
/// parser.set_language(&tree_sitter_kotlin_sqry::language()).unwrap();
/// let tree = parser.parse(b"class User { fun getName() = \"Alice\" }", None).unwrap();
/// let mut staging = StagingGraph::new();
/// let builder = KotlinGraphBuilder::new();
/// builder.build_graph(&tree, b"class User { fun getName() = \"Alice\" }",
///                      std::path::Path::new("test.kt"), &mut staging).unwrap();
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct KotlinGraphBuilder;

impl KotlinGraphBuilder {
    /// Create a new Kotlin `GraphBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if a node has the `private` or `internal` visibility modifier.
    ///
    /// In Kotlin:
    /// - Default visibility is public
    /// - `private` makes a symbol private to the file or class (not exported)
    /// - `internal` makes a symbol visible within the module (not exported across module boundaries)
    /// - `protected` makes a symbol visible in subclasses (we treat as exported)
    ///
    /// For export purposes, we only export symbols that are public (no modifier or explicit `public`).
    fn is_private_or_internal(node: &tree_sitter::Node, content: &[u8]) -> bool {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "modifiers" {
                let mut mod_cursor = child.walk();
                for modifier in child.children(&mut mod_cursor) {
                    if let Ok(mod_text) = modifier.utf8_text(content)
                        && (mod_text == "private" || mod_text == "internal")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl GraphBuilder for KotlinGraphBuilder {
    fn language(&self) -> Language {
        Language::Kotlin
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(kotlin_shape_mapping())
    }

    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Kotlin);

        // Build AST graph for call context tracking
        let ast_graph =
            ASTGraph::from_tree(tree, content, 4).map_err(|e| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: e,
            })?;

        // Build scope tree for local variable reference tracking
        let mut scope_tree = local_scopes::build(tree.root_node(), content)?;

        // Walk tree to find classes, functions, and calls
        walk_tree_for_graph(
            tree.root_node(),
            content,
            &ast_graph,
            &mut helper,
            &mut scope_tree,
        )?;

        Ok(())
    }
}

// ============================================================================
// AST Graph - tracks callable contexts (functions, methods, classes)
// ============================================================================

#[derive(Debug, Clone)]
struct CallContext {
    qualified_name: String,
    span: (usize, usize),
    is_async: bool,
    is_method: bool,
    class_name: Option<String>,
    /// Return type of the function (e.g., `Deferred<Int>`, `String`)
    return_type: Option<String>,
    /// Whether this is an external function (JNI/Kotlin Native FFI)
    is_external: bool,
}

impl CallContext {
    fn qualified_name(&self) -> &str {
        &self.qualified_name
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

        // Create recursion guard
        let recursion_limits = sqry_core::config::RecursionLimits::load_or_default()
            .map_err(|e| format!("Failed to load recursion limits: {e}"))?;
        let file_ops_depth = recursion_limits
            .effective_file_ops_depth()
            .map_err(|e| format!("Invalid file_ops_depth configuration: {e}"))?;
        let mut guard = sqry_core::query::security::RecursionGuard::new(file_ops_depth)
            .map_err(|e| format!("Failed to create recursion guard: {e}"))?;

        walk_ast(
            tree.root_node(),
            content,
            &mut contexts,
            &mut node_to_context,
            &mut scope_stack,
            &mut class_stack,
            max_depth,
            &mut guard,
        )?;

        Ok(Self {
            contexts,
            node_to_context,
        })
    }

    fn get_callable_context(&self, node_id: usize) -> Option<&CallContext> {
        self.node_to_context
            .get(&node_id)
            .and_then(|idx| self.contexts.get(*idx))
    }

    fn get_enclosing_callable_context(&self, node: Node<'_>) -> Option<&CallContext> {
        self.get_callable_context(node.id()).or_else(|| {
            self.find_enclosing_callable_context_by_byte_span(node.start_byte(), node.end_byte())
        })
    }

    fn find_enclosing_callable_context_by_byte_span(
        &self,
        start_byte: usize,
        end_byte: usize,
    ) -> Option<&CallContext> {
        self.contexts
            .iter()
            .filter(|ctx| ctx.span.0 <= start_byte && end_byte <= ctx.span.1)
            .min_by_key(|ctx| ctx.span.1.saturating_sub(ctx.span.0))
    }
}

#[allow(clippy::too_many_lines)] // Central traversal; refactor after Kotlin AST stabilizes.
/// # Errors
///
/// Returns error if recursion depth exceeds the guard's limit.
fn walk_ast(
    node: Node,
    content: &[u8],
    contexts: &mut Vec<CallContext>,
    node_to_context: &mut HashMap<usize, usize>,
    scope_stack: &mut Vec<String>,
    class_stack: &mut Vec<String>,
    max_depth: usize,
    guard: &mut sqry_core::query::security::RecursionGuard,
) -> Result<(), String> {
    guard
        .enter()
        .map_err(|e| format!("Recursion limit exceeded: {e}"))?;

    if scope_stack.len() > max_depth {
        guard.exit();
        return Ok(());
    }

    match node.kind() {
        "class_declaration" | "object_declaration" => {
            // Find the type_identifier child
            let mut cursor = node.walk();
            let name_node = node
                .children(&mut cursor)
                .find(|child| child.kind() == "type_identifier");

            if let Some(name_node) = name_node {
                let class_name = name_node
                    .utf8_text(content)
                    .map_err(|_| "failed to read class name".to_string())?;

                // Build qualified class name
                let qualified_class = if scope_stack.is_empty() {
                    class_name.to_string()
                } else {
                    format!("{}.{}", scope_stack.join("."), class_name)
                };

                class_stack.push(qualified_class);
                scope_stack.push(class_name.to_string());

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
                        guard,
                    )?;
                }

                class_stack.pop();
                scope_stack.pop();
            } else {
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
                        guard,
                    )?;
                }
            }
        }
        "function_declaration" => {
            // Find the simple_identifier child
            let mut cursor = node.walk();
            let name_node = node
                .children(&mut cursor)
                .find(|child| child.kind() == "simple_identifier" || child.kind() == "identifier");

            let Some(name_node) = name_node else {
                // Still recurse into nested declarations (e.g., local functions) without failing the build.
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
                        guard,
                    )?;
                }
                guard.exit();
                return Ok(());
            };

            let func_name = name_node
                .utf8_text(content)
                .map_err(|_| "failed to read function name".to_string())?;

            // Check if suspend (async)
            let is_async = node.children(&mut node.walk()).any(|child| {
                if child.kind() == "modifiers" {
                    child
                        .children(&mut child.walk())
                        .any(|modifier| modifier.utf8_text(content) == Ok("suspend"))
                } else {
                    false
                }
            });

            // Check if external (FFI)
            let is_external = node.children(&mut node.walk()).any(|child| {
                if child.kind() == "modifiers" {
                    child
                        .children(&mut child.walk())
                        .any(|modifier| modifier.utf8_text(content) == Ok("external"))
                } else {
                    false
                }
            });

            // Build qualified function name
            let qualified_func = if scope_stack.is_empty() {
                func_name.to_string()
            } else {
                format!("{}.{}", scope_stack.join("."), func_name)
            };

            // Determine if this is a method (inside a class)
            let is_method = !class_stack.is_empty();
            let class_name = class_stack.last().cloned();

            // Extract return type (Kotlin uses user_type after colon)
            let return_type = extract_return_type(node, content);

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name: qualified_func.clone(),
                span: (node.start_byte(), node.end_byte()),
                is_async,
                is_method,
                class_name,
                return_type,
                is_external,
            });

            // Associate all descendants with this context.
            //
            // `tree-sitter-kotlin-sg` does not assign field names like `body`, so relying on
            // `child_by_field_name` is fragile. Mapping the full subtree ensures call sites can be
            // attributed to the correct enclosing callable.
            associate_descendants(node, context_idx, node_to_context);

            scope_stack.push(func_name.to_string());

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
                    guard,
                )?;
            }

            scope_stack.pop();
        }
        "getter" | "setter" => {
            // Property accessor — create a CallContext so that identifiers inside
            // the body can be attributed to a caller for Reference edges.
            let prefix = if node.kind() == "getter" {
                "get"
            } else {
                "set"
            };
            let property_name = find_preceding_property_name(node, content);
            let accessor_name = if let Some(prop) = property_name {
                format!("{prefix}_{prop}")
            } else {
                format!("<{prefix}>@{}", node.start_byte())
            };

            let qualified_name = if scope_stack.is_empty() {
                accessor_name.clone()
            } else {
                format!("{}.{}", scope_stack.join("."), accessor_name)
            };

            let context_idx = contexts.len();
            contexts.push(CallContext {
                qualified_name,
                span: (node.start_byte(), node.end_byte()),
                is_async: false,
                is_method: !class_stack.is_empty(),
                class_name: class_stack.last().cloned(),
                return_type: None,
                is_external: false,
            });
            associate_descendants(node, context_idx, node_to_context);

            // Push accessor name so nested local functions get correctly scoped names
            // (e.g., a local function `helper()` inside `get_computed` becomes
            // `ClassName.get_computed.helper` rather than `ClassName.helper`).
            scope_stack.push(accessor_name);

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
                    guard,
                )?;
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
                    guard,
                )?;
            }
        }
    }

    guard.exit();
    Ok(())
}

/// Walk backwards through siblings to find the property name associated with a
/// getter/setter node. In tree-sitter-kotlin, getter and setter are siblings of
/// `property_declaration` in `class_body`, not children.
fn find_preceding_property_name(node: Node, content: &[u8]) -> Option<String> {
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        if sibling.kind() == "property_declaration" {
            let mut cursor = sibling.walk();
            for child in sibling.children(&mut cursor) {
                if child.kind() == "variable_declaration" {
                    let mut inner_cursor = child.walk();
                    for inner in child.children(&mut inner_cursor) {
                        if inner.kind() == "simple_identifier" {
                            return inner.utf8_text(content).ok().map(str::to_string);
                        }
                    }
                }
            }
            return None;
        }
        current = sibling.prev_named_sibling();
    }
    None
}

/// Extract return type from a Kotlin function declaration.
///
/// Kotlin syntax: `fun name(): ReturnType { ... }`
/// tree-sitter-kotlin may use different field names, so we search for
/// `user_type` or `nullable_type` after the parameter list.
fn extract_return_type(node: Node, content: &[u8]) -> Option<String> {
    // First try the "type" field name (some grammars use this)
    if let Some(type_node) = node.child_by_field_name("type") {
        return type_node.utf8_text(content).ok().map(str::to_string);
    }

    // Look for user_type or nullable_type AFTER the parameter list (skip parameters)
    let mut cursor = node.walk();
    let mut passed_params = false;
    for child in node.children(&mut cursor) {
        match child.kind() {
            // Skip past parameter list first
            "function_value_parameters" => {
                passed_params = true;
            }
            // Only accept type nodes after parameter list
            "user_type" | "nullable_type" | "type_identifier" if passed_params => {
                return child.utf8_text(content).ok().map(str::to_string);
            }
            _ => {}
        }
    }

    None
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

/// Extract type name from a `delegation_specifier` node.
///
/// Handles various tree-sitter-kotlin structures:
/// - `user_type` → directly contains `type_identifier`
/// - `constructor_invocation` → contains `user_type`
/// - `explicit_delegation` → contains "by" keyword and expression
fn extract_delegation_type(node: Node, content: &[u8]) -> Option<String> {
    // First, try to find a user_type directly or in child nodes
    fn find_type_name(node: Node, content: &[u8]) -> Option<String> {
        match node.kind() {
            "type_identifier" | "simple_identifier" => {
                node.utf8_text(content).ok().map(String::from)
            }
            "user_type" => {
                // user_type contains type_identifier(s) - get the first one for simple types
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "type_identifier" || child.kind() == "simple_identifier" {
                        return child.utf8_text(content).ok().map(String::from);
                    }
                }
                None
            }
            "constructor_invocation" => {
                // constructor_invocation contains user_type
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "user_type" {
                        return find_type_name(child, content);
                    }
                }
                None
            }
            "delegation_specifier" => {
                // delegation_specifier can contain user_type, constructor_invocation, or explicit_delegation
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "user_type" | "constructor_invocation" => {
                            return find_type_name(child, content);
                        }
                        "explicit_delegation" => {
                            // Skip explicit delegation ("by" expressions) for now
                            // These are delegation patterns, not inheritance
                            return None;
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => {
                // Try children
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if let Some(name) = find_type_name(child, content) {
                        return Some(name);
                    }
                }
                None
            }
        }
    }

    find_type_name(node, content)
}

/// Check if a type name looks like an interface.
///
/// Kotlin convention: interfaces often start with 'I' followed by uppercase,
/// but this isn't enforced. We use a heuristic approach.
///
/// Common interface patterns:
/// - Starts with "I" + uppercase (`IClickable`, `ISerializable`)
/// - Common suffix patterns: -able, -ible, -ive, -Listener, -Callback, -Handler
fn is_interface_name(name: &str) -> bool {
    // Pattern 1: Starts with 'I' followed by uppercase letter
    if name.len() >= 2 {
        let chars: Vec<char> = name.chars().collect();
        if chars[0] == 'I' && chars[1].is_uppercase() {
            return true;
        }
    }

    // Pattern 2: Common interface suffixes
    let interface_suffixes = [
        "able",
        "ible",
        "ive",
        "Listener",
        "Callback",
        "Handler",
        "Observer",
        "Provider",
        "Factory",
        "Service",
        "Repository",
        "Adapter",
        "Interface",
        "Contract",
        "Delegate",
    ];

    for suffix in &interface_suffixes {
        if name.ends_with(suffix) {
            return true;
        }
    }

    false
}

/// Process an `import_header` node and create an Import edge.
///
/// Handles three Kotlin import patterns:
/// - Simple: `import com.example.MyClass`
/// - Aliased: `import com.example.MyClass as MC`
/// - Wildcard: `import com.example.*`
///
/// # AST Structure (from tree-sitter-kotlin)
///
/// ```text
/// import_header
///   import
///   identifier (dotted path like "com.example.MyClass")
///     simple_identifier ("com")
///     . (".")
///     simple_identifier ("example")
///     . (".")
///     simple_identifier ("MyClass")
///   [optional] import_alias ("as MC")
///     as
///     type_identifier ("MC")
///   [optional for wildcard] . (".")
///   [optional for wildcard] wildcard_import ("*")
/// ```
fn process_import_header(
    import_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut cursor = import_node.walk();

    // Find the identifier (import path), import_alias (optional), and wildcard_import (optional)
    let mut identifier_node: Option<Node> = None;
    let mut alias_node: Option<Node> = None;
    let mut is_wildcard = false;

    for child in import_node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                identifier_node = Some(child);
            }
            "import_alias" => {
                alias_node = Some(child);
            }
            "wildcard_import" => {
                is_wildcard = true;
            }
            _ => {}
        }
    }

    // Must have an identifier to create an import
    let Some(id_node) = identifier_node else {
        return Ok(());
    };

    // Extract the full import path from the identifier node
    let import_path = id_node
        .utf8_text(content)
        .map_err(|_| GraphBuilderError::ParseError {
            span: Span::from_node(&import_node),
            reason: "failed to read import path".to_string(),
        })?;

    // Build the full imported name
    let imported_name = if is_wildcard {
        format!("{import_path}.*")
    } else {
        import_path.to_string()
    };

    // Extract alias if present
    let alias: Option<String> = if let Some(alias_parent) = alias_node {
        // The alias is in the type_identifier child of import_alias
        let mut alias_cursor = alias_parent.walk();
        alias_parent
            .children(&mut alias_cursor)
            .find(|child| child.kind() == "type_identifier")
            .and_then(|type_id| type_id.utf8_text(content).ok())
            .map(String::from)
    } else {
        None
    };

    // Create module node (importer) and import node (imported)
    let module_id = helper.add_module("<module>", None);
    let imported_id = helper.add_import(&imported_name, Some(Span::from_node(&import_node)));

    // Add import edge with appropriate metadata
    if alias.is_some() || is_wildcard {
        helper.add_import_edge_full(module_id, imported_id, alias.as_deref(), is_wildcard);
    } else {
        helper.add_import_edge(module_id, imported_id);
    }

    Ok(())
}

// ============================================================================
// TypeOf and Reference Edge Processing
// ============================================================================

/// Extract visibility modifier from a node (if present).
///
/// Returns Some("private"), Some("protected"), Some("internal"), or Some("public") if explicit.
/// Returns None for default visibility (public in Kotlin).
fn extract_visibility(node: Node) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.children(&mut mod_cursor) {
                match modifier.kind() {
                    "visibility_modifier" => {
                        // The visibility_modifier node has a child with the actual keyword
                        let mut vis_cursor = modifier.walk();
                        for vis_child in modifier.children(&mut vis_cursor) {
                            match vis_child.kind() {
                                "private" => return Some("private"),
                                "protected" => return Some("protected"),
                                "internal" => return Some("internal"),
                                "public" => return Some("public"),
                                _ => {}
                            }
                        }
                    }
                    "private" => return Some("private"),
                    "protected" => return Some("protected"),
                    "internal" => return Some("internal"),
                    "public" => return Some("public"),
                    _ => {}
                }
            }
        }
    }
    None
}

/// Process `TypeOf` and Reference edges for a property declaration.
///
/// Handles both class properties and top-level variables.
/// Creates `TypeOf` edges with appropriate context (Field vs Variable).
#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)]
#[allow(clippy::unnecessary_wraps)]
fn process_property_typeof_edges(
    node: Node,
    helper: &mut GraphBuildHelper,
    content: &[u8],
    owner_class: Option<&str>,
) -> GraphResult<()> {
    // Extract property name
    let mut cursor = node.walk();
    let name_node = node.children(&mut cursor).find(|child| {
        matches!(
            child.kind(),
            "simple_identifier" | "identifier" | "variable_declaration"
        )
    });

    let Some(name_node) = name_node else {
        return Ok(());
    };

    // For variable_declaration, extract the actual identifier
    let property_name = if name_node.kind() == "variable_declaration" {
        let mut var_cursor = name_node.walk();
        if let Some(id_node) = name_node
            .children(&mut var_cursor)
            .find(|child| child.kind() == "simple_identifier")
        {
            id_node.utf8_text(content).ok()
        } else {
            None
        }
    } else {
        name_node.utf8_text(content).ok()
    };

    let Some(property_name) = property_name else {
        return Ok(());
    };

    // Find variable_declaration child which contains the type information
    let mut cursor = node.walk();
    let var_decl_node = node
        .children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration");

    let Some(var_decl_node) = var_decl_node else {
        return Ok(());
    };

    // Find type node inside variable_declaration
    // Structure: simple_identifier (name) : user_type (type)
    // We need to skip the identifier and colon, then find the type
    let mut cursor = var_decl_node.walk();
    let mut found_colon = false;
    let type_node = var_decl_node.children(&mut cursor).find(|child| {
        // Skip everything until we pass the colon
        if child.kind() == ":" {
            found_colon = true;
            return false;
        }

        // After colon, find the type node
        if found_colon
            && matches!(
                child.kind(),
                "user_type"
                    | "nullable_type"
                    | "function_type"
                    | "type_reference"
                    | "type_identifier"
                    | "platform_type"
                    | "parenthesized_type"
            )
        {
            return true;
        }

        false
    });

    let Some(type_node) = type_node else {
        return Ok(()); // No type annotation
    };

    // Extract full type text for TypeOf edge
    let type_text = type_node.utf8_text(content).ok();
    let Some(type_text) = type_text else {
        return Ok(());
    };

    // Determine if this is const val (true constant) or just val/var (variable)
    // Kotlin: const val is compile-time constant, val is immutable variable, var is mutable
    let has_const_modifier = node.children(&mut node.walk()).any(|child| {
        if child.kind() == "modifiers" {
            child
                .children(&mut child.walk())
                .any(|m| m.kind() == "const")
        } else {
            false
        }
    });

    // Create property or constant node
    let qualified_name = if let Some(class_name) = owner_class {
        format!("{class_name}.{property_name}")
    } else {
        property_name.to_string()
    };

    // Extract visibility for properties
    let visibility = extract_visibility(node);

    // Class properties use Property node kind, const val uses Constant
    let property_id = if has_const_modifier {
        if visibility.is_some() {
            helper.add_constant_with_visibility(
                &qualified_name,
                Some(Span::from_node(&node)),
                visibility,
            )
        } else {
            helper.add_constant(&qualified_name, Some(Span::from_node(&node)))
        }
    } else if owner_class.is_some() {
        // Class property
        helper.add_property_with_static_and_visibility(
            &qualified_name,
            Some(Span::from_node(&node)),
            false,
            visibility,
        )
    } else {
        // Top-level variable
        helper.add_variable(&qualified_name, Some(Span::from_node(&node)))
    };
    // issue #394: real declaration; opt dual-use bare helper into is_definition
    // (constant/property branches already mark via *_with_* helpers; the
    // monotonic OR-in is a no-op there and sets the top-level variable case).
    helper.mark_definition(property_id);

    // Create TypeOf edge
    let type_id = helper.add_type(type_text, None);
    let context = if owner_class.is_some() {
        TypeOfContext::Field
    } else {
        TypeOfContext::Variable
    };
    helper.add_typeof_edge_with_context(
        property_id,
        type_id,
        Some(context),
        None,
        Some(property_name),
    );

    // Create Reference edges for all nested types
    let referenced_types = extract_all_type_names_from_kotlin_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(property_id, ref_type_id);
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for function parameters.
fn process_function_parameters_typeof(
    func_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Find function_value_parameters node
    let mut cursor = func_node.walk();
    let params_node = func_node
        .children(&mut cursor)
        .find(|child| child.kind() == "function_value_parameters");

    let Some(params_node) = params_node else {
        return Ok(());
    };

    // Process each parameter
    let mut param_index = 0u8;
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        if child.kind() == "parameter" {
            process_parameter_typeof(child, func_name, class_name, param_index, helper, content)?;
            param_index = param_index.saturating_add(1);
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a single parameter.
#[allow(clippy::unnecessary_wraps)]
fn process_parameter_typeof(
    param_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    param_index: u8,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Extract parameter name
    let mut cursor = param_node.walk();
    let name_node = param_node
        .children(&mut cursor)
        .find(|child| child.kind() == "simple_identifier");

    let Some(name_node) = name_node else {
        return Ok(());
    };

    let param_name = name_node.utf8_text(content).ok();
    let Some(param_name) = param_name else {
        return Ok(());
    };

    // Find type node after colon (same pattern as properties)
    // Structure: simple_identifier (name) : user_type (type)
    let mut cursor = param_node.walk();
    let mut found_colon = false;
    let type_node = param_node.children(&mut cursor).find(|child| {
        if child.kind() == ":" {
            found_colon = true;
            return false;
        }

        if found_colon
            && matches!(
                child.kind(),
                "user_type"
                    | "nullable_type"
                    | "function_type"
                    | "type_reference"
                    | "type_identifier"
            )
        {
            return true;
        }

        false
    });

    let Some(type_node) = type_node else {
        return Ok(()); // No type annotation
    };

    // Extract full type text
    let type_text = type_node.utf8_text(content).ok();
    let Some(type_text) = type_text else {
        return Ok(());
    };

    // Get the function or method node ID (should already exist from walk_tree_for_graph)
    // func_name is already qualified (e.g., "ClassName.methodName" for methods)
    let param_span = Span::from_node(&param_node);
    let func_id = if class_name.is_some() {
        helper.ensure_method(func_name, None, false, false)
    } else {
        helper.ensure_callee(func_name, param_span, CalleeKindHint::Function)
    };

    // Create TypeOf edge
    let type_id = helper.add_type(type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(u16::from(param_index)),
        Some(param_name),
    );

    // Create Reference edges
    let referenced_types = extract_all_type_names_from_kotlin_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for function return type.
#[allow(clippy::unnecessary_wraps)]
fn process_function_return_typeof(
    func_node: Node,
    func_name: &str,
    class_name: Option<&str>,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // Find type_reference after the function parameters (return type)
    // Kotlin structure: fun name(params): ReturnType
    let mut cursor = func_node.walk();
    let mut found_params = false;

    for child in func_node.children(&mut cursor) {
        // Skip until we pass the parameters
        if child.kind() == "function_value_parameters" {
            found_params = true;
            continue;
        }

        // After parameters and colon, look for the return type
        if found_params
            && matches!(
                child.kind(),
                "user_type"
                    | "nullable_type"
                    | "function_type"
                    | "type_reference"
                    | "type_identifier"
                    | "platform_type"
                    | "parenthesized_type"
            )
        {
            // Extract full type text
            let type_text = child.utf8_text(content).ok();
            let Some(type_text) = type_text else {
                continue;
            };

            // Get the function or method node ID
            // func_name is already qualified (e.g., "ClassName.methodName" for methods)
            let func_id = if class_name.is_some() {
                helper.ensure_method(func_name, None, false, false)
            } else {
                helper.ensure_callee(
                    func_name,
                    Span::from_node(&func_node),
                    CalleeKindHint::Function,
                )
            };

            // Create TypeOf edge
            let type_id = helper.add_type(type_text, None);
            helper.add_typeof_edge_with_context(
                func_id,
                type_id,
                Some(TypeOfContext::Return),
                Some(0), // Return type index is always 0 for consistency with Go/Swift
                None,
            );

            // Create Reference edges
            let referenced_types = extract_all_type_names_from_kotlin_type(child, content);
            for ref_type_name in referenced_types {
                let ref_type_id = helper.add_type(&ref_type_name, None);
                helper.add_reference_edge(func_id, ref_type_id);
            }

            break;
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for constructor parameters.
#[allow(clippy::too_many_lines)]
#[allow(clippy::unnecessary_wraps)]
fn process_constructor_parameters_typeof(
    constructor_node: Node,
    class_name: &str,
    helper: &mut GraphBuildHelper,
    content: &[u8],
) -> GraphResult<()> {
    // class_parameter nodes are direct children of primary_constructor
    // (not wrapped in a class_parameters node)
    let mut cursor = constructor_node.walk();
    let mut param_index: u16 = 0;

    for param in constructor_node.children(&mut cursor) {
        if param.kind() != "class_parameter" {
            continue;
        }

        // Extract parameter name
        let mut param_cursor = param.walk();
        let name_node = param
            .children(&mut param_cursor)
            .find(|child| child.kind() == "simple_identifier");

        let Some(name_node) = name_node else {
            continue;
        };

        let Ok(param_name) = name_node.utf8_text(content) else {
            continue;
        };

        // Find type annotation (after colon)
        let mut param_cursor = param.walk();
        let mut found_colon = false;
        let type_node = param.children(&mut param_cursor).find(|child| {
            if child.kind() == ":" {
                found_colon = true;
                return false;
            }

            if found_colon
                && matches!(
                    child.kind(),
                    "user_type"
                        | "nullable_type"
                        | "function_type"
                        | "type_reference"
                        | "type_identifier"
                        | "platform_type"
                        | "parenthesized_type"
                )
            {
                return true;
            }

            false
        });

        let Some(type_node) = type_node else {
            continue; // No type annotation
        };

        // Extract full type text
        let Ok(type_text) = type_node.utf8_text(content) else {
            continue;
        };

        // Check if this is a property (has val/var modifier)
        // In tree-sitter-kotlin, val/var are nested inside a binding_pattern_kind node
        let mut param_cursor = param.walk();
        let is_property = param.children(&mut param_cursor).any(|child| {
            if child.kind() == "binding_pattern_kind" {
                // Check first child of binding_pattern_kind for val/var
                if let Some(keyword) = child.child(0) {
                    matches!(keyword.kind(), "val" | "var")
                } else {
                    false
                }
            } else {
                // Also check direct children as fallback
                matches!(child.kind(), "val" | "var")
            }
        });

        // Check for const modifier (const val)
        let mut param_cursor = param.walk();
        let has_const_modifier = param.children(&mut param_cursor).any(|child| {
            if child.kind() == "modifiers" {
                child
                    .children(&mut child.walk())
                    .any(|m| m.kind() == "const")
            } else {
                false
            }
        });

        let referenced_types = extract_all_type_names_from_kotlin_type(type_node, content);
        let type_id = helper.add_type(type_text, None);

        if is_property {
            // Constructor property (val/var) - create property node like regular properties
            let qualified_name = format!("{class_name}.{param_name}");
            let visibility = extract_visibility(param);
            let property_id = if has_const_modifier {
                if visibility.is_some() {
                    helper.add_constant_with_visibility(
                        &qualified_name,
                        Some(Span::from_node(&param)),
                        visibility,
                    )
                } else {
                    helper.add_constant(&qualified_name, Some(Span::from_node(&param)))
                }
            } else {
                // Class property
                helper.add_property_with_static_and_visibility(
                    &qualified_name,
                    Some(Span::from_node(&param)),
                    false,
                    visibility,
                )
            };

            // Create TypeOf edge on the property node
            helper.add_typeof_edge_with_context(
                property_id,
                type_id,
                Some(TypeOfContext::Field),
                None,
                Some(param_name),
            );

            // Create Reference edges on the property node
            for ref_type_name in referenced_types {
                let ref_type_id = helper.add_type(&ref_type_name, None);
                helper.add_reference_edge(property_id, ref_type_id);
            }
        } else {
            // Regular constructor parameter (no val/var) - attach to class
            let class_id = helper.add_class(class_name, None);

            helper.add_typeof_edge_with_context(
                class_id,
                type_id,
                Some(TypeOfContext::Parameter),
                Some(param_index),
                Some(param_name),
            );

            // Create Reference edges on the class node
            for ref_type_name in referenced_types {
                let ref_type_id = helper.add_type(&ref_type_name, None);
                helper.add_reference_edge(class_id, ref_type_id);
            }
        }

        param_index = param_index.saturating_add(1);
    }

    Ok(())
}

/// Walk the tree and populate the staging graph.
fn walk_tree_for_graph(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    scope_tree: &mut KotlinScopeTree,
) -> GraphResult<()> {
    walk_tree_for_graph_with_context(node, content, ast_graph, helper, None, scope_tree)
}

/// Walk the tree with class context tracking for property `TypeOf` edges.
#[allow(clippy::too_many_lines)]
fn walk_tree_for_graph_with_context(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    helper: &mut GraphBuildHelper,
    current_class: Option<&str>,
    scope_tree: &mut KotlinScopeTree,
) -> GraphResult<()> {
    match node.kind() {
        "class_declaration" | "object_declaration" => {
            // Extract class/object name
            let mut cursor = node.walk();
            if let Some(name_node) = node
                .children(&mut cursor)
                .find(|child| child.kind() == "type_identifier")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                let span = Span::from_node(&node);
                let qualified_name = class_name.to_string();
                let class_id = helper.add_class(&qualified_name, Some(span));
                // issue #394: real declaration; opt dual-use bare helper into is_definition
                helper.mark_definition(class_id);

                // REQ:R0027 — emit per-type-parameter Type nodes for
                // generic class declarations. Qualified name shape is
                // `<ClassName>.<ParamName>` (canonicalised to `::`
                // downstream). Variance modifiers (`<in T>` / `<out T>`)
                // and `where`-clause constraints at class level are
                // handled by `process_type_parameter_declarations`.
                process_type_parameter_declarations(node, content, &qualified_name, helper);

                // Export class if not private or internal
                // In Kotlin, default visibility is public, so we export unless explicitly private/internal
                if !KotlinGraphBuilder::is_private_or_internal(&node, content) {
                    let module_id = helper.add_module("<module>", None);
                    helper.add_export_edge(module_id, class_id);
                }

                // Extract inheritance/implementation from delegation_specifier children
                // Kotlin syntax: class Foo : Bar(), Baz { }
                // tree-sitter-kotlin structure:
                //   class_declaration
                //     type_identifier (class name)
                //     : (colon)
                //     delegation_specifier (one for each parent/interface)
                //       constructor_invocation or user_type
                let mut cursor = node.walk();
                let mut first_type = true;
                for child in node.children(&mut cursor) {
                    if child.kind() == "delegation_specifier" {
                        // Extract the type from the specifier
                        // Could be: user_type, constructor_invocation, or explicit_delegation
                        if let Some(parent_name) = extract_delegation_type(child, content) {
                            let parent_name = parent_name.trim();
                            if !parent_name.is_empty() {
                                // In Kotlin, interfaces typically start with 'I' or don't have ()
                                // But we use a simpler heuristic: first type is superclass, rest are interfaces
                                // unless it looks like an interface name
                                let is_interface = is_interface_name(parent_name);

                                if is_interface {
                                    let interface_id = helper.add_interface(parent_name, None);
                                    helper.add_implements_edge(class_id, interface_id);
                                } else if first_type {
                                    let parent_id = helper.add_class(parent_name, None);
                                    helper.add_inherits_edge(class_id, parent_id);
                                    first_type = false;
                                } else {
                                    // Additional non-I types after first are also interfaces
                                    let interface_id = helper.add_interface(parent_name, None);
                                    helper.add_implements_edge(class_id, interface_id);
                                }
                            }
                        }
                    }
                }

                // Recurse into class children with class name as context
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_tree_for_graph_with_context(
                        child,
                        content,
                        ast_graph,
                        helper,
                        Some(class_name),
                        scope_tree,
                    )?;
                }

                // Return early to avoid double recursion
                return Ok(());
            }
        }
        "function_declaration" => {
            // Extract function context from AST graph
            if let Some(context) = ast_graph.get_enclosing_callable_context(node) {
                let span = Span::from_node(&node);

                // Extract visibility (Kotlin default is public)
                let visibility = extract_visibility(node).or(Some("public"));

                // Add function or method node with return type for returns: queries
                let node_id = if context.is_method {
                    helper.add_method_with_signature(
                        &context.qualified_name,
                        Some(span),
                        context.is_async,
                        false, // Kotlin doesn't distinguish static methods at AST level
                        visibility,
                        context.return_type.as_deref(),
                    )
                } else {
                    helper.add_function_with_signature(
                        &context.qualified_name,
                        Some(span),
                        context.is_async,
                        false, // Kotlin doesn't have unsafe
                        visibility,
                        context.return_type.as_deref(),
                    )
                };

                // Export top-level functions only (not methods inside classes)
                // In Kotlin, default visibility is public, so we export unless explicitly private/internal
                if !context.is_method && !KotlinGraphBuilder::is_private_or_internal(&node, content)
                {
                    let module_id = helper.add_module("<module>", None);
                    helper.add_export_edge(module_id, node_id);
                }

                // JNI/Kotlin Native: Create FFI edge for external functions
                if context.is_external {
                    build_jni_external_function_edge(context, helper, node, content);
                }

                // Process TypeOf and Reference edges for parameters and returns
                process_function_parameters_typeof(
                    node,
                    &context.qualified_name,
                    context.class_name.as_deref(),
                    helper,
                    content,
                )?;
                process_function_return_typeof(
                    node,
                    &context.qualified_name,
                    context.class_name.as_deref(),
                    helper,
                    content,
                )?;

                // REQ:R0027 — emit per-type-parameter Type nodes for
                // generic function declarations. Qualified name shape:
                //   * top-level fun: `<func>.<ParamName>`
                //   * member fun:    `<Class>.<func>.<ParamName>`
                // (`context.qualified_name` already encodes the
                // `<Class>.<func>` shape; canonicalisation rewrites the
                // `.` separators to `::` downstream.)
                //
                // `inline fun <reified T>` and variance markers on
                // function-type-parameters do not occur in vanilla
                // Kotlin grammar — variance is class-only — but reified
                // is preserved here as a base node (attribute deferred
                // per design §4.15). `where T : A, T : B` clauses are
                // collected from the function-declaration's
                // `type_constraints` child by
                // `process_type_parameter_declarations`.
                process_type_parameter_declarations(node, content, &context.qualified_name, helper);
            }
        }
        "primary_constructor" => {
            // Process constructor parameters (including data class properties)
            if let Some(class_name) = current_class {
                process_constructor_parameters_typeof(node, class_name, helper, content)?;
            }
        }
        "property_declaration" => {
            // Process TypeOf and Reference edges for properties
            // Use the current_class context passed from parent
            process_property_typeof_edges(node, helper, content, current_class)?;
        }
        "call_expression" => {
            // Build call edge
            if let Some((caller_qname, callee_qname)) =
                build_call_for_staging(ast_graph, node, content)?
            {
                // Ensure both nodes exist
                let call_context = ast_graph.get_enclosing_callable_context(node);
                let _is_async = call_context.is_some_and(|c| c.is_async);

                let call_span = Span::from_node(&node);
                let caller_function_id =
                    helper.ensure_callee(&caller_qname, call_span, CalleeKindHint::Function);
                let target_function_id =
                    helper.ensure_callee(&callee_qname, call_span, CalleeKindHint::Function);

                // Add call edge
                let argument_count = count_call_arguments(node);
                helper.add_call_edge_full_with_span(
                    caller_function_id,
                    target_function_id,
                    argument_count,
                    false,
                    vec![call_span],
                );
            }
        }
        "import_header" => {
            // Process import statement
            process_import_header(node, content, helper)?;
        }
        "simple_identifier" => {
            // Resolve local variable references
            handle_identifier_for_reference(node, content, ast_graph, scope_tree, helper);
        }
        _ => {}
    }

    // Recurse into children (with current class context)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_graph_with_context(
            child,
            content,
            ast_graph,
            helper,
            current_class,
            scope_tree,
        )?;
    }

    Ok(())
}

/// Extract callee name from a callee expression, handling various expression types.
///
/// Handles:
/// - Direct identifiers: `foo()`
/// - Navigation expressions: `obj.method()`
/// - Parenthesized expressions: `(getHandler)()`
/// - Chained calls: `getHandler()(x)` (extracts from inner call)
fn extract_callee_name(
    expr: Node<'_>,
    content: &[u8],
    call_node: &Node<'_>,
) -> GraphResult<Option<String>> {
    match expr.kind() {
        "simple_identifier" | "identifier" | "type_identifier" => {
            let name = expr
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: Span::from_node(call_node),
                    reason: "failed to read call callee identifier".to_string(),
                })?
                .trim()
                .to_string();
            Ok(Some(name))
        }
        "navigation_expression" => {
            // Extract the method name from `obj.method` or `obj?.method`
            let mut nav_cursor = expr.walk();
            let suffix = expr
                .children(&mut nav_cursor)
                .find(|child| child.kind() == "navigation_suffix");

            let Some(suffix) = suffix else {
                return Ok(None);
            };

            let mut suffix_cursor = suffix.walk();
            let name_node = suffix.children(&mut suffix_cursor).find(|child| {
                child.kind() == "simple_identifier"
                    || child.kind() == "identifier"
                    || child.kind() == "type_identifier"
            });

            let Some(name_node) = name_node else {
                return Ok(None);
            };

            let name = name_node
                .utf8_text(content)
                .map_err(|_| GraphBuilderError::ParseError {
                    span: Span::from_node(call_node),
                    reason: "failed to read call navigation callee identifier".to_string(),
                })?
                .trim()
                .to_string();
            Ok(Some(name))
        }
        "parenthesized_expression" => {
            // Unwrap parentheses: `(foo)()` -> extract from inner expression
            let mut cursor = expr.walk();
            let inner = expr
                .children(&mut cursor)
                .find(|child| child.is_named() && child.kind() != "(" && child.kind() != ")");

            if let Some(inner) = inner {
                extract_callee_name(inner, content, call_node)
            } else {
                Ok(None)
            }
        }
        "call_expression" => {
            // Chained call: `getHandler()(x)` - we can't easily name this,
            // but we could extract the outer call's callee if desired.
            // For now, skip these complex cases to avoid false positives.
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn count_call_arguments(call_node: Node<'_>) -> u8 {
    let args_node = call_node
        .child_by_field_name("value_arguments")
        .or_else(|| {
            let mut cursor = call_node.walk();
            call_node
                .children(&mut cursor)
                .find(|child| child.kind() == "value_arguments")
        });

    let Some(args_node) = args_node else {
        return 255;
    };

    let mut count: u16 = 0;
    let mut cursor = args_node.walk();
    for child in args_node.children(&mut cursor) {
        if child.kind() == "value_argument" {
            count += 1;
        }
    }

    if count <= 254 {
        u8::try_from(count).unwrap_or(u8::MAX)
    } else {
        u8::MAX
    }
}

/// Build call edge information for the staging graph.
fn build_call_for_staging(
    ast_graph: &ASTGraph,
    call_node: Node<'_>,
    content: &[u8],
) -> GraphResult<Option<(String, String)>> {
    if call_node.kind() != "call_expression" {
        return Ok(None);
    }

    // Get or create module-level context for top-level calls
    let module_context;
    let call_context = if let Some(ctx) = ast_graph.get_enclosing_callable_context(call_node) {
        ctx
    } else {
        // Create synthetic module-level context for top-level calls
        module_context = CallContext {
            qualified_name: "<module>".to_string(),
            span: (0, content.len()),
            is_async: false,
            is_method: false,
            class_name: None,
            return_type: None,
            is_external: false,
        };
        &module_context
    };

    // The Kotlin grammar represents calls as:
    //   call_expression := _expression call_suffix
    // so we need to extract the callee name from the leading expression.
    let mut cursor = call_node.walk();
    let callee_expr = call_node
        .children(&mut cursor)
        .find(|child| child.kind() != "call_suffix");

    let Some(callee_expr) = callee_expr else {
        return Ok(None);
    };

    let callee_name = extract_callee_name(callee_expr, content, &call_node)?;

    let Some(callee_name) = callee_name else {
        return Ok(None);
    };

    if callee_name.is_empty() {
        return Ok(None);
    }

    // Derive qualified callee name
    let caller_qualified_name = call_context.qualified_name().to_string();
    let target_qualified_name = callee_name;

    Ok(Some((caller_qualified_name, target_qualified_name)))
}

// ================================
// FFI Detection (JNI / Kotlin Native)
// ================================

/// Extract parameter types from a function declaration node.
///
/// Returns a vector of Kotlin type names, for example `["Int", "String", "Double"]`.
fn extract_parameter_types_from_function(func_node: Node, content: &[u8]) -> Vec<String> {
    let mut param_types = Vec::new();

    // Find the function_value_parameters node
    let mut cursor = func_node.walk();
    let params_node = func_node
        .children(&mut cursor)
        .find(|child| child.kind() == "function_value_parameters");

    if let Some(params_node) = params_node {
        let mut param_cursor = params_node.walk();

        // Iterate over parameters
        for param in params_node.children(&mut param_cursor) {
            if param.kind() == "parameter" {
                // Find the type after the colon
                let mut inner_cursor = param.walk();
                let mut found_colon = false;

                for child in param.children(&mut inner_cursor) {
                    if child.kind() == ":" {
                        found_colon = true;
                        continue;
                    }

                    // After colon, look for type nodes
                    if found_colon && is_type_node(child.kind()) {
                        // Extract the type text (preserve nullable marker for correct boxing)
                        if let Ok(type_text) = child.utf8_text(content) {
                            let type_name = type_text.trim();
                            param_types.push(type_name.to_string());
                        }
                        break;
                    }
                }
            }
        }
    }

    param_types
}

/// Normalize a Kotlin type name by removing fully qualified prefixes and handling type projections.
///
/// Handles:
/// - Fully qualified stdlib types: `kotlin.Int` → `Int`, `kotlin.collections.List` → `List`
/// - Type projections: `out String` → `String`, `in Any` → `Any`, `*` → `Any?`
/// - Whitespace trimming
///
/// This allows the main descriptor function to work uniformly on simple names.
fn normalize_kotlin_type(kotlin_type: &str) -> String {
    let trimmed = kotlin_type.trim();

    // Handle star projection: * → Any? (maps to Object)
    if trimmed == "*" {
        return "Any?".to_string();
    }

    // Strip variance modifiers (out/in) used in type projections
    let without_variance = if let Some(stripped) = trimmed.strip_prefix("out ") {
        stripped.trim()
    } else if let Some(stripped) = trimmed.strip_prefix("in ") {
        stripped.trim()
    } else {
        trimmed
    };

    // Strip fully qualified Kotlin stdlib prefixes
    let normalized = if let Some(stripped) = without_variance.strip_prefix("kotlin.collections.") {
        stripped
    } else if let Some(stripped) = without_variance.strip_prefix("kotlin.") {
        stripped
    } else {
        without_variance
    };

    normalized.to_string()
}

/// Convert a Kotlin type name to a JVM type descriptor.
///
/// JVM descriptors use a compact format:
/// - Primitives: I (int), D (double), Z (boolean), F (float), J (long), B (byte), S (short), C (char)
/// - Boxed types: Ljava/lang/Integer;, Ljava/lang/Double;, etc. (for nullable primitives)
/// - Reference types: Ljava/lang/String;, Ljava/util/List;
/// - Arrays: [I (int[]), [Ljava/lang/String; (String[])
///
/// Handles nullable types, arrays (both primitive and generic), Kotlin stdlib mappings,
/// fully qualified type names, and type projections.
fn kotlin_type_to_jvm_descriptor(kotlin_type: &str) -> String {
    // Normalize first: strip prefixes and handle projections
    let normalized = normalize_kotlin_type(kotlin_type);

    // Check for nullable type (ends with ?)
    let (base_type, is_nullable) = if let Some(stripped) = normalized.strip_suffix('?') {
        (stripped, true)
    } else {
        (normalized.as_str(), false)
    };

    // Handle Array<T> generic (before checking primitives)
    if let Some(array_content) = base_type.strip_prefix("Array<")
        && let Some(element_type) = array_content.strip_suffix('>')
    {
        let trimmed_element = element_type.trim();

        // Normalize the element type first to handle qualified types and projections
        // Array<kotlin.Int> → normalize → Array<Int> → box to [Ljava/lang/Integer;
        // Array<out Int> → normalize → Array<Int> → box to [Ljava/lang/Integer;
        let normalized_element = normalize_kotlin_type(trimmed_element);

        // Check if the normalized element is nullable (Array<Int?> uses nullable boxed type)
        let (element_base, _element_nullable) =
            if let Some(stripped) = normalized_element.strip_suffix('?') {
                (stripped, true)
            } else {
                (normalized_element.as_str(), false)
            };

        // Array<T> in Kotlin always holds object references, so primitive types must be boxed
        // Array<Int> → [Ljava/lang/Integer;, not [I
        // Array<kotlin.Int> → [Ljava/lang/Integer; (after normalization)
        // Array<out Int> → [Ljava/lang/Integer; (after normalization)
        let element_descriptor = match element_base {
            // Signed primitives in Array<T> are boxed to java.lang wrappers
            "Int" => "Ljava/lang/Integer;".to_string(),
            "Double" => "Ljava/lang/Double;".to_string(),
            "Float" => "Ljava/lang/Float;".to_string(),
            "Long" => "Ljava/lang/Long;".to_string(),
            "Short" => "Ljava/lang/Short;".to_string(),
            "Byte" => "Ljava/lang/Byte;".to_string(),
            "Char" => "Ljava/lang/Character;".to_string(),
            "Boolean" => "Ljava/lang/Boolean;".to_string(),
            // Unsigned primitives in Array<T> are boxed to kotlin.U* wrappers
            "UInt" => "Lkotlin/UInt;".to_string(),
            "ULong" => "Lkotlin/ULong;".to_string(),
            "UByte" => "Lkotlin/UByte;".to_string(),
            "UShort" => "Lkotlin/UShort;".to_string(),
            // Reference types - process the normalized element (handles String, stdlib, custom types)
            _ => kotlin_type_to_jvm_descriptor(&normalized_element),
        };

        return format!("[{element_descriptor}");
    }

    // Handle nullable primitives → boxed types
    if is_nullable {
        return match base_type {
            // Signed primitives - box to java.lang wrappers
            "Int" => "Ljava/lang/Integer;".to_string(),
            "Double" => "Ljava/lang/Double;".to_string(),
            "Float" => "Ljava/lang/Float;".to_string(),
            "Long" => "Ljava/lang/Long;".to_string(),
            "Short" => "Ljava/lang/Short;".to_string(),
            "Byte" => "Ljava/lang/Byte;".to_string(),
            "Char" => "Ljava/lang/Character;".to_string(),
            "Boolean" => "Ljava/lang/Boolean;".to_string(),
            // Unsigned primitives - box to kotlin.U* wrappers
            "UInt" => "Lkotlin/UInt;".to_string(),
            "ULong" => "Lkotlin/ULong;".to_string(),
            "UByte" => "Lkotlin/UByte;".to_string(),
            "UShort" => "Lkotlin/UShort;".to_string(),
            // Nullable reference types - just strip ? and process normally
            _ => kotlin_type_to_jvm_descriptor(base_type),
        };
    }

    // Handle non-nullable primitives
    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    match base_type {
        // Signed primitives - map to JVM primitive descriptors
        #[allow(clippy::match_same_arms)]
        // Arms separated by AST node type for documentation clarity
        "Int" => "I".to_string(),
        "Double" => "D".to_string(),
        "Float" => "F".to_string(),
        "Long" => "J".to_string(),
        "Short" => "S".to_string(),
        "Byte" => "B".to_string(),
        "Char" => "C".to_string(),
        "Boolean" => "Z".to_string(),

        // Unsigned primitives - erase to underlying signed primitive descriptors
        // (At JVM bytecode level, unsigned types use the same representation as signed)
        "UInt" => "I".to_string(),
        "ULong" => "J".to_string(),
        "UByte" => "B".to_string(),
        "UShort" => "S".to_string(),

        // Unit is a reference type in parameters (V is for return types only)
        "Unit" => "Lkotlin/Unit;".to_string(),

        // Common reference types
        "String" => "Ljava/lang/String;".to_string(),
        "Any" => "Ljava/lang/Object;".to_string(),

        // Kotlin stdlib collection types (map to java.util)
        "List" => "Ljava/util/List;".to_string(),
        "MutableList" => "Ljava/util/List;".to_string(),
        "Set" => "Ljava/util/Set;".to_string(),
        "MutableSet" => "Ljava/util/Set;".to_string(),
        "Map" => "Ljava/util/Map;".to_string(),
        "MutableMap" => "Ljava/util/Map;".to_string(),
        "Collection" => "Ljava/util/Collection;".to_string(),
        "MutableCollection" => "Ljava/util/Collection;".to_string(),
        "Iterable" => "Ljava/lang/Iterable;".to_string(),
        "MutableIterable" => "Ljava/lang/Iterable;".to_string(),

        // Primitive array types (IntArray, DoubleArray, etc.)
        type_name if type_name.ends_with("Array") => {
            if let Some(elem_type) = type_name.strip_suffix("Array") {
                let elem_descriptor = kotlin_type_to_jvm_descriptor(elem_type);
                format!("[{elem_descriptor}")
            } else {
                // Shouldn't reach here, but fallback
                "[Ljava/lang/Object;".to_string()
            }
        }

        // Generic types - use erasure (just the base type without parameters)
        type_name if type_name.contains('<') => {
            let base_generic = type_name.split('<').next().unwrap_or(type_name);
            // Recursively process base type (handles stdlib types)
            kotlin_type_to_jvm_descriptor(base_generic)
        }

        // Reference types - convert to L format
        _ => {
            // Convert package.qualified.Type to Lpackage/qualified/Type;
            // If no dots, assume kotlin package for common types
            if base_type.contains('.') {
                format!("L{};", base_type.replace('.', "/"))
            } else {
                // Unqualified reference type - could be kotlin.* or user type
                // For safety, use kotlin package as default
                format!("Lkotlin/{base_type};")
            }
        }
    }
}

/// Generate Kotlin inline-class name mangling suffix for unsigned types.
///
/// Kotlin uses name mangling for inline value classes such as `UInt` and `ULong` to avoid
/// JVM signature collisions with their underlying primitive types.
///
/// For example:
/// - `process(x: Int)` → no mangling
/// - `process(x: UInt)` → mangling suffix `-UInt`
/// - `process(x: UInt, y: ULong)` → mangling suffix `-UInt-ULong`
///
/// This ensures that `process(Int)` and `process(UInt)` generate distinct FFI targets
/// even though both erase to the same JVM descriptor (I).
///
/// Returns empty string if no unsigned types are present.
fn generate_inline_class_mangling(func_node: Node, content: &[u8]) -> String {
    let param_types = extract_parameter_types_from_function(func_node, content);

    if param_types.is_empty() {
        return String::new();
    }

    let mut manglings = Vec::new();

    for param_type in &param_types {
        // Normalize the type to strip kotlin. prefixes and variance modifiers
        let normalized = normalize_kotlin_type(param_type);

        // Strip nullable suffix to get base type
        let base_type = normalized.strip_suffix('?').unwrap_or(&normalized);

        // Check if this is an unsigned inline class type
        // This includes both scalar types (UInt, ULong) and specialized array types (UIntArray, ULongArray)
        match base_type {
            "UInt" | "ULong" | "UByte" | "UShort" | "UIntArray" | "ULongArray" | "UByteArray"
            | "UShortArray" => {
                manglings.push(base_type.to_string());
            }
            _ => {
                // Signed primitives and reference types don't need mangling
            }
        }
    }

    if manglings.is_empty() {
        String::new()
    } else {
        // Join all unsigned types with hyphens
        // Format: -UInt or -UInt-ULong for multiple unsigned params
        format!("-{}", manglings.join("-"))
    }
}

/// Generate a JVM signature suffix for a function's parameters.
///
/// Returns a string like `I_Ljava/lang/String` for `(Int, String)` parameters.
/// Descriptors are joined with underscores, semicolons are stripped for readability,
/// but slashes are preserved to distinguish primitives (I) from reference types (Ljava/lang/String).
///
/// Empty string if the function has no parameters.
fn generate_jvm_signature(func_node: Node, content: &[u8]) -> String {
    let param_types = extract_parameter_types_from_function(func_node, content);

    if param_types.is_empty() {
        return String::new();
    }

    let descriptors: Vec<String> = param_types
        .iter()
        .map(|t| kotlin_type_to_jvm_descriptor(t))
        .collect();

    // Join descriptors with underscores and strip semicolons for readability
    // Slashes are preserved to distinguish types: I_Ljava/lang/String (not I_Ljava_lang_String)
    let signature = descriptors.join("_");
    signature.replace(';', "")
}

/// Build an FFI edge for an external function.
///
/// In Kotlin, external functions are JNI bridges (JVM) or Kotlin/Native C interop.
/// The `external` modifier marks a function as implemented natively.
///
/// FFI target names include JVM signature mangling AND inline-class name mangling
/// to disambiguate overloaded methods.
///
/// Format: `<ffi:qualified.name[-MANGLING]__SIGNATURE>`
/// - MANGLING: Added for inline classes (unsigned types) to avoid signature collisions
/// - SIGNATURE: JVM descriptor sequence for parameters
///
/// # Examples
///
/// ```kotlin
/// // JNI on JVM
/// external fun getNativeLibraryPath(): String
/// // FFI target: <ffi:getNativeLibraryPath>
///
/// // Overloaded methods with signed types
/// external fun process(x: Int): String
/// // FFI target: <ffi:NativeLib.process__I>
/// external fun process(s: String): String
/// // FFI target: <ffi:NativeLib.process__Ljava_lang_String>
///
/// // Overloaded methods with unsigned types (inline classes)
/// external fun process(x: UInt): String
/// // FFI target: <ffi:NativeLib.process-UInt__I>
/// external fun process(x: UInt, y: ULong): String
/// // FFI target: <ffi:NativeLib.process-UInt-ULong__I_J>
///
/// // Kotlin/Native C interop
/// @CName("c_function")
/// external fun cFunction(x: Int): Int
/// ```
#[allow(clippy::similar_names)] // Domain variable naming is intentional
fn build_jni_external_function_edge(
    context: &CallContext,
    helper: &mut GraphBuildHelper,
    func_node: Node,
    #[allow(clippy::similar_names)] // Domain variable naming is intentional
    #[allow(clippy::similar_names)] // AST node variables
    content: &[u8],
) {
    use sqry_core::graph::unified::edge::FfiConvention;

    // Get function span for FFI edge
    let span = Span::from_node(&func_node);

    // Generate inline-class mangling suffix for unsigned types
    // Kotlin uses name mangling to distinguish inline classes (UInt, ULong, etc.)
    // from their underlying primitives (Int, Long, etc.) to avoid JVM signature collisions
    // Example: process(UInt) -> "-UInt", process(UInt, ULong) -> "-UInt-ULong"
    let mangling = generate_inline_class_mangling(func_node, content);

    // Generate JVM signature for parameters to disambiguate overloads
    // Example: (Int, String) -> "__I_Ljava_lang_String"
    let signature = generate_jvm_signature(func_node, content);

    // Create an FFI target using qualified name + mangling + signature
    // Format: <ffi:ClassName.methodName[-MANGLING]__SIGNATURE>
    // Examples:
    // - process(Int) -> <ffi:NativeLib.process__I>
    // - process(UInt) -> <ffi:NativeLib.process-UInt__I>
    // - process(UInt, ULong) -> <ffi:NativeLib.process-UInt-ULong__I_J>
    //
    // This prevents collisions between signed and unsigned overloads that
    // erase to the same JVM descriptor (e.g., Int and UInt both -> I)
    let ffi_name = match (mangling.is_empty(), signature.is_empty()) {
        (true, true) => {
            // No mangling, no params
            format!("<ffi:{}>", context.qualified_name)
        }
        (false, true) => {
            // Mangling but no signature (shouldn't happen - unsigned types create signature)
            format!("<ffi:{}{}>", context.qualified_name, mangling)
        }
        (true, false) => {
            // Signature but no mangling (normal case for signed types)
            format!("<ffi:{}__{}>", context.qualified_name, signature)
        }
        (false, false) => {
            // Both mangling and signature (unsigned types)
            format!(
                "<ffi:{}{}__{}>",
                context.qualified_name, mangling, signature
            )
        }
    };

    // Get the caller node ID (the external function/method itself)
    // Note: The node is already created in walk_tree_for_graph_with_context,
    // but we need to retrieve its ID to create the FFI edge
    // Use is_method to ensure we get/create the correct node kind
    let caller_id = if context.is_method {
        helper.add_method(
            &context.qualified_name,
            Some(span),
            context.is_async,
            false, // Kotlin methods are not static by default
        )
    } else {
        helper.add_function(
            &context.qualified_name,
            Some(span),
            context.is_async,
            false, // Kotlin doesn't have unsafe
        )
    };

    // Create a module node representing the native implementation. The span
    // in hand is the Kotlin `external` declaration's, which the native module
    // does not own (issue #748).
    let target_id = helper.add_call_site_node(&ffi_name, span, NodeKind::Module);

    // Add FFI edge (Kotlin uses C calling convention for both JNI and Kotlin/Native)
    helper.add_ffi_edge(caller_id, target_id, FfiConvention::C);
}

// ============================================================================
// Local variable reference resolution
// ============================================================================

/// Handle a `simple_identifier` node for local variable reference resolution.
fn handle_identifier_for_reference(
    node: Node,
    content: &[u8],
    ast_graph: &ASTGraph,
    scope_tree: &mut KotlinScopeTree,
    helper: &mut GraphBuildHelper,
) {
    let identifier_text = node.utf8_text(content).unwrap_or("").trim();
    if identifier_text.is_empty() {
        return;
    }

    // Skip if this identifier is part of a declaration
    if is_declaration_context(node) {
        return;
    }

    // For call names like `foo()`, only skip if the identifier is NOT a local variable.
    // Function-typed locals like `val f = {}; f()` should still produce Reference edges.
    if is_call_name(node) && !scope_tree.has_local_binding(identifier_text, node.start_byte()) {
        return;
    }

    // Skip type annotation contexts
    if is_type_context(node) {
        return;
    }

    // Skip import contexts
    if is_import_context(node) {
        return;
    }

    // Skip navigation member access: `obj.member` → skip `member`
    if is_navigation_member(node) {
        return;
    }

    // Skip label references: `break@label`, `continue@label`
    if is_label_context(node) {
        return;
    }

    // Skip named argument labels: `foo(name = value)` → skip `name`
    if is_named_argument_label(node) {
        return;
    }

    match scope_tree.resolve_identifier(node.start_byte(), identifier_text) {
        ResolutionOutcome::Local(binding) => {
            let target_id = if let Some(node_id) = binding.node_id {
                node_id
            } else {
                let span = binding.decl_span;
                let qualified_var = format!("{identifier_text}@{}", binding.decl_start_byte);
                let var_id = helper.add_variable(&qualified_var, Some(span));
                // issue #394: real declaration (local binding materialized with its
                // declaration span); opt dual-use bare helper into is_definition
                helper.mark_definition(var_id);
                scope_tree.attach_node_id(identifier_text, binding.decl_start_byte, var_id);
                var_id
            };

            // Create Reference edge from enclosing callable to the variable
            if let Some(context) = ast_graph.get_enclosing_callable_context(node) {
                let caller_id = helper.ensure_callee(
                    context.qualified_name(),
                    Span::from_node(&node),
                    CalleeKindHint::Function,
                );
                helper.add_reference_edge(caller_id, target_id);
            }
        }
        ResolutionOutcome::Member { .. }
        | ResolutionOutcome::Ambiguous
        | ResolutionOutcome::NoMatch => {}
    }
}

/// Check if the identifier node is in a declaration context (not a usage).
#[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
fn is_declaration_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    match parent.kind() {
        // property_declaration: `val x = ...` → `x` is declaration
        "property_declaration" => {
            // The first simple_identifier child of property_declaration is the name
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
                // Skip modifier keywords
                if child.kind() == "modifiers" || child.kind() == "val" || child.kind() == "var" {
                    continue;
                }
                break;
            }
            false
        }
        // variable_declaration inside for or destructuring: `val (a, b)` or `for (x in list)`
        "variable_declaration" => true,
        // parameter_with_optional_type: `set(value)` → `value` is declaration
        "parameter_with_optional_type" => {
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
            }
            false
        }
        // parameter: `fun foo(x: Int)` → `x` is declaration
        "parameter" => {
            // First simple_identifier in parameter is the name
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
            }
            false
        }
        // class_parameter: `class Foo(val x: Int)` → `x` is declaration
        "class_parameter" => {
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
                if child.kind() == "modifiers" || child.kind() == "val" || child.kind() == "var" {
                    // Skip modifier keywords in property declarations
                }
            }
            false
        }
        // function_declaration: `fun foo()` → `foo` is declaration
        "function_declaration" => {
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
                // Skip modifiers, fun keyword, type parameters, etc.
                if matches!(
                    child.kind(),
                    "modifiers" | "fun" | "type_parameters" | "user_type"
                ) {
                    continue;
                }
                break;
            }
            false
        }
        // catch_block: `catch (e: Exception)` → `e` is declaration
        "catch_block" => {
            // The first simple_identifier in catch_block is the exception variable
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "simple_identifier" {
                    return child.id() == node.id();
                }
            }
            false
        }
        // Lambda parameter context
        "lambda_parameters" => true,
        _ => false,
    }
}

/// Check if the identifier is the callee name in a `call_expression`.
fn is_call_name(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    // Direct call: `foo()` where simple_identifier is direct child of call_expression
    if parent.kind() == "call_expression" {
        // Check if this identifier is the callee (first child), not an argument
        let mut cursor = parent.walk();
        if let Some(first) = parent.children(&mut cursor).next() {
            return first.id() == node.id();
        }
    }
    false
}

/// Check if the identifier is in a type annotation context.
fn is_type_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    // type_identifier is a different node kind, but simple_identifier
    // can appear inside user_type
    matches!(
        parent.kind(),
        "user_type" | "type_identifier" | "type_projection" | "type_constraint"
    )
}

/// Check if the identifier is inside an import.
fn is_import_context(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "import_header" {
            return true;
        }
        // Don't traverse too far
        if matches!(
            parent.kind(),
            "class_declaration"
                | "function_declaration"
                | "property_declaration"
                | "object_declaration"
        ) {
            return false;
        }
        current = parent.parent();
    }
    false
}

/// Check if the identifier is a navigation member: `obj.member` → `member` is navigation.
fn is_navigation_member(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    // navigation_suffix contains the member name in `obj.member`
    parent.kind() == "navigation_suffix"
}

/// Check if the identifier is in a label context: `@loop` in `break@loop`
fn is_label_context(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "label"
}

/// Check if the identifier is a named argument label: `foo(name = value)` → `name` is a label.
fn is_named_argument_label(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    // In Kotlin AST: value_argument has children: [simple_identifier, "=", expression]
    // The named argument label is the first simple_identifier child before "="
    if parent.kind() == "value_argument" {
        // Check if there's an "=" sibling after this identifier
        let mut cursor = parent.walk();
        let mut found_self = false;
        for child in parent.children(&mut cursor) {
            if child.id() == node.id() {
                found_self = true;
                continue;
            }
            if found_self && child.kind() == "=" {
                return true;
            }
        }
    }
    false
}

/// Emit per-type-parameter `Type` nodes for a generic Kotlin
/// declaration (class or function) and `TypeOf{Constraint}` edges for
/// inline bounds and `where`-clause constraints.
///
/// Tree-sitter-kotlin grammar shape (no field names — children are
/// positional / kind-based):
///
/// ```text
/// type_parameters: '<' commaSep1(type_parameter) '>'
/// type_parameter:  type_parameter_modifiers? type_identifier (':' type)?
/// type_parameter_modifiers: (annotation | reification_modifier | variance_modifier)+
/// type_constraints: 'where' commaSep1(type_constraint)
/// type_constraint:  annotation* type_identifier ':' type
/// ```
///
/// Per design §4.15: `reified` and variance (`in` / `out`) modifiers
/// emit only the base Type node — the modifier-as-attribute extension
/// is deferred. The base node is sufficient for "find generic
/// type-parameter declarations" semantic queries.
///
/// The qualified name shape is `<parent>.<ParamName>`; the caller
/// provides `parent_qualified_name`:
///
/// * top-level `fun <T> id(...)` →  `id`
/// * member   `class Box { fun <T> wrap(...) }` →  `Box.wrap`
/// * class    `class Container<T>` →  `Container`
///
/// `canonicalize_graph_qualified_name` later rewrites the `.`
/// separators to `::` for graph-internal storage.
fn process_type_parameter_declarations(
    decl_node: Node,
    content: &[u8],
    parent_qualified_name: &str,
    helper: &mut GraphBuildHelper,
) {
    // 1. Iterate the `type_parameters` child (declaration-site `<T, U>`).
    let mut decl_cursor = decl_node.walk();
    let params_node = decl_node
        .children(&mut decl_cursor)
        .find(|child| child.kind() == "type_parameters");

    // Map from type-parameter identifier text → Type-node id, so that
    // a `where T : A` clause can attach the constraint edge to the
    // already-emitted parameter node rather than synthesising a new one.
    let mut param_ids: HashMap<String, sqry_core::graph::unified::node::NodeId> = HashMap::new();

    if let Some(params_node) = params_node {
        let mut cursor = params_node.walk();
        for param_node in params_node.children(&mut cursor) {
            if param_node.kind() != "type_parameter" {
                continue;
            }

            let Some(name_node) = first_type_parameter_name_node(param_node) else {
                continue;
            };
            let Ok(param_name) = name_node.utf8_text(content) else {
                continue;
            };

            let qualified_param = format!("{parent_qualified_name}.{param_name}");
            let span = Span::from_node(&name_node);
            let param_id = helper.add_type(&qualified_param, Some(span));
            param_ids.insert(param_name.to_string(), param_id);

            // Inline bound: `<T : Number>`. The bound type is the first
            // named child after the `:` token.
            if let Some(bound_node) = first_type_parameter_bound_node(param_node) {
                emit_type_parameter_constraint(bound_node, content, param_id, helper);
            }
        }
    }

    // 2. `where T : A, T : B` — function-declaration / class-declaration
    //    `type_constraints` child. Each `type_constraint` produces one
    //    Constraint edge, attached to the matching declaration-site
    //    type-parameter node when one exists. (When the where-clause
    //    references a parameter that wasn't declared in `<...>` — which
    //    is invalid Kotlin but defensively tolerated — the constraint
    //    is silently dropped rather than synthesising a stub.)
    let mut decl_cursor2 = decl_node.walk();
    for child in decl_node.children(&mut decl_cursor2) {
        if child.kind() != "type_constraints" {
            continue;
        }
        let mut tc_cursor = child.walk();
        for tc in child.children(&mut tc_cursor) {
            if tc.kind() != "type_constraint" {
                continue;
            }
            // First named child is the type-parameter identifier;
            // subsequent type-typed children form the bound.
            let mut named: Vec<Node> = Vec::new();
            let mut tc_inner = tc.walk();
            for c in tc.children(&mut tc_inner) {
                if c.is_named() && c.kind() != "annotation" {
                    named.push(c);
                }
            }
            if named.len() < 2 {
                continue;
            }
            let Ok(param_name) = named[0].utf8_text(content) else {
                continue;
            };
            let Some(&param_id) = param_ids.get(param_name) else {
                continue;
            };
            emit_type_parameter_constraint(named[1], content, param_id, helper);
        }
    }
}

/// Pick the parameter-name `type_identifier` child of a `type_parameter`
/// node — skipping past any leading modifiers (annotations, `reified`,
/// `in`, `out`).
fn first_type_parameter_name_node(param_node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = param_node.walk();
    param_node
        .children(&mut cursor)
        .find(|child| child.kind() == "type_identifier")
}

/// Find the bound-type node of a `type_parameter`. The grammar lists
/// the bound as any type-typed named child appearing AFTER the
/// parameter-name `type_identifier`. Returns `None` when the parameter
/// is unbounded (`<T>` rather than `<T : Bound>`).
fn first_type_parameter_bound_node(param_node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = param_node.walk();
    let mut seen_name = false;
    for child in param_node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if !seen_name {
            if child.kind() == "type_identifier" {
                seen_name = true;
            }
            continue;
        }
        // Any type-typed named child after the name identifier is the
        // bound (`user_type`, `nullable_type`, `function_type`,
        // `parenthesized_type`, or another `type_identifier`).
        if matches!(
            child.kind(),
            "user_type"
                | "nullable_type"
                | "function_type"
                | "parenthesized_type"
                | "not_nullable_type"
                | "type_identifier"
        ) {
            return Some(child);
        }
    }
    None
}

/// Emit a single `TypeOf{Constraint}` edge from the type-parameter
/// node to the bound type.
///
/// The constraint target is created via `helper.add_type(name, None)`:
/// like Java's `emit_type_bound_constraints`, the target is a synthetic
/// reference stub that may be referenced from many distinct
/// type-parameter declarations and therefore has no single source span.
/// Cross-file unification (Phase 4c-prime) collapses these stubs into
/// the canonical declaration when one exists.
fn emit_type_parameter_constraint(
    bound_node: Node,
    content: &[u8],
    param_id: sqry_core::graph::unified::node::NodeId,
    helper: &mut GraphBuildHelper,
) {
    let bound_name = extract_bound_type_base_name(bound_node, content);
    if bound_name.is_empty() {
        return;
    }
    let constraint_id = helper.add_type(&bound_name, None);
    helper.add_typeof_edge_with_context(
        param_id,
        constraint_id,
        Some(TypeOfContext::Constraint),
        None,
        None,
    );
}

/// Extract the base type name from a constraint bound, stripping any
/// generic type arguments and nullable markers. Mirrors Java's
/// `extract_bound_type_base_name` but adapted for the Kotlin grammar:
///
/// * `user_type` → walk to the first `type_identifier` (drops type
///   arguments such as `Comparable<T>` → `Comparable`).
/// * `nullable_type` → unwrap to inner type.
/// * `type_identifier` → raw text.
/// * other type kinds (`function_type`, `parenthesized_type`,
///   `not_nullable_type`) fall back to raw text.
fn extract_bound_type_base_name(type_node: Node, content: &[u8]) -> String {
    match type_node.kind() {
        "user_type" => {
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    return child.utf8_text(content).unwrap_or("").trim().to_string();
                }
                if child.kind() == "user_type" {
                    return extract_bound_type_base_name(child, content);
                }
            }
            type_node
                .utf8_text(content)
                .unwrap_or("")
                .trim()
                .to_string()
        }
        "nullable_type" => {
            let mut cursor = type_node.walk();
            for child in type_node.children(&mut cursor) {
                if child.is_named() {
                    return extract_bound_type_base_name(child, content);
                }
            }
            String::new()
        }
        _ => type_node
            .utf8_text(content)
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

/// Per-language [`ShapeMapping`] for Kotlin: a precomputed `kind_id -> CfBucket`
/// table over the tree-sitter-kotlin grammar, shared process-wide via
/// [`kotlin_shape_mapping`]. Same shape as the C reference impl: one array index
/// per node on the hot walk, identifier-blind throughout.
pub struct KotlinShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl KotlinShapeMapping {
    fn build() -> Self {
        // tree-sitter-kotlin-sqry exposes `language()` returning a `Language`
        // directly, not a `LanguageFn`, so there is no `.into()` here.
        let lang: tree_sitter::Language = tree_sitter_kotlin_sqry::language();
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
                *slot = cf_bucket_for_kotlin_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for KotlinShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // A `function_declaration` holds its parameters in a
        // `function_value_parameters` child (by kind, not field); count its
        // `parameter` children for the positional arity.
        let mut cursor = fn_node.walk();
        for child in fn_node.named_children(&mut cursor) {
            if child.kind() == "function_value_parameters" {
                let mut pcursor = child.walk();
                for param in child.named_children(&mut pcursor) {
                    if param.kind() == "parameter" {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                    }
                }
            }
        }
        shape.has_return_annotation = kotlin_has_return_type(fn_node);
        shape
    }
}

/// Whether a Kotlin `function_declaration` declares a return type. The grammar
/// exposes it either through a `type` field or, when unfielded, as a type node
/// sitting after the `function_value_parameters` child (the same lookup the
/// plugin's `extract_return_type` performs).
fn kotlin_has_return_type(fn_node: Node) -> bool {
    if fn_node.child_by_field_name("type").is_some() {
        return true;
    }
    let mut cursor = fn_node.walk();
    let mut passed_params = false;
    for child in fn_node.named_children(&mut cursor) {
        match child.kind() {
            "function_value_parameters" => passed_params = true,
            "user_type" | "nullable_type" | "not_nullable_type" | "function_type"
            | "parenthesized_type"
                if passed_params =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Map one tree-sitter-kotlin grammar node-kind name to its canonical
/// control-flow bucket. Additive-only against the frozen [`CfBucket`] set.
fn cf_bucket_for_kotlin_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_expression" => CfBucket::Branch,
        "for_statement" | "while_statement" | "do_while_statement" => CfBucket::Loop,
        "when_expression" | "when_entry" | "when_condition" => CfBucket::Match,
        "try_expression" => CfBucket::Try,
        "catch_block" => CfBucket::Catch,
        "finally_block" => CfBucket::Resource,
        // Kotlin uses one `jump_expression` node for return / break / continue /
        // throw; the histogram cannot disambiguate those structurally, so the
        // whole jump family maps onto BreakContinue.
        "jump_expression" => CfBucket::BreakContinue,
        "call_expression"
        | "infix_expression"
        | "constructor_invocation"
        | "constructor_delegation_call" => CfBucket::Call,
        "assignment"
        | "property_declaration"
        | "variable_declaration"
        | "multi_variable_declaration" => CfBucket::Assign,
        "lambda_literal" | "anonymous_function" | "annotated_lambda" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Kotlin shape mapping, built once on first use.
#[must_use]
pub fn kotlin_shape_mapping() -> &'static KotlinShapeMapping {
    static MAPPING: OnceLock<KotlinShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(KotlinShapeMapping::build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use tree_sitter::Parser;

    fn parse_kotlin(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_sqry::language())
            .expect("Failed to set Kotlin language");
        parser
            .parse(source.as_bytes(), None)
            .expect("Failed to parse Kotlin source")
    }

    #[test]
    fn test_extract_class() {
        let source = "class User { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        assert!(
            !nodes.is_empty(),
            "Expected at least 1 node, got {}",
            nodes.len()
        );
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n.entry.kind, NodeKind::Class))
        );
        assert!(nodes.iter().any(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("User"))
        }));
    }

    #[test]
    fn test_extract_data_class() {
        let source = "data class Person(val name: String, val age: Int)";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        assert!(nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Class)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("Person"))
        }));
    }

    #[test]
    fn test_extract_function() {
        let source = "fun hello() { println(\"Hello\") }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        assert!(nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("hello"))
        }));
    }

    #[test]
    fn test_extract_suspend_function() {
        let source = "suspend fun fetchData() { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        assert!(nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && n.entry.is_async
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("fetchData"))
        }));
    }

    #[test]
    fn test_extract_call_edge() {
        let source = r#"
            fun main() {
                println("Hello")
            }
        "#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        assert!(nodes.iter().any(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("main"))
        }));
        assert!(nodes.iter().any(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("println"))
        }));

        let edges: Vec<_> = staging.edges().collect();
        assert!(
            edges
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Calls { .. }))
        );
    }

    #[test]
    fn test_property_access_does_not_create_call_edge() {
        // Property access like `user.id` should NOT create a call edge.
        // Only actual calls like `user.getId()` should create edges.
        let source = r#"
            class User(val id: Int, val name: String)

            fun main() {
                val user = User(1, "Alice")
                val userId = user.id
                val userName = user.name
            }
        "#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Verify no call edges were created for property access
        // The only call should be to User constructor, not `id` or `name` property access
        let ops = staging.operations();
        let call_edge_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .count();

        // Property access `user.id` and `user.name` are NOT calls, only `User(1, "Alice")` is.
        // So we expect exactly 1 call edge (the User constructor call).
        assert!(
            call_edge_count <= 1,
            "Property access should not create call edges, but found {call_edge_count} call edges"
        );
    }

    #[test]
    fn test_method_call_creates_call_edge() {
        // Method calls like `user.getName()` SHOULD create call edges.
        let source = r#"
            class User {
                fun getName(): String = "Alice"
            }

            fun main() {
                val user = User()
                val name = user.getName()
            }
        "#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Verify call edge was created for method call
        let ops = staging.operations();
        let call_edge_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .count();

        // Should have at least one call edge for `user.getName()` and/or `User()` constructor
        assert!(
            call_edge_count >= 1,
            "Method call 'getName()' should create a call edge, but found {call_edge_count} call edges"
        );
    }

    #[test]
    fn test_class_inheritance_creates_inherits_edge() {
        // class Child : Parent() should create an Inherits edge
        let source = r#"
            open class Parent {
                fun greet() = "Hello"
            }

            class Child : Parent() {
                fun wave() = "Hi"
            }
        "#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Check for Inherits edge
        let ops = staging.operations();
        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge for Child : Parent(), found {inherits_count}"
        );
    }

    #[test]
    fn test_interface_implementation_creates_implements_edge() {
        // class Foo : IClickable should create an Implements edge
        let source = r#"
            interface IClickable {
                fun click()
            }

            class Button : IClickable {
                override fun click() { println("clicked") }
            }
        "#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Check for Implements edge
        let ops = staging.operations();
        let implements_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Implements,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            implements_count, 1,
            "Expected 1 Implements edge for Button : IClickable, found {implements_count}"
        );
    }

    #[test]
    fn test_class_with_multiple_supertypes() {
        // class Foo : Parent(), IClickable, Runnable should create 1 Inherits + 2 Implements
        let source = r"
            open class Parent
            interface IClickable { fun click() }
            interface Runnable { fun run() }

            class Widget : Parent(), IClickable, Runnable {
                override fun click() { }
                override fun run() { }
            }
        ";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ops = staging.operations();

        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        let implements_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Implements,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge for Widget : Parent(), found {inherits_count}"
        );
        assert_eq!(
            implements_count, 2,
            "Expected 2 Implements edges for Widget : IClickable, Runnable, found {implements_count}"
        );
    }

    #[test]
    fn test_is_interface_name_heuristics() {
        // Test the interface name heuristic function
        assert!(is_interface_name("IClickable"));
        assert!(is_interface_name("ISerializable"));
        assert!(is_interface_name("Clickable")); // -able suffix
        assert!(is_interface_name("Runnable"));
        assert!(is_interface_name("OnClickListener")); // -Listener suffix
        assert!(is_interface_name("EventHandler")); // -Handler suffix
        assert!(is_interface_name("DataProvider")); // -Provider suffix
        assert!(is_interface_name("UserRepository")); // -Repository suffix

        assert!(!is_interface_name("Parent"));
        assert!(!is_interface_name("User"));
        assert!(!is_interface_name("Button"));
        assert!(!is_interface_name("String"));
    }

    // ==========================================================================
    // Import Edge Tests
    // ==========================================================================

    /// Helper to count Import edges and extract their metadata
    fn count_import_edges(staging: &StagingGraph) -> Vec<(bool, Option<String>)> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let sqry_core::graph::unified::StagingOp::AddEdge {
                    kind: sqry_core::graph::unified::EdgeKind::Imports { is_wildcard, alias },
                    ..
                } = op
                {
                    // For alias, we just check if it's Some (we can't easily resolve the StringId here)
                    Some((*is_wildcard, alias.map(|_| "has_alias".to_string())))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn test_simple_import_creates_import_edge() {
        // import com.example.MyClass should create an Import edge
        let source = "import com.example.MyClass";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Expected 1 Import edge, found {}",
            import_edges.len()
        );

        // Simple import should have is_wildcard=false and no alias
        let (is_wildcard, alias) = &import_edges[0];
        assert!(!is_wildcard, "Simple import should not be wildcard");
        assert!(alias.is_none(), "Simple import should not have alias");
    }

    #[test]
    fn test_aliased_import_creates_import_edge_with_alias() {
        // import com.example.MyClass as MC should create an Import edge with alias
        let source = "import com.example.MyClass as MC";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Expected 1 Import edge, found {}",
            import_edges.len()
        );

        // Aliased import should have is_wildcard=false and have an alias
        let (is_wildcard, alias) = &import_edges[0];
        assert!(!is_wildcard, "Aliased import should not be wildcard");
        assert!(alias.is_some(), "Aliased import should have alias");
    }

    #[test]
    fn test_wildcard_import_creates_import_edge_with_wildcard() {
        // import com.example.* should create an Import edge with is_wildcard=true
        let source = "import com.example.*";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Expected 1 Import edge, found {}",
            import_edges.len()
        );

        // Wildcard import should have is_wildcard=true
        let (is_wildcard, _alias) = &import_edges[0];
        assert!(is_wildcard, "Wildcard import should have is_wildcard=true");
    }

    #[test]
    fn test_multiple_imports_create_multiple_edges() {
        // Multiple imports should create multiple Import edges
        let source = r"
import kotlin.collections.List
import java.util.HashMap
import javax.swing.*
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            3,
            "Expected 3 Import edges for 3 imports, found {}",
            import_edges.len()
        );

        // Count wildcards
        let wildcard_count = import_edges.iter().filter(|(w, _)| *w).count();
        assert_eq!(
            wildcard_count, 1,
            "Expected 1 wildcard import (javax.swing.*), found {wildcard_count}"
        );
    }

    #[test]
    fn test_import_with_code_creates_correct_edges() {
        // Import statements with class definitions should create both Import and other edges
        let source = r#"
import com.example.service.UserService
import com.example.util.Logger as Log

class UserController {
    fun handleRequest() {
        println("handling")
    }
}
"#;
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ops = staging.operations();

        // Check for import edges
        let import_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Imports { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            import_count, 2,
            "Expected 2 Import edges, found {import_count}"
        );

        // Check that class was also created (verifying imports don't break other processing)
        let class_count = ops
            .iter()
            .filter(|op| {
                if let sqry_core::graph::unified::StagingOp::AddNode { entry, .. } = op {
                    matches!(entry.kind, sqry_core::graph::unified::NodeKind::Class)
                } else {
                    false
                }
            })
            .count();

        assert!(
            class_count >= 1,
            "Expected at least 1 Class node for UserController"
        );
    }

    #[test]
    fn test_kotlin_standard_library_imports() {
        // Test common Kotlin standard library import patterns
        let source = r"
import kotlin.collections.mutableListOf
import kotlin.text.StringBuilder
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.*
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            4,
            "Expected 4 Import edges, found {}",
            import_edges.len()
        );
    }

    #[test]
    fn test_android_imports() {
        // Test Android-specific import patterns
        let source = r"
import android.app.Activity
import android.os.Bundle
import android.view.View as V
import android.widget.*
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            4,
            "Expected 4 Import edges, found {}",
            import_edges.len()
        );

        // Check for alias and wildcard
        let has_alias = import_edges.iter().any(|(_, a)| a.is_some());
        let has_wildcard = import_edges.iter().any(|(w, _)| *w);

        assert!(
            has_alias,
            "Expected at least one aliased import (View as V)"
        );
        assert!(
            has_wildcard,
            "Expected at least one wildcard import (android.widget.*)"
        );
    }

    #[test]
    fn test_nested_package_import() {
        // Test deeply nested package imports
        let source = "import org.springframework.boot.autoconfigure.SpringBootApplication";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Expected 1 Import edge for deeply nested import"
        );
    }

    #[test]
    fn test_single_identifier_import() {
        // Test single-word import (rare but valid)
        let source = "import SomeClass";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Expected 1 Import edge for single identifier import"
        );
    }

    #[test]
    fn test_import_does_not_affect_class_processing() {
        // Verify that import processing doesn't interfere with class/OOP processing
        let source = r"
import com.example.BaseClass

open class Parent
class Child : Parent()
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ops = staging.operations();

        // Check import edge
        let import_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Imports { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(import_count, 1, "Expected 1 Import edge");

        // Check inherits edge still works
        let inherits_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Inherits,
                        ..
                    }
                )
            })
            .count();

        assert_eq!(
            inherits_count, 1,
            "Expected 1 Inherits edge (Child : Parent)"
        );
    }

    #[test]
    fn test_mixed_import_styles() {
        // Test file with all import styles together
        let source = r"
import kotlin.io.path.Path
import kotlin.io.path.pathString as ps
import kotlin.collections.*
import kotlinx.coroutines.flow.Flow as F
import java.util.*
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let import_edges = count_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            5,
            "Expected 5 Import edges, found {}",
            import_edges.len()
        );

        // Count each type
        let simple_count = import_edges
            .iter()
            .filter(|(w, a)| !*w && a.is_none())
            .count();
        let alias_count = import_edges
            .iter()
            .filter(|(w, a)| !*w && a.is_some())
            .count();
        let wildcard_count = import_edges.iter().filter(|(w, _)| *w).count();

        assert_eq!(simple_count, 1, "Expected 1 simple import");
        assert_eq!(alias_count, 2, "Expected 2 aliased imports");
        assert_eq!(wildcard_count, 2, "Expected 2 wildcard imports");
    }

    // ==========================================================================
    // Export Edge Tests
    // ==========================================================================

    /// Helper to count Export edges
    fn count_export_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::Exports { .. },
                        ..
                    }
                )
            })
            .count()
    }

    #[test]
    fn test_public_class_creates_export_edge() {
        // Public class should create an Export edge
        let source = "class User { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 1,
            "Expected 1 Export edge for public class, found {export_count}"
        );
    }

    #[test]
    fn test_private_class_no_export_edge() {
        // Private class should NOT create an Export edge
        let source = "private class Internal { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for private class, found {export_count}"
        );
    }

    #[test]
    fn test_internal_class_no_export_edge() {
        // Internal class should NOT create an Export edge
        let source = "internal class ModuleInternal { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for internal class, found {export_count}"
        );
    }

    #[test]
    fn test_public_function_creates_export_edge() {
        // Public function should create an Export edge
        let source = "fun greet(name: String) { println(name) }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 1,
            "Expected 1 Export edge for public function, found {export_count}"
        );
    }

    #[test]
    fn test_private_function_no_export_edge() {
        // Private function should NOT create an Export edge
        let source = "private fun helper() { }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for private function, found {export_count}"
        );
    }

    #[test]
    fn test_object_declaration_creates_export_edge() {
        // Object declaration should create an Export edge (but not for methods inside)
        let source = "object Database { fun connect() { } }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        // Expect exactly 1: one for object (methods inside are not exported separately)
        assert_eq!(
            export_count, 1,
            "Expected 1 Export edge for object (not methods inside), found {export_count}"
        );
    }

    #[test]
    fn test_mixed_visibility_exports() {
        // Test file with mixed visibility modifiers
        let source = r"
class PublicClass { }
private class PrivateClass { }
internal class InternalClass { }

fun publicFunction() { }
private fun privateFunction() { }
internal fun internalFunction() { }

object Singleton { }
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        // Should export: PublicClass, publicFunction, Singleton = 3
        assert_eq!(
            export_count, 3,
            "Expected 3 Export edges (public class, public function, object), found {export_count}"
        );
    }

    #[test]
    fn test_interface_node_kind() {
        // Test to understand how interfaces are represented in tree-sitter-kotlin AST
        let source = "interface Repository { fun save() }";
        let tree = parse_kotlin(source);

        #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
        // Walk the tree to find what node kind is used for interface
        fn find_node_kinds(node: tree_sitter::Node) -> Vec<String> {
            let mut kinds = vec![node.kind().to_string()];
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    kinds.extend(find_node_kinds(child));
                }
            }
            kinds
        }

        let kinds = find_node_kinds(tree.root_node());

        // Interfaces in Kotlin are typically parsed as class_declaration nodes
        // with "interface" as a modifier keyword
        assert!(
            kinds.contains(&"class_declaration".to_string()),
            "Expected interface to be parsed as class_declaration, found: {kinds:?}"
        );
    }

    #[test]
    fn test_interface_creates_export_edge() {
        // Interface should create an Export edge (but not for methods inside)
        let source = "interface Repository { fun save(item: Int) }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        // Expect exactly 1: one for interface (methods inside are not exported separately)
        assert_eq!(
            export_count, 1,
            "Expected 1 Export edge for interface (not methods inside), found {export_count}"
        );
    }

    #[test]
    fn test_private_interface_no_export() {
        // Private interface should NOT create an Export edge
        let source = "private interface InternalRepo { fun load() }";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for private interface, found {export_count}"
        );
    }

    // ================================
    // FFI Detection Tests (JNI / Kotlin Native)
    // ================================

    #[test]
    fn test_external_function_creates_ffi_edge() {
        // external fun should create an FfiCall edge from a Function node
        let source = "external fun getNativeLibraryPath(): String";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 1,
            "Expected 1 FfiCall edge for external function, found {ffi_count}"
        );

        // Verify the source node is a Function, not a Method
        let nodes: Vec<_> = staging.nodes().collect();
        let external_func = nodes.iter().find(|n| {
            staging.resolve_node_name(n.entry).is_some_and(|name| {
                name == "getNativeLibraryPath" && matches!(n.entry.kind, NodeKind::Function)
            })
        });
        assert!(
            external_func.is_some(),
            "Expected external function to be NodeKind::Function with exact name"
        );

        // Verify FFI target uses qualified name (exact match)
        let ffi_target = nodes.iter().find(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == "<ffi:getNativeLibraryPath>")
        });
        assert!(
            ffi_target.is_some(),
            "Expected FFI target <ffi:getNativeLibraryPath>"
        );
    }

    #[test]
    fn test_external_method_in_class_creates_ffi_edge() {
        // external fun inside a class should create an FfiCall edge from a Method node
        let source = r"
class NativeLib {
    external fun nativeMethod(): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 1,
            "Expected 1 FfiCall edge for external method, found {ffi_count}"
        );

        // Verify the source node uses canonical graph identity and native display.
        let nodes: Vec<_> = staging.nodes().collect();
        let external_method = nodes.iter().find(|n| {
            staging
                .resolve_node_canonical_name(n.entry)
                .is_some_and(|name| {
                    name == "NativeLib::nativeMethod" && matches!(n.entry.kind, NodeKind::Method)
                })
        });
        assert!(
            external_method.is_some(),
            "Expected external method to be NodeKind::Method with canonical qualified name"
        );
        assert_eq!(
            external_method
                .and_then(|n| staging.resolve_node_display_name(Language::Kotlin, n.entry)),
            Some("NativeLib.nativeMethod".to_string()),
            "Expected external method display name to preserve Kotlin-native qualification"
        );

        // Verify FFI target uses qualified name (exact match)
        let ffi_target = nodes.iter().find(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == "<ffi:NativeLib.nativeMethod>")
        });
        assert!(
            ffi_target.is_some(),
            "Expected FFI target <ffi:NativeLib.nativeMethod>"
        );
    }

    #[test]
    fn test_multiple_external_functions() {
        // Multiple external functions should each create an FfiCall edge
        let source = r"
external fun loadLibrary(path: String): Boolean
external fun unloadLibrary(): Unit
external fun getVersion(): Int
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 3,
            "Expected 3 FfiCall edges for 3 external functions, found {ffi_count}"
        );
    }

    #[test]
    fn test_external_suspend_function() {
        // external suspend fun should create an FfiCall edge and mark as async
        let source = "external suspend fun asyncNative(): String";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 1,
            "Expected 1 FfiCall edge for external suspend function, found {ffi_count}"
        );

        // Check that function is marked as async
        let nodes: Vec<_> = staging.nodes().collect();
        let has_async_external = nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && n.entry.is_async
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("asyncNative"))
        });
        assert!(
            has_async_external,
            "Expected external suspend function to be marked as async"
        );
    }

    #[test]
    fn test_non_external_function_no_ffi_edge() {
        // Regular (non-external) functions should NOT create FfiCall edges
        let source = "fun regularFunction(): String = \"Hello\"";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 0,
            "Expected 0 FfiCall edges for regular function, found {ffi_count}"
        );
    }

    #[test]
    fn test_external_function_in_companion_object() {
        // external fun inside companion object should create an FfiCall edge
        let source = r"
class NativeLib {
    companion object {
        external fun loadLibrary(name: String): Boolean
    }
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 1,
            "Expected 1 FfiCall edge for external function in companion object, found {ffi_count}"
        );
    }

    #[test]
    fn test_external_private_function() {
        // private external fun should still create an FfiCall edge (but not Export edge)
        let source = "private external fun nativeHelper(): String";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 1,
            "Expected 1 FfiCall edge for private external function, found {ffi_count}"
        );

        let export_count = count_export_edges(&staging);
        assert_eq!(
            export_count, 0,
            "Expected 0 Export edges for private external function, found {export_count}"
        );
    }

    #[test]
    fn test_overloaded_external_methods_create_distinct_ffi_targets() {
        // Overloaded external methods should create distinct FFI targets with signature mangling
        let source = r"
class NativeLib {
    external fun process(x: Int): String
    external fun process(s: String): String
    external fun process(x: Int, y: Int): String
    external fun compute(): Int
    external fun compute(value: Double): Double
}

external fun topLevel(n: Int): String
external fun topLevel(s: String): String
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Should have 7 FfiCall edges (5 methods in class + 2 top-level functions)
        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 7,
            "Expected 7 FfiCall edges for 7 overloaded external functions, found {ffi_count}"
        );

        // Collect all node names for verification
        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        // Collect FFI targets for validation
        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify distinct FFI targets for process() overloads
        // Note: JVM descriptors use I for Int, Ljava/lang/String for String (with slashes, without semicolons)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I>"),
            "Expected FFI target <ffi:NativeLib.process__I> for process(Int), found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/String>"),
            "Expected FFI target <ffi:NativeLib.process__Ljava/lang/String> for process(String), found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I_I>"),
            "Expected FFI target <ffi:NativeLib.process__I_I> for process(Int, Int), found: {ffi_targets:?}"
        );

        // Verify distinct FFI targets for compute() overloads
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.compute>"),
            "Expected FFI target <ffi:NativeLib.compute> for compute() with no params, found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.compute__D>"),
            "Expected FFI target <ffi:NativeLib.compute__D> for compute(Double), found: {ffi_targets:?}"
        );

        // Verify distinct FFI targets for top-level overloads
        assert!(
            node_names.iter().any(|name| name == "<ffi:topLevel__I>"),
            "Expected FFI target <ffi:topLevel__I> for topLevel(Int), found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:topLevel__Ljava/lang/String>"),
            "Expected FFI target <ffi:topLevel__Ljava/lang/String> for topLevel(String), found: {ffi_targets:?}"
        );

        // Verify all FFI targets are unique (no collisions)
        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            ffi_targets.len(),
            unique_ffi_targets.len(),
            "Expected all FFI targets to be unique, but found duplicates"
        );
    }

    #[test]
    fn test_nullable_primitive_overloads_create_distinct_ffi_targets() {
        // Nullable primitives should map to boxed types (Ljava/lang/Integer;)
        // Non-nullable primitives should map to primitive descriptors (I)
        let source = r"
class NativeLib {
    external fun process(x: Int): String
    external fun process(x: Int?): String
    external fun process(flag: Boolean): String
    external fun process(flag: Boolean?): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        // Should have 4 distinct FFI targets
        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for nullable vs non-nullable overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Int vs Int? have distinct targets
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I>"),
            "Expected <ffi:NativeLib.process__I> for process(Int), found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/Integer>"),
            "Expected <ffi:NativeLib.process__Ljava/lang/Integer> for process(Int?), found: {ffi_targets:?}"
        );

        // Verify Boolean vs Boolean? have distinct targets
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Z>"),
            "Expected <ffi:NativeLib.process__Z> for process(Boolean), found: {ffi_targets:?}"
        );
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/Boolean>"),
            "Expected <ffi:NativeLib.process__Ljava/lang/Boolean> for process(Boolean?), found: {ffi_targets:?}"
        );

        // Verify all targets are unique
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            ffi_targets.len(),
            unique_ffi_targets.len(),
            "Expected all FFI targets to be unique (no collision between nullable and non-nullable)"
        );
    }

    #[test]
    fn test_array_type_overloads_create_distinct_ffi_targets() {
        // Array<T> should generate array descriptors [Ljava/lang/String;
        // Primitive arrays (IntArray) should generate primitive array descriptors [I
        let source = r"
class NativeLib {
    external fun process(arr: Array<String>): String
    external fun process(arr: Array<Int>): String
    external fun process(nums: IntArray): String
    external fun process(values: DoubleArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for array overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Array<String> generates array descriptor
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/String>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/String> for Array<String>, found: {ffi_targets:?}"
        );

        // Verify Array<Int> generates array of boxed Int (since Array<T> holds objects)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Integer>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Integer> for Array<Int>, found: {ffi_targets:?}"
        );

        // Verify IntArray generates primitive array descriptor
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[I>"),
            "Expected <ffi:NativeLib.process__[I> for IntArray, found: {ffi_targets:?}"
        );

        // Verify DoubleArray generates primitive array descriptor
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[D>"),
            "Expected <ffi:NativeLib.process__[D> for DoubleArray, found: {ffi_targets:?}"
        );

        // Verify all targets are unique
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            ffi_targets.len(),
            unique_ffi_targets.len(),
            "Expected all array FFI targets to be unique"
        );
    }

    #[test]
    fn test_kotlin_stdlib_type_overloads_create_distinct_ffi_targets() {
        // Kotlin stdlib collection types should map to java.util
        let source = r"
class NativeLib {
    external fun process(items: List<String>): String
    external fun process(items: Set<String>): String
    external fun process(data: Map<String, Int>): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 3,
            "Expected 3 FfiCall edges for stdlib collection overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify List<String> maps to java.util.List
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/List>"),
            "Expected <ffi:NativeLib.process__Ljava/util/List> for List<String>, found: {ffi_targets:?}"
        );

        // Verify Set<String> maps to java.util.Set
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/Set>"),
            "Expected <ffi:NativeLib.process__Ljava/util/Set> for Set<String>, found: {ffi_targets:?}"
        );

        // Verify Map<String, Int> maps to java.util.Map (type erasure ignores generics)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/Map>"),
            "Expected <ffi:NativeLib.process__Ljava/util/Map> for Map<String, Int>, found: {ffi_targets:?}"
        );

        // Verify all targets are unique
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            ffi_targets.len(),
            unique_ffi_targets.len(),
            "Expected all stdlib collection FFI targets to be unique"
        );
    }

    #[test]
    fn test_unit_parameter_type_mapping() {
        // Unit as a parameter should map to Lkotlin/Unit; (not V which is for return types)
        let source = r"
class NativeLib {
    external fun process(callback: Unit): String
    external fun process(value: Int): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Unit parameter maps to Lkotlin/Unit; (not V)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Lkotlin/Unit>"),
            "Expected <ffi:NativeLib.process__Lkotlin/Unit> for Unit parameter, found: {ffi_targets:?}"
        );

        // Verify distinct from Int overload
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I>"),
            "Expected <ffi:NativeLib.process__I> for Int parameter, found: {ffi_targets:?}"
        );
    }

    #[test]
    fn test_fully_qualified_kotlin_types_create_correct_descriptors() {
        // Fully qualified Kotlin stdlib types should be normalized and mapped correctly
        let source = r"
class NativeLib {
    external fun process(x: kotlin.Int): String
    external fun process(s: kotlin.String): String
    external fun process(items: kotlin.collections.List<String>): String
    external fun process(arr: kotlin.Array<String>): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for fully qualified type overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify kotlin.Int → I (primitive descriptor)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I>"),
            "Expected <ffi:NativeLib.process__I> for kotlin.Int, found: {ffi_targets:?}"
        );

        // Verify kotlin.String → Ljava/lang/String (not Lkotlin/String)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/String>"),
            "Expected <ffi:NativeLib.process__Ljava/lang/String> for kotlin.String, found: {ffi_targets:?}"
        );

        // Verify kotlin.collections.List → Ljava/util/List (not Lkotlin/collections/List)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/List>"),
            "Expected <ffi:NativeLib.process__Ljava/util/List> for kotlin.collections.List, found: {ffi_targets:?}"
        );

        // Verify kotlin.Array<String> → [Ljava/lang/String (array descriptor)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/String>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/String> for kotlin.Array<String>, found: {ffi_targets:?}"
        );

        // Verify all targets are unique
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            ffi_targets.len(),
            unique_ffi_targets.len(),
            "Expected all FFI targets to be unique (fully qualified types normalized correctly)"
        );
    }

    #[test]
    fn test_array_type_projections_create_correct_descriptors() {
        // Array type projections (out, in, *) should be normalized to valid descriptors
        let source = r"
class NativeLib {
    external fun process(arr: Array<out String>): String
    external fun process(arr: Array<in Any>): String
    external fun process(arr: Array<*>): String
    external fun process(nums: IntArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for array projection overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Array<out String> → [Ljava/lang/String (out variance stripped)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/String>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/String> for Array<out String>, found: {ffi_targets:?}"
        );

        // Verify Array<in Any> → [Ljava/lang/Object (in variance stripped)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Object>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Object> for Array<in Any>, found: {ffi_targets:?}"
        );

        // Verify Array<*> → [Ljava/lang/Object (* maps to Any? → Object)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Object>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Object> for Array<*>, found: {ffi_targets:?}"
        );

        // Verify IntArray → [I (primitive array, unaffected by projections)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[I>"),
            "Expected <ffi:NativeLib.process__[I> for IntArray, found: {ffi_targets:?}"
        );

        // Note: Array<in Any> and Array<*> both map to [Ljava/lang/Object, so they collide
        // This is correct JVM behavior - at runtime they're the same type
        // So we expect 3 unique FFI targets, not 4
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            3,
            "Expected 3 unique FFI targets (Array<in Any> and Array<*> collide correctly)"
        );
    }

    #[test]
    fn test_qualified_primitive_array_types_create_correct_descriptors() {
        // Array<kotlin.Int>, Array<out Int>, Array<in Int> should normalize and box to Integer
        let source = r"
class NativeLib {
    external fun process(arr: Array<kotlin.Int>): String
    external fun process(arr: Array<out Int>): String
    external fun process(arr: Array<in Int>): String
    external fun process(nums: IntArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for qualified array overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Array<kotlin.Int> → [Ljava/lang/Integer (normalized and boxed)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Integer>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Integer> for Array<kotlin.Int>, found: {ffi_targets:?}"
        );

        // Verify Array<out Int> → [Ljava/lang/Integer (variance stripped, then boxed)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Integer>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Integer> for Array<out Int>, found: {ffi_targets:?}"
        );

        // Verify Array<in Int> → [Ljava/lang/Integer (variance stripped, then boxed)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Ljava/lang/Integer>"),
            "Expected <ffi:NativeLib.process__[Ljava/lang/Integer> for Array<in Int>, found: {ffi_targets:?}"
        );

        // Verify IntArray → [I (primitive array, distinct from Array<Int>)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[I>"),
            "Expected <ffi:NativeLib.process__[I> for IntArray, found: {ffi_targets:?}"
        );

        // Note: Array<kotlin.Int>, Array<out Int>, and Array<in Int> all normalize to [Ljava/lang/Integer
        // This is correct - they're semantically equivalent at runtime
        // So we expect 2 unique FFI targets, not 4
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            2,
            "Expected 2 unique FFI targets (qualified and projected Array<Int> variants collapse correctly)"
        );
    }

    #[test]
    fn test_mutable_collection_types_create_correct_descriptors() {
        // MutableCollection and MutableIterable should map to java.util.Collection and java.lang.Iterable
        let source = r"
class NativeLib {
    external fun process(items: kotlin.collections.MutableCollection<String>): String
    external fun process(items: kotlin.collections.MutableIterable<String>): String
    external fun process(items: Collection<String>): String
    external fun process(items: Iterable<String>): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 4,
            "Expected 4 FfiCall edges for mutable collection overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify MutableCollection → Ljava/util/Collection
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/Collection>"),
            "Expected <ffi:NativeLib.process__Ljava/util/Collection> for MutableCollection, found: {ffi_targets:?}"
        );

        // Verify MutableIterable → Ljava/lang/Iterable
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/Iterable>"),
            "Expected <ffi:NativeLib.process__Ljava/lang/Iterable> for MutableIterable, found: {ffi_targets:?}"
        );

        // Verify Collection → Ljava/util/Collection (same as MutableCollection)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/util/Collection>"),
            "Expected <ffi:NativeLib.process__Ljava/util/Collection> for Collection, found: {ffi_targets:?}"
        );

        // Verify Iterable → Ljava/lang/Iterable (same as MutableIterable)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__Ljava/lang/Iterable>"),
            "Expected <ffi:NativeLib.process__Ljava/lang/Iterable> for Iterable, found: {ffi_targets:?}"
        );

        // Note: MutableCollection and Collection both map to Ljava/util/Collection (collision is correct)
        // MutableIterable and Iterable both map to Ljava/lang/Iterable (collision is correct)
        // So we expect 2 unique FFI targets, not 4
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            2,
            "Expected 2 unique FFI targets (mutable and immutable variants collapse correctly)"
        );
    }

    #[test]
    fn test_unsigned_type_overloads_create_correct_descriptors() {
        // Unsigned types should map correctly: UInt→I, UInt?→Lkotlin/UInt;, etc.
        let source = r"
class NativeLib {
    external fun process(x: UInt): String
    external fun process(x: UInt?): String
    external fun process(x: ULong): String
    external fun process(x: ULong?): String
    external fun process(arr: Array<UInt>): String
    external fun process(arr: Array<ULong>): String
    external fun process(nums: UIntArray): String
    external fun process(nums: ULongArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let ffi_count = count_ffi_call_edges(&staging);
        assert_eq!(
            ffi_count, 8,
            "Expected 8 FfiCall edges for unsigned type overloads, found {ffi_count}"
        );

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify UInt → -UInt__I (non-nullable unsigned primitive with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UInt__I>"),
            "Expected <ffi:NativeLib.process-UInt__I> for UInt, found: {ffi_targets:?}"
        );

        // Verify UInt? → -UInt__Lkotlin/UInt; (nullable unsigned primitive with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UInt__Lkotlin/UInt>"),
            "Expected <ffi:NativeLib.process-UInt__Lkotlin/UInt> for UInt?, found: {ffi_targets:?}"
        );

        // Verify ULong → -ULong__J (non-nullable unsigned primitive with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-ULong__J>"),
            "Expected <ffi:NativeLib.process-ULong__J> for ULong, found: {ffi_targets:?}"
        );

        // Verify ULong? → -ULong__Lkotlin/ULong; (nullable unsigned primitive with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-ULong__Lkotlin/ULong>"),
            "Expected <ffi:NativeLib.process-ULong__Lkotlin/ULong> for ULong?, found: {ffi_targets:?}"
        );

        // Verify Array<UInt> → [Lkotlin/UInt; (boxed unsigned elements)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Lkotlin/UInt>"),
            "Expected <ffi:NativeLib.process__[Lkotlin/UInt> for Array<UInt>, found: {ffi_targets:?}"
        );

        // Verify Array<ULong> → [Lkotlin/ULong; (boxed unsigned elements)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Lkotlin/ULong>"),
            "Expected <ffi:NativeLib.process__[Lkotlin/ULong> for Array<ULong>, found: {ffi_targets:?}"
        );

        // Verify UIntArray → -UIntArray__[I (unsigned primitive array with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UIntArray__[I>"),
            "Expected <ffi:NativeLib.process-UIntArray__[I> for UIntArray, found: {ffi_targets:?}"
        );

        // Verify ULongArray → -ULongArray__[J (unsigned primitive array with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-ULongArray__[J>"),
            "Expected <ffi:NativeLib.process-ULongArray__[J> for ULongArray, found: {ffi_targets:?}"
        );

        // Note: All 8 overloads now have distinct FFI targets thanks to inline-class mangling
        // UInt gets -UInt mangling to distinguish it from Int (both erase to I)
        // ULong gets -ULong mangling to distinguish it from Long (both erase to J)
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            8,
            "Expected 8 unique FFI targets for unsigned type overloads (mangling prevents collisions)"
        );
    }

    #[test]
    fn test_signed_vs_unsigned_overloads_have_distinct_ffi_targets() {
        // Inline-class mangling ensures Int and UInt don't collide despite same JVM descriptor
        let source = r"
class NativeLib {
    external fun process(x: Int): String
    external fun process(x: UInt): String
    external fun process(x: Long): String
    external fun process(x: ULong): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify Int → __I (signed primitive, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__I>"),
            "Expected <ffi:NativeLib.process__I> for Int, found: {ffi_targets:?}"
        );

        // Verify UInt → -UInt__I (unsigned primitive, WITH mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UInt__I>"),
            "Expected <ffi:NativeLib.process-UInt__I> for UInt, found: {ffi_targets:?}"
        );

        // Verify Long → __J (signed primitive, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__J>"),
            "Expected <ffi:NativeLib.process__J> for Long, found: {ffi_targets:?}"
        );

        // Verify ULong → -ULong__J (unsigned primitive, WITH mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-ULong__J>"),
            "Expected <ffi:NativeLib.process-ULong__J> for ULong, found: {ffi_targets:?}"
        );

        // Critical: All 4 overloads must have distinct FFI targets
        // Before mangling: Int and UInt both -> __I (collision!)
        // After mangling: Int -> __I, UInt -> -UInt__I (distinct!)
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            4,
            "Expected 4 unique FFI targets (mangling prevents Int/UInt and Long/ULong collisions)"
        );
    }

    #[test]
    fn test_signed_vs_unsigned_array_overloads_have_distinct_ffi_targets() {
        // Inline-class mangling ensures IntArray and UIntArray don't collide despite same JVM descriptor
        let source = r"
class NativeLib {
    external fun process(nums: IntArray): String
    external fun process(nums: UIntArray): String
    external fun process(nums: LongArray): String
    external fun process(nums: ULongArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify IntArray → __[I (signed array, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[I>"),
            "Expected <ffi:NativeLib.process__[I> for IntArray, found: {ffi_targets:?}"
        );

        // Verify UIntArray → -UIntArray__[I (unsigned array, WITH mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UIntArray__[I>"),
            "Expected <ffi:NativeLib.process-UIntArray__[I> for UIntArray, found: {ffi_targets:?}"
        );

        // Verify LongArray → __[J (signed array, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[J>"),
            "Expected <ffi:NativeLib.process__[J> for LongArray, found: {ffi_targets:?}"
        );

        // Verify ULongArray → -ULongArray__[J (unsigned array, WITH mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-ULongArray__[J>"),
            "Expected <ffi:NativeLib.process-ULongArray__[J> for ULongArray, found: {ffi_targets:?}"
        );

        // Critical: All 4 overloads must have distinct FFI targets
        // Before mangling: IntArray and UIntArray both -> __[I (collision!)
        // After mangling: IntArray -> __[I, UIntArray -> -UIntArray__[I (distinct!)
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            4,
            "Expected 4 unique FFI targets (mangling prevents IntArray/UIntArray and LongArray/ULongArray collisions)"
        );
    }

    #[test]
    fn test_ubyte_and_ushort_type_variants_create_correct_descriptors() {
        // Complete test coverage for UByte and UShort (addressing Codex LOW finding)
        let source = r"
class NativeLib {
    external fun process(x: UByte): String
    external fun process(x: UByte?): String
    external fun process(x: UShort): String
    external fun process(x: UShort?): String
    external fun process(arr: Array<UByte>): String
    external fun process(arr: Array<UShort>): String
    external fun process(nums: UByteArray): String
    external fun process(nums: UShortArray): String
}
";
        let tree = parse_kotlin(source);
        let mut staging = StagingGraph::new();
        let builder = KotlinGraphBuilder::new();

        let result =
            builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);

        assert!(result.is_ok());

        let nodes: Vec<_> = staging.nodes().collect();
        let node_names: Vec<String> = nodes
            .iter()
            .filter_map(|n| staging.resolve_node_name(n.entry).map(String::from))
            .collect();

        let ffi_targets: Vec<&String> = node_names
            .iter()
            .filter(|name| name.starts_with("<ffi:"))
            .collect();

        // Verify UByte → -UByte__B (non-nullable unsigned byte with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UByte__B>"),
            "Expected <ffi:NativeLib.process-UByte__B> for UByte, found: {ffi_targets:?}"
        );

        // Verify UByte? → -UByte__Lkotlin/UByte; (nullable unsigned byte with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UByte__Lkotlin/UByte>"),
            "Expected <ffi:NativeLib.process-UByte__Lkotlin/UByte> for UByte?, found: {ffi_targets:?}"
        );

        // Verify UShort → -UShort__S (non-nullable unsigned short with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UShort__S>"),
            "Expected <ffi:NativeLib.process-UShort__S> for UShort, found: {ffi_targets:?}"
        );

        // Verify UShort? → -UShort__Lkotlin/UShort; (nullable unsigned short with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UShort__Lkotlin/UShort>"),
            "Expected <ffi:NativeLib.process-UShort__Lkotlin/UShort> for UShort?, found: {ffi_targets:?}"
        );

        // Verify Array<UByte> → [Lkotlin/UByte; (boxed unsigned byte elements, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Lkotlin/UByte>"),
            "Expected <ffi:NativeLib.process__[Lkotlin/UByte> for Array<UByte>, found: {ffi_targets:?}"
        );

        // Verify Array<UShort> → [Lkotlin/UShort; (boxed unsigned short elements, no mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process__[Lkotlin/UShort>"),
            "Expected <ffi:NativeLib.process__[Lkotlin/UShort> for Array<UShort>, found: {ffi_targets:?}"
        );

        // Verify UByteArray → -UByteArray__[B (unsigned primitive byte array with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UByteArray__[B>"),
            "Expected <ffi:NativeLib.process-UByteArray__[B> for UByteArray, found: {ffi_targets:?}"
        );

        // Verify UShortArray → -UShortArray__[S (unsigned primitive short array with mangling)
        assert!(
            node_names
                .iter()
                .any(|name| name == "<ffi:NativeLib.process-UShortArray__[S>"),
            "Expected <ffi:NativeLib.process-UShortArray__[S> for UShortArray, found: {ffi_targets:?}"
        );

        // All 8 overloads have distinct FFI targets
        let unique_ffi_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
        assert_eq!(
            unique_ffi_targets.len(),
            8,
            "Expected 8 unique FFI targets for UByte/UShort type overloads"
        );
    }

    /// Helper to count `FfiCall` edges in staging graph
    fn count_ffi_call_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    sqry_core::graph::unified::StagingOp::AddEdge {
                        kind: sqry_core::graph::unified::EdgeKind::FfiCall { .. },
                        ..
                    }
                )
            })
            .count()
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, compute_shape_descriptor,
    };
    use tree_sitter::Parser;

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/systems/sample.kt"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_sqry::language())
            .expect("load Kotlin grammar");
        parser.parse(src, None).expect("parse Kotlin sample")
    }

    fn first_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn kotlin_mapping_is_non_empty() {
        let mapping = kotlin_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_kotlin_sqry::language();
        let count = (0..lang.node_kind_count())
            .filter_map(|id| u16::try_from(id).ok())
            .filter(|id| mapping.cf_bucket(*id).is_some())
            .count();
        assert!(
            count > 0,
            "Kotlin cf_bucket map should cover real control-flow kinds"
        );
    }

    #[test]
    fn kotlin_histogram_covers_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_declaration")
            .expect("sample has a function_declaration");
        let desc = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            kotlin_shape_mapping(),
            &ShapeBudget::default(),
        );
        let h = &desc.cf_histogram;
        assert!(h[CfBucket::Branch.index()] >= 1, "branch present");
        assert!(h[CfBucket::Loop.index()] >= 1, "loop present");
        assert!(h[CfBucket::Match.index()] >= 1, "when present");
        assert!(h[CfBucket::Call.index()] >= 1, "call present");
        assert!(h[CfBucket::Try.index()] >= 1, "try present");
        assert!(h[CfBucket::Catch.index()] >= 1, "catch present");
        assert!(
            h[CfBucket::BreakContinue.index()] >= 1,
            "jump_expression (return/break/continue/throw) present"
        );
    }

    #[test]
    fn kotlin_signature_shape_reads_params() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_declaration")
            .expect("sample has a function_declaration");
        let shape = kotlin_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // classify(n: Int, label: String): Int -> two positional params + return type.
        assert_eq!(shape.arity_positional, 2, "two positional params");
        assert!(shape.has_return_annotation, "return type present");
    }
}
