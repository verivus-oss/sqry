// Nested conditionals kept for readability in Puppet AST traversal

//! Puppet `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts class dependency edges from Puppet manifests:
//! - `include myclass` → Class include relationship
//! - `require myclass` → Class require relationship (stronger than include)
//! - `class myclass inherits parent` → Class inheritance
//!
//! NOTE: `contain` statements are not supported by the tree-sitter-puppet grammar
//! (they are parsed as bare identifiers, not as a statement type).

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Span,
    unified::{GraphBuildHelper, NodeId as UnifiedNodeId, StagingGraph},
};
use tree_sitter::{Node, Tree};

/// `GraphBuilder` for Puppet manifests
#[derive(Debug, Default)]
pub struct PuppetGraphBuilder;

impl PuppetGraphBuilder {
    /// Create a new Puppet graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for PuppetGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Puppet);

        // Create module node for this Puppet file
        let module_id = helper.add_module("<module>", None);

        // First pass: collect class and defined type definitions and create export edges
        let mut class_ids = std::collections::HashMap::new();
        collect_class_definitions(
            tree.root_node(),
            content,
            module_id,
            &mut helper,
            &mut class_ids,
        )?;

        // Second pass: walk AST to find include/require/contain/inherits statements
        walk_ast_with_helper(
            tree.root_node(),
            content,
            module_id,
            &mut helper,
            &class_ids,
        )?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Puppet
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(puppet_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Puppet manifests.
///
/// Puppet is largely declarative (its `class_definition` / `defined_resource_type`
/// bodies map to `NodeKind::Class`, not Function/Method, so the build seam does not
/// currently attach descriptors to them). It still carries real control flow inside
/// those bodies (`if`/`elsif`/`unless` branches, `case`/selector matches, the
/// `.each` iterator, resource declarations as the unit of action, assignment, and
/// lambda blocks), so this maps that surface honestly onto the canonical
/// [`CfBucket`] schema. The table is built once and shared via
/// [`puppet_shape_mapping`].
pub struct PuppetShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl PuppetShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-puppet grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_puppet::LANGUAGE.into();
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
                *slot = cf_bucket_for_puppet_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for PuppetShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // A `class_definition` / `defined_resource_type` exposes its parameters as a
        // `parameter_list` child; each `parameter` may carry a default value
        // (`parameter` with more than the lone `variable` child).
        let mut cursor = fn_node.walk();
        for child in fn_node.named_children(&mut cursor) {
            if child.kind() == "parameter_list" {
                let mut pcursor = child.walk();
                for param in child.named_children(&mut pcursor) {
                    if param.kind() == "parameter" {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                        if param.named_child_count() > 1 {
                            shape.has_defaults = true;
                        }
                    }
                }
            }
        }
        shape
    }
}

/// Map one tree-sitter-puppet grammar node-kind name to its canonical control-flow
/// bucket. Additive-only; `_ => return None` for non-control-flow.
fn cf_bucket_for_puppet_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "elsif_statement" | "unless_statement" => CfBucket::Branch,
        "case_statement" | "selector" => CfBucket::Match,
        // The `$x.each |$i| { ... }` iteration construct.
        "iterator_statement" => CfBucket::Loop,
        // Function calls plus resource declarations / requires (the declarative unit
        // of action in a Puppet manifest).
        "function_call" | "resource_declaration" | "require_statement" => CfBucket::Call,
        "assignment" => CfBucket::Assign,
        "lambda" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Puppet shape mapping, built once on first use.
#[must_use]
pub fn puppet_shape_mapping() -> &'static PuppetShapeMapping {
    static MAPPING: OnceLock<PuppetShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(PuppetShapeMapping::build)
}

/// First pass: collect all class and defined type definitions, create nodes and export edges
fn collect_class_definitions(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
    class_ids: &mut HashMap<String, UnifiedNodeId>,
) -> GraphResult<()> {
    match node.kind() {
        "class_definition" | "defined_resource_type" => {
            // Extract class/defined type name
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if (child.kind() == "identifier" || child.kind() == "class_identifier")
                    && let Ok(name) = child.utf8_text(content)
                {
                    let class_name = name.to_string();
                    let span = Some(span_from_node(node));

                    // Create class node
                    let class_id = helper.add_class(&class_name, span);
                    // issue #394: real declaration; opt dual-use bare helper into is_definition
                    helper.mark_definition(class_id);
                    class_ids.insert(class_name.clone(), class_id);

                    // Export this class from the module
                    helper.add_export_edge(module_id, class_id);

                    // Extract TypeOf edges from typed parameters
                    extract_parameter_types(node, content, class_id, &class_name, helper);
                    break;
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_class_definitions(child, content, module_id, helper, class_ids)?;
    }

    Ok(())
}

/// Check if a tree-sitter node kind represents a Puppet type annotation
fn is_puppet_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "builtin_type"
            | "array_type"
            | "composite_type"
            | "attribute_type"
            | "class_identifier"
            | "identifier"
    )
}

/// Extract `TypeOf` edges from typed parameters in class/define definitions.
///
/// Puppet parameters can optionally have type annotations:
/// - `String $pkg = 'nginx'` → `TypeOf(myapp::pkg, String)`
/// - `Array[String] $pkgs` → `TypeOf(myapp::pkgs, Array[String])`
/// - `$untyped = 'x'` → no `TypeOf` edge (no type annotation)
///
/// Variable nodes are scope-qualified as `<class_name>::<param_name>` to prevent
/// collisions when different classes share the same parameter name.
fn extract_parameter_types(
    node: Node<'_>,
    content: &[u8],
    class_id: UnifiedNodeId,
    class_name: &str,
    helper: &mut GraphBuildHelper,
) {
    // Find the parameter_list child of the class/define node
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parameter_list" {
            let mut param_cursor = child.walk();
            for param in child.children(&mut param_cursor) {
                if param.kind() == "parameter" {
                    extract_single_param_type(param, content, class_id, class_name, helper);
                }
            }
        }
    }
}

/// Extract `TypeOf` edge from a single Puppet parameter node.
///
/// AST structure for typed parameter: `String $pkg = 'nginx'`
///   parameter
///     `builtin_type`  → "String"      (type annotation)
///     variable        → "$pkg"        (parameter name)
///     =
///     string          → "'nginx'"     (default value)
///
/// For untyped parameter: `$untyped = 'x'`
///   parameter
///     variable      → "$untyped"    (no type annotation)
///     =
///     string        → "'x'"
///
/// Variable nodes are scope-qualified as `<class_name>::<param_name>` to prevent
/// collisions when different classes share the same parameter name.
fn extract_single_param_type(
    param: Node<'_>,
    content: &[u8],
    class_id: UnifiedNodeId,
    class_name: &str,
    helper: &mut GraphBuildHelper,
) {
    let mut type_text = None;
    let mut var_name = None;

    let mut cursor = param.walk();
    for child in param.children(&mut cursor) {
        if type_text.is_none() && is_puppet_type_kind(child.kind()) && child.kind() != "identifier"
        {
            // Type annotation node (builtin_type, array_type, etc.)
            type_text = child.utf8_text(content).ok().map(ToString::to_string);
        } else if child.kind() == "identifier" && type_text.is_none() && var_name.is_none() {
            // An identifier before a variable could be a type name (e.g., custom class type)
            // But only if the next sibling is a variable node
            let maybe_type = child.utf8_text(content).ok().map(ToString::to_string);
            // Look ahead to see if a variable follows
            if let Some(next) = child.next_named_sibling()
                && next.kind() == "variable"
            {
                type_text = maybe_type;
            }
        } else if child.kind() == "variable" {
            // Extract variable name, stripping the $ prefix
            if let Ok(text) = child.utf8_text(content) {
                var_name = Some(text.trim_start_matches('$').to_string());
            }
        }
    }

    // Only create TypeOf edge if we have both type and variable name
    if let (Some(type_str), Some(name)) = (type_text, var_name) {
        // Scope-qualify: <class_name>::<param_name> (e.g., "myapp::pkg")
        let qualified_name = format!("{class_name}::{name}");
        let param_id = helper.add_variable(&qualified_name, Some(span_from_node(param)));
        // issue #394: real declaration; opt dual-use bare helper into is_definition
        helper.mark_definition(param_id);
        let type_id = helper.add_type(&type_str, None);
        helper.add_typeof_edge_with_context(
            param_id,
            type_id,
            Some(TypeOfContext::Parameter),
            None,
            Some(&qualified_name),
        );
        helper.add_reference_edge(param_id, type_id);
        helper.add_contains_edge(class_id, param_id);
    }
}

/// Walk AST to find class relationships using `GraphBuildHelper`
fn walk_ast_with_helper(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
    class_ids: &HashMap<String, UnifiedNodeId>,
) -> GraphResult<()> {
    // Check for relationship statements and call patterns
    // NOTE: contain_statement is NOT supported by tree-sitter-puppet grammar
    match node.kind() {
        "include_statement" => {
            extract_include_edge_with_helper(node, content, module_id, helper, "include");
        }
        "require_statement" => {
            extract_include_edge_with_helper(node, content, module_id, helper, "require");
        }
        "class_definition" => {
            // Check for inheritance
            extract_inheritance_edge_with_helper(node, content, helper, class_ids);
        }
        "resource_declaration" => {
            // Resource declarations like `file { '/path': }`, `package { 'nginx': }`
            extract_resource_call_with_helper(node, content, module_id, helper);
        }
        "function_call" => {
            // Function calls like `template('path')`, `lookup('key')`
            extract_function_call_with_helper(node, content, module_id, helper);
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ast_with_helper(child, content, module_id, helper, class_ids)?;
    }

    Ok(())
}

/// Extract edge from include/require/contain statement using `GraphBuildHelper`
///
/// The `relation_type` parameter distinguishes between:
/// - "include": brings class into catalog (ordering independent)
/// - "require": brings class into catalog with ordering dependency
/// - "contain": (future) for containment relationships if grammar support is added
///
/// Currently all relation types produce the same Import edge, but the parameter
/// is preserved for future semantic differentiation (e.g., edge metadata or
/// different edge kinds for ordering-sensitive relationships).
fn extract_include_edge_with_helper(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
    _relation_type: &str,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Look for the class identifier
        if (child.kind() == "identifier" || child.kind() == "class_identifier")
            && let Ok(class_name) = child.utf8_text(content)
        {
            // Convert class name to file path pattern
            let class_path = class_name.replace("::", "/");
            let qualified_name = format!("manifests/{class_path}.pp::{class_name}");

            // The extent is the `include` statement in THIS manifest; the class
            // itself is declared in another one (issue #748).
            let target_id =
                helper.add_call_site_node(&qualified_name, span_from_node(node), NodeKind::Class);

            // Add import edge
            helper.add_import_edge(module_id, target_id);

            return;
        }

        // Also check for string arguments (include 'classname')
        if child.kind() == "string"
            && let Ok(text) = child.utf8_text(content)
        {
            let class_name = text.trim_matches('\'').trim_matches('"');
            if !class_name.is_empty() {
                // Convert class name to file path pattern
                let class_path = class_name.replace("::", "/");
                let qualified_name = format!("manifests/{class_path}.pp::{class_name}");

                // The extent is the `include` statement in THIS manifest; the
                // class itself is declared in another one (issue #748).
                let target_id = helper.add_call_site_node(
                    &qualified_name,
                    span_from_node(node),
                    NodeKind::Class,
                );

                // Add import edge
                helper.add_import_edge(module_id, target_id);

                return;
            }
        }
    }
}

/// Extract inheritance edge from class definition using `GraphBuildHelper`
fn extract_inheritance_edge_with_helper(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    class_ids: &HashMap<String, UnifiedNodeId>,
) {
    let mut cursor = node.walk();
    let mut class_name = None;
    let mut parent_name = None;

    for child in node.children(&mut cursor) {
        match child.kind() {
            // The class name is a direct identifier child of class_definition
            "identifier" | "class_identifier" => {
                if class_name.is_none()
                    && let Ok(text) = child.utf8_text(content)
                {
                    class_name = Some(text.to_string());
                }
            }
            // class_inherits contains: inherits keyword + identifier for parent
            "class_inherits" => {
                let mut inherits_cursor = child.walk();
                for inherits_child in child.children(&mut inherits_cursor) {
                    if (inherits_child.kind() == "identifier"
                        || inherits_child.kind() == "class_identifier")
                        && let Ok(text) = inherits_child.utf8_text(content)
                    {
                        parent_name = Some(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // If we found inheritance, create edge
    if let (Some(class_name), Some(parent)) = (class_name, parent_name) {
        // Use existing class node from first pass
        let class_id = if let Some(&id) = class_ids.get(&class_name) {
            id
        } else {
            // Fallback: create class node if not found (shouldn't happen)
            helper.add_class(&class_name, Some(span_from_node(node)))
        };

        // Add parent class node
        let parent_path = parent.replace("::", "/");
        let parent_qualified = format!("manifests/{parent_path}.pp::{parent}");
        let parent_id = helper.add_class(&parent_qualified, None);

        // Add inheritance edge
        helper.add_inherits_edge(class_id, parent_id);
    }
}

/// Extract call edge from resource declaration using `GraphBuildHelper`
///
/// Resource declarations in Puppet are like function calls that instantiate resources:
/// - `file { '/tmp/test': ensure => present }` → Calls edge to `file` resource type
/// - `package { 'nginx': ensure => installed }` → Calls edge to `package` resource type
fn extract_resource_call_with_helper(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = node.walk();

    // First child is the resource type (identifier)
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(resource_type) = child.utf8_text(content)
        {
            // Create a function node for the resource type
            let callee_id =
                helper.add_function(&format!("resource::{resource_type}"), None, false, false);

            // Add call edge from module to resource type
            helper.add_call_edge_full_with_span(
                module_id,
                callee_id,
                255,
                false,
                vec![span_from_node(node)],
            );

            return;
        }
    }
}

/// Extract call edge from function call using `GraphBuildHelper`
///
/// Function calls in Puppet like:
/// - `template('mymodule/template.erb')` → Calls edge to `template`
/// - `lookup('mykey')` → Calls edge to `lookup`
/// - `hiera('config::option')` → Calls edge to `hiera`
fn extract_function_call_with_helper(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) {
    let mut cursor = node.walk();

    // First child is the function name (identifier)
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(function_name) = child.utf8_text(content)
        {
            // Create a function node for the function
            let callee_id = helper.add_function(function_name, None, false, false);

            // Add call edge from module to function
            helper.add_call_edge_full_with_span(
                module_id,
                callee_id,
                255,
                false,
                vec![span_from_node(node)],
            );

            return;
        }
    }
}

/// Create span from tree-sitter node
fn span_from_node(node: Node<'_>) -> Span {
    Span::from_node(&node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::test_helpers::*;
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::PathBuf;

    fn parse_puppet(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_puppet::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    #[test]
    fn test_extracts_include() {
        let source = r"
class myclass {
  include other_class
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(import_edges.len(), 1, "Should extract one include edge");

        // Verify the class definition was created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);

        // Verify the included class node was created
        assert_has_node(&staging, "other_class");
    }

    #[test]
    fn test_extracts_require() {
        let source = r"
class myclass {
  require base_class
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(import_edges.len(), 1, "Should extract one require edge");

        // Verify the class definition was created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);

        // Verify the required class node was created
        assert_has_node(&staging, "base_class");
    }

    #[test]
    fn test_extracts_string_class_name() {
        let source = r"
include 'mymodule::myclass'
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/site.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Should extract include edge with string"
        );

        // Verify the included class node was created
        assert_has_node(&staging, "mymodule::myclass");
    }

    #[test]
    fn test_multiple_includes() {
        let source = r"
class myclass {
  include base
  include networking
  require security
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(import_edges.len(), 3, "Should extract 3 import edges");

        // Verify the class definition was created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);

        // Verify all included/required classes were created
        assert_has_node(&staging, "base");
        assert_has_node(&staging, "networking");
        assert_has_node(&staging, "security");
    }

    #[test]
    fn test_qualified_class_name() {
        let source = r"
include mymodule::submodule::myclass
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/site.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(import_edges.len(), 1, "Should extract one import edge");

        // Verify the qualified class name was captured
        assert_has_node(&staging, "mymodule");
    }

    #[test]
    fn test_contain_not_supported_by_grammar() {
        // NOTE: tree-sitter-puppet grammar does not have contain_statement node type.
        // `contain contained_class` is parsed as bare identifiers, not as a statement.
        // This test documents the grammar limitation.
        let source = r"
class myclass {
  contain contained_class
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        // Grammar doesn't recognize contain as a statement type, so no edges should be created
        // Only the class definition and module should exist
        assert_eq!(import_edges.len(), 0, "Contain not supported by grammar");

        // Verify the class definition was still created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);
    }

    #[test]
    fn test_extracts_class_inheritance() {
        let source = r"
class myclass inherits parent_class {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let inherits_edges = collect_inherits_edges(&staging);
        assert_eq!(inherits_edges.len(), 1, "Should extract one inherits edge");

        // Verify both class nodes were created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);
        assert_has_node(&staging, "parent_class");

        // Verify the inheritance edge exists (parent is qualified)
        assert_has_inherits_edge(
            &staging,
            "myclass",
            "manifests/parent_class.pp::parent_class",
        );
    }

    #[test]
    fn test_class_without_inheritance() {
        let source = r"
class myclass {
  # class body without inheritance
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let inherits_edges = collect_inherits_edges(&staging);
        assert_eq!(inherits_edges.len(), 0, "Should have no inherits edges");

        // Verify the class definition was created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);
    }

    #[test]
    fn test_empty_file() {
        let source = "";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/empty.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should only have the module node for the file itself
        assert_eq!(
            count_nodes_by_kind(&staging, NodeKind::Module),
            1,
            "Empty file should have exactly one module node"
        );

        // No other nodes should exist
        assert_eq!(
            count_nodes_by_kind(&staging, NodeKind::Class),
            0,
            "Empty file should have no class nodes"
        );
    }

    #[test]
    fn test_mixed_statements() {
        // Note: contain is not supported by the grammar, so we only test include/require
        let source = r"
class myclass inherits base_class {
  include helper
  require dependency
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myclass.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have inheritance edge
        let inherits_edges = collect_inherits_edges(&staging);
        assert_eq!(inherits_edges.len(), 1, "Should have one inherits edge");

        // Should have import edges for include and require
        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            2,
            "Should have two import edges (include + require)"
        );

        // Verify all nodes were created
        assert_has_node_with_kind(&staging, "myclass", NodeKind::Class);
        assert_has_node(&staging, "base_class");
        assert_has_node(&staging, "helper");
        assert_has_node(&staging, "dependency");

        // Verify the inheritance edge (parent is qualified)
        assert_has_inherits_edge(&staging, "myclass", "manifests/base_class.pp::base_class");
    }

    #[test]
    fn test_nested_class_include() {
        let source = r"
class outer {
  class { 'inner':
    # nested resource-style class
  }
  include nested_include
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/outer.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert!(
            !import_edges.is_empty(),
            "Should extract include edge from nested class"
        );

        // Verify the outer class and included class nodes
        assert_has_node_with_kind(&staging, "outer", NodeKind::Class);
        assert_has_node(&staging, "nested_include");
    }
}

// Active tests for Unified Graph (Wave 8)
#[cfg(test)]
mod active_tests {
    use super::*;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use std::path::PathBuf;

    fn parse_puppet(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_puppet::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

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

    /// Helper to extract Call edges from staging operations
    fn extract_call_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Calls { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    /// Helper to extract Inherits edges from staging operations
    fn extract_inherits_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Inherits)
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    #[test]
    fn test_puppet_graph_builder_language() {
        let builder = PuppetGraphBuilder;
        assert_eq!(builder.language(), Language::Puppet);
    }

    #[test]
    fn test_extracts_resource_declaration_calls() {
        let source = r"
class myapp {
  package { 'nginx':
    ensure => installed,
  }

  service { 'nginx':
    ensure => running,
  }

  file { '/etc/nginx/nginx.conf':
    ensure => present,
  }
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            3,
            "Should extract three call edges for resource declarations (package, service, file)"
        );
    }

    #[test]
    fn test_extracts_function_calls() {
        let source = r#"
define myapp::config($port = 80) {
  file { "/etc/myapp/${name}.conf":
    content => template('myapp/config.erb'),
  }
}
"#;

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp/config.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let call_edges = extract_call_edges(&staging);
        // Should have at least: file resource + template function
        assert!(
            call_edges.len() >= 2,
            "Should extract call edges for file resource and template() function"
        );
    }

    #[test]
    fn test_extracts_include_import_edge() {
        let source = r"
class webserver {
  include myapp
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/webserver.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for include statement"
        );
    }

    #[test]
    fn test_extracts_require_import_edge() {
        let source = r"
class webserver {
  require myapp::prereqs
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/webserver.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for require statement"
        );
    }

    #[test]
    fn test_extracts_inheritance_edge() {
        let source = r"
class child inherits parent {
  # child class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/child.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let inherits_edges = extract_inherits_edges(&staging);
        assert_eq!(
            inherits_edges.len(),
            1,
            "Should extract one inherits edge for class inheritance"
        );
    }

    #[test]
    fn test_empty_file_no_edges() {
        let source = "";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/empty.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let call_edges = extract_call_edges(&staging);
        assert!(
            call_edges.is_empty(),
            "Empty file should have no call edges"
        );

        let import_edges = extract_import_edges(&staging);
        assert!(
            import_edges.is_empty(),
            "Empty file should have no import edges"
        );
    }

    #[test]
    fn test_mixed_statements() {
        let source = r"
class myapp inherits base {
  include helper
  require prereqs

  package { 'app':
    ensure => installed,
  }

  $config = lookup('app::config')
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        // Should have import edges for include and require
        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            2,
            "Should extract two import edges (include + require)"
        );

        // Should have inherits edge
        let inherits_edges = extract_inherits_edges(&staging);
        assert_eq!(inherits_edges.len(), 1, "Should have one inherits edge");

        // Should have call edges for package resource and lookup function
        let call_edges = extract_call_edges(&staging);
        assert!(
            call_edges.len() >= 2,
            "Should have at least 2 call edges (package resource + lookup function)"
        );
    }

    #[test]
    fn test_node_definition_with_resources() {
        let source = r"
node 'web01.example.com' {
  package { 'nginx':
    ensure => installed,
  }
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/nodes.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Should extract one call edge for package resource"
        );
    }

    /// Helper to extract `TypeOf` edges from staging operations
    fn extract_typeof_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::TypeOf { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    #[test]
    fn test_class_typed_params_create_typeof_edges() {
        let source = r"
class myapp (
  String $pkg,
  Integer $port = 80,
) {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Class with 2 typed parameters should create 2 TypeOf edges"
        );
    }

    #[test]
    fn test_define_typed_params_create_typeof_edges() {
        let source = r"
define myapp::config (
  String $name,
  Boolean $ssl = false,
) {
  # define body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp/config.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Define with 2 typed parameters should create 2 TypeOf edges"
        );
    }

    #[test]
    fn test_class_untyped_params_no_typeof_edges() {
        let source = r"
class myapp (
  $pkg = 'nginx',
  $port = 80,
) {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert!(
            typeof_edges.is_empty(),
            "Class with only untyped parameters should have no TypeOf edges"
        );
    }

    #[test]
    fn test_complex_puppet_types_create_typeof_edges() {
        let source = r"
class myapp (
  Array[String] $pkgs,
  Hash $config = {},
) {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Class with 2 complex typed parameters should create 2 TypeOf edges"
        );
    }

    #[test]
    fn test_mixed_typed_untyped_params() {
        let source = r"
class myapp (
  String $name,
  $untyped_param,
  Integer $port = 80,
) {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Class with 2 typed + 1 untyped should create 2 TypeOf edges"
        );
    }

    #[test]
    fn test_class_without_params_no_typeof_edges() {
        let source = r"
class myapp {
  package { 'nginx':
    ensure => installed,
  }
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert!(
            typeof_edges.is_empty(),
            "Class without parameters should have no TypeOf edges"
        );
    }

    #[test]
    #[allow(clippy::items_after_statements)] // Items near usage for clarity
    fn test_same_param_name_different_classes_distinct_nodes() {
        // Two classes with same parameter name should produce distinct Variable nodes
        // thanks to scope-qualified naming: foo::name vs bar::name
        let source = r"
class foo (
  String $name = 'a',
) { }

class bar (
  Integer $name = 1,
) { }
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/classes.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Two classes with typed params should create 2 TypeOf edges"
        );

        // Verify distinct scope-qualified variable nodes
        use sqry_core::graph::unified::build::test_helpers::assert_has_node_with_kind;
        use sqry_core::graph::unified::node::NodeKind;
        assert_has_node_with_kind(&staging, "foo::name", NodeKind::Variable);
        assert_has_node_with_kind(&staging, "bar::name", NodeKind::Variable);

        // Verify distinct type nodes
        assert_has_node_with_kind(&staging, "String", NodeKind::Type);
        assert_has_node_with_kind(&staging, "Integer", NodeKind::Type);
    }

    #[test]
    #[allow(clippy::items_after_statements)] // Items near usage for clarity
    fn test_custom_namespaced_type_extracts_correctly() {
        // Validates that class_identifier types (e.g., Stdlib::Absolutepath)
        // are correctly extracted as type text
        let source = r"
class myapp (
  String $name,
  Stdlib::Absolutepath $config_dir = '/etc/myapp',
) {
  # class body
}
";

        let tree = parse_puppet(source);
        let mut staging = StagingGraph::new();
        let builder = PuppetGraphBuilder;
        let file = PathBuf::from("manifests/myapp.pp");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert_eq!(
            typeof_edges.len(),
            2,
            "Should have 2 TypeOf edges (String + Stdlib::Absolutepath)"
        );

        // Verify the namespaced type node was created with correct text
        use sqry_core::graph::unified::build::test_helpers::assert_has_node_with_kind;
        use sqry_core::graph::unified::node::NodeKind;
        assert_has_node_with_kind(&staging, "Stdlib::Absolutepath", NodeKind::Type);
        assert_has_node_with_kind(&staging, "String", NodeKind::Type);
        assert_has_node_with_kind(&staging, "myapp::config_dir", NodeKind::Variable);
    }
}

#[cfg(test)]
mod shape_tests {
    use super::{PuppetGraphBuilder, puppet_shape_mapping};
    use sqry_core::graph::GraphBuilder;
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/iac/manifest.pp"
    ));

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_puppet::LANGUAGE.into())
            .expect("load puppet grammar");
        parser.parse(src, None).expect("parse puppet")
    }

    fn first_of_kind<'t>(node: tree_sitter::Node<'t>, kind: &str) -> Option<tree_sitter::Node<'t>> {
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
    fn builder_advertises_shape_mapping() {
        assert!(
            PuppetGraphBuilder.shape_mapping().is_some(),
            "Puppet builder must advertise a ShapeMapping"
        );
    }

    #[test]
    fn cf_map_covers_real_control_flow_in_define_body() {
        let mapping = puppet_shape_mapping();
        let tree = parse(SAMPLE);
        let src = SAMPLE.as_bytes();
        // The `defined_resource_type` carries the manifest body's control flow.
        let define = first_of_kind(tree.root_node(), "defined_resource_type")
            .expect("defined_resource_type present");
        let d = compute_shape_descriptor(define, src, mapping, &ShapeBudget::default());

        assert!(!d.is_unhashable(), "the define body must be hashable");
        assert!(
            d.cf_histogram[CfBucket::Branch.index()] >= 1,
            "if/elsif/unless branch"
        );
        assert!(d.cf_histogram[CfBucket::Match.index()] >= 1, "case match");
        assert!(
            d.cf_histogram[CfBucket::Loop.index()] >= 1,
            "the .each iterator"
        );
        assert!(
            d.cf_histogram[CfBucket::Call.index()] >= 1,
            "resource declarations / function calls"
        );
        assert!(d.cf_histogram[CfBucket::Assign.index()] >= 1, "assignment");
    }

    #[test]
    fn signature_shape_reads_parameter_list() {
        let mapping = puppet_shape_mapping();
        let tree = parse(SAMPLE);
        let src = SAMPLE.as_bytes();
        let define = first_of_kind(tree.root_node(), "defined_resource_type")
            .expect("defined_resource_type present");
        let d = compute_shape_descriptor(define, src, mapping, &ShapeBudget::default());
        assert_eq!(
            d.signature_shape.arity_positional, 2,
            "myapp::service has two parameters"
        );
        assert!(
            d.signature_shape.has_defaults,
            "replicas carries a default value"
        );
    }
}
// Collapsible nested conditionals kept for readability with early-exit patterns
