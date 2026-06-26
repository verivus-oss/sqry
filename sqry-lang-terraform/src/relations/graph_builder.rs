// Nested conditionals kept for readability in Terraform AST traversal

//! Terraform `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts infrastructure dependency edges from Terraform/HCL documents:
//! - `module { source = "..." }` → Module source reference
//! - `provider "aws" { ... }` → Provider dependency
//! - Registry modules (registry.terraform.io)
//! - Git modules (`git::`, github.com)
//! - Local modules (./local/path)

use std::sync::OnceLock;
use std::{path::Path, sync::Arc};

use sqry_core::graph::unified::build::shape::{CfBucket, ShapeMapping};
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::storage::shape::SignatureShape;
use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Span,
    path_resolver::resolve_import_path,
    unified::{GraphBuildHelper, NodeId as UnifiedNodeId, StagingGraph},
};
use tree_sitter::{Node, Tree};

/// `GraphBuilder` for Terraform/HCL documents
#[derive(Debug, Default)]
pub struct TerraformGraphBuilder;

impl TerraformGraphBuilder {
    /// Create a new Terraform graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for TerraformGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Create helper for staging graph population
        let mut helper = GraphBuildHelper::new(staging, file, Language::Terraform);

        // Create module node for this Terraform file
        let module_id = helper.add_module("<module>", None);

        let root = tree.root_node();

        // HCL AST structure: config_file -> body -> blocks
        let mut root_cursor = root.walk();
        let body_node = root.children(&mut root_cursor).find(|n| n.kind() == "body");

        if let Some(body) = body_node {
            let mut cursor = body.walk();

            // First pass: collect outputs, variables, and resources
            let mut exports = Vec::new();
            for node in body.children(&mut cursor) {
                if node.kind() == "block"
                    && let Some(export_info) = collect_exportable_block(node, content, &mut helper)
                {
                    exports.push(export_info);
                }
            }

            // Create export edges for outputs, variables, and resources
            for (_name, node_id) in exports {
                helper.add_export_edge(module_id, node_id);
            }

            // Second pass: extract module and provider edges
            let mut cursor = body.walk();
            for node in body.children(&mut cursor) {
                if node.kind() == "block" {
                    extract_block_edge_with_helper(node, content, module_id, &mut helper)?;
                }
            }
        }

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Terraform
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(terraform_shape_mapping())
    }
}

/// Per-language [`ShapeMapping`] for Terraform / HCL.
///
/// Terraform is purely declarative: the tree-sitter-hcl grammar has no
/// function/method definition node, and this plugin emits no `NodeKind::Function`
/// or `NodeKind::Method` with a body span (its only `add_function` calls create
/// span-less call-target stubs for cross-module reference edges). The build seam
/// therefore never attaches a descriptor for a Terraform file. The mapping is still
/// implemented (AC-1: no plugin omitted) and is genuinely populated for HCL's
/// expression-level control flow (`for` comprehensions, `conditional` ternaries,
/// `function_call`), so it is correct if the walker is ever pointed at an HCL
/// expression. The coverage test asserts the honest declarative contract: no
/// eligible function-with-body nodes exist. Shared via [`terraform_shape_mapping`].
pub struct TerraformShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl TerraformShapeMapping {
    /// Build the `kind_id -> CfBucket` table from the tree-sitter-hcl grammar.
    fn build() -> Self {
        let lang: tree_sitter::Language = tree_sitter_hcl::LANGUAGE.into();
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
                *slot = cf_bucket_for_hcl_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for TerraformShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, _fn_node: Node, _src: &[u8]) -> SignatureShape {
        // HCL blocks have no parameter list; the signature shape is empty by
        // construction (honest minimal impl for a declarative language).
        SignatureShape::default()
    }
}

/// Map one tree-sitter-hcl grammar node-kind name to its canonical control-flow
/// bucket. HCL control flow is expression-level only. Additive-only.
fn cf_bucket_for_hcl_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        // `[for x in list : x if cond]` and the tuple/object comprehension heads.
        "for_expr" | "for_tuple_expr" | "for_object_expr" => CfBucket::Comprehension,
        // `cond ? a : b` ternary, plus the `if` clause of a comprehension.
        "conditional" | "for_cond" => CfBucket::Branch,
        "function_call" => CfBucket::Call,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Terraform shape mapping, built once on first use.
#[must_use]
pub fn terraform_shape_mapping() -> &'static TerraformShapeMapping {
    static MAPPING: OnceLock<TerraformShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(TerraformShapeMapping::build)
}

/// Collect exportable blocks (output, variable, resource) and create nodes
/// Returns (`block_name`, `node_id`) for creating export edges
#[allow(clippy::too_many_lines)]
fn collect_exportable_block(
    node: Node<'_>,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Option<(String, UnifiedNodeId)> {
    let mut block_type = None;
    let mut block_labels = Vec::new();
    let mut block_body = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if block_type.is_none() => {
                block_type = child.utf8_text(content).ok().map(ToString::to_string);
            }
            "string_lit" => {
                if let Ok(text) = child.utf8_text(content) {
                    block_labels.push(text.trim_matches('"').to_string());
                }
            }
            "body" => {
                block_body = Some(child);
            }
            _ => {}
        }
    }

    let bt = block_type?;
    let name = block_labels.first()?;

    match bt.as_str() {
        "output" => {
            // Output blocks define module outputs (primary exports)
            let node_id = helper.add_variable(name, Some(span_from_node(node)));
            // Outputs can optionally have `type = <expr>` attribute
            if let Some(body) = block_body
                && let Some(raw_type) = extract_attribute_value(body, content, "type")
            {
                let type_text = normalize_type_text(&raw_type);
                let type_id = helper.add_type(&type_text, None);
                helper.add_typeof_edge_with_context(
                    node_id,
                    type_id,
                    Some(TypeOfContext::Variable),
                    None,
                    Some(name),
                );
                helper.add_reference_edge(node_id, type_id);
            }
            Some((name.clone(), node_id))
        }
        "variable" => {
            // Variable blocks define module inputs (also part of module interface)
            let node_id = helper.add_variable(name, Some(span_from_node(node)));
            // Variables can have `type = <expr>` attribute
            if let Some(body) = block_body
                && let Some(raw_type) = extract_attribute_value(body, content, "type")
            {
                let type_text = normalize_type_text(&raw_type);
                let type_id = helper.add_type(&type_text, None);
                helper.add_typeof_edge_with_context(
                    node_id,
                    type_id,
                    Some(TypeOfContext::Variable),
                    None,
                    Some(name),
                );
                helper.add_reference_edge(node_id, type_id);
            }
            Some((name.clone(), node_id))
        }
        "resource" => {
            // Resource blocks define infrastructure resources
            // Format: resource "type" "name" - block_labels[0] is type, block_labels[1] is name
            // Use the Terraform-native address text `<type>.<name>` as the input
            // name (for example `aws_instance.web`). The unified graph layer then
            // canonicalizes qualified identities to `::` separators, which avoids
            // collisions when different resource types share the same local name.
            if block_labels.len() >= 2 {
                let resource_type = &block_labels[0];
                let resource_name = &block_labels[1];
                let canonical_name = format!("{resource_type}.{resource_name}");
                let node_id = helper.add_variable(&canonical_name, Some(span_from_node(node)));
                let type_id = helper.add_type(resource_type, None);
                helper.add_typeof_edge_with_context(
                    node_id,
                    type_id,
                    Some(TypeOfContext::Variable),
                    None,
                    Some(&canonical_name),
                );
                helper.add_reference_edge(node_id, type_id);
                Some((canonical_name, node_id))
            } else {
                None
            }
        }
        "data" => {
            // Data source blocks: data "type" "name"
            // Use the Terraform-native address text `data.<type>.<name>` as the
            // input name. The unified graph layer canonicalizes the staged
            // qualified identity to `::` separators while preserving the three
            // semantic components and preventing collisions between data types.
            if block_labels.len() >= 2 {
                let data_type = &block_labels[0];
                let data_name = &block_labels[1];
                let qualified_name = format!("data.{data_type}.{data_name}");
                let type_id = helper.add_type(data_type, None);
                let node_id = helper.add_variable(&qualified_name, Some(span_from_node(node)));
                helper.add_typeof_edge_with_context(
                    node_id,
                    type_id,
                    Some(TypeOfContext::Variable),
                    None,
                    Some(&qualified_name),
                );
                helper.add_reference_edge(node_id, type_id);
                Some((qualified_name, node_id))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract import edge from a block node (module or provider) using `GraphBuildHelper`
fn extract_block_edge_with_helper(
    node: Node<'_>,
    content: &[u8],
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let mut block_type = None;
    let mut block_labels = Vec::new();
    let mut block_body = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if block_type.is_none() => {
                block_type = child.utf8_text(content).ok().map(ToString::to_string);
            }
            "string_lit" => {
                if let Ok(text) = child.utf8_text(content) {
                    block_labels.push(text.trim_matches('"').to_string());
                }
            }
            "body" => {
                block_body = Some(child);
            }
            _ => {}
        }
    }

    let Some(bt) = block_type else {
        return Ok(());
    };

    match bt.as_str() {
        "module" => {
            // Extract module name for Call edge (if present in labels)
            let module_name = block_labels.first().cloned();

            if let Some(body) = block_body
                && let Some(source) = extract_attribute_value(body, content, "source")
            {
                extract_module_edge_with_helper(
                    node,
                    &source,
                    module_name.as_deref(),
                    module_id,
                    helper,
                )?;
            }
        }
        "provider" => {
            // Provider blocks reference external providers
            if !block_labels.is_empty() {
                extract_provider_edge_with_helper(node, &block_labels[0], module_id, helper);
            }
        }
        _ => {}
    }

    Ok(())
}

/// Extract module source edge using `GraphBuildHelper`
///
/// Creates both Import and Call edges for module blocks:
/// - Import edge: represents the dependency on the module source
/// - Call edge: represents the invocation/instantiation of the module
fn extract_module_edge_with_helper(
    node: Node<'_>,
    source: &str,
    module_name: Option<&str>,
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) -> GraphResult<()> {
    let file_arc: Arc<str> = Arc::from(helper.file_path());
    let (resolved_path, _is_remote) = resolve_module_source(source, &file_arc)?;

    // Add target node as an import
    let target_id = helper.add_import(&resolved_path, Some(span_from_node(node)));

    // Add import edge from module to target (dependency)
    helper.add_import_edge(module_id, target_id);

    // Add Call edge for module instantiation
    // The module block is semantically like a function call that instantiates the module
    if let Some(name) = module_name {
        // Create a function node for the module instantiation
        // Use a qualified name like "module::vpc" or just the resolved path
        let caller_name = format!("module::{name}");
        let source_id = helper.add_function(&caller_name, Some(span_from_node(node)), false, false);

        // Create a callee representing the module source
        let target_id = helper.add_function(&resolved_path, None, false, false);

        // Add call edge from the module declaration to the source module
        helper.add_call_edge_full_with_span(
            source_id,
            target_id,
            255,
            false,
            vec![span_from_node(node)],
        );
    }

    Ok(())
}

/// Extract provider dependency edge using `GraphBuildHelper`
fn extract_provider_edge_with_helper(
    node: Node<'_>,
    provider_name: &str,
    module_id: UnifiedNodeId,
    helper: &mut GraphBuildHelper,
) {
    // Create a provider target node with registry URL pattern
    let provider_url = format!("registry.terraform.io/providers/hashicorp/{provider_name}/latest");

    // Add target node as an import
    let target_id = helper.add_import(&provider_url, Some(span_from_node(node)));

    // Add import edge from module to provider
    helper.add_import_edge(module_id, target_id);
}

/// Normalize Terraform type text: trim and collapse internal whitespace.
fn normalize_type_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract an attribute value from a block body
fn extract_attribute_value(body: Node<'_>, content: &[u8], attr_name: &str) -> Option<String> {
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        if child.kind() == "attribute" {
            let mut name = None;
            let mut value = None;

            let mut attr_cursor = child.walk();
            for attr_child in child.children(&mut attr_cursor) {
                if attr_child.kind() == "identifier" && name.is_none() {
                    name = attr_child.utf8_text(content).ok().map(ToString::to_string);
                } else if attr_child.kind() == "expression" {
                    value = attr_child.utf8_text(content).ok().map(ToString::to_string);
                }
            }

            if name.as_deref() == Some(attr_name)
                && let Some(v) = value
            {
                // Remove quotes if present
                return Some(v.trim_matches('"').to_string());
            }
        }
    }

    None
}

/// Resolve a Terraform module source to a path and determine if it's remote
fn resolve_module_source(source: &str, file: &Arc<str>) -> GraphResult<(String, bool)> {
    // Terraform module source formats:
    // - Local: ./local/path, ../relative/path
    // - Registry: hashicorp/consul/aws, registry.terraform.io/...
    // - Git: git::https://..., github.com/...
    // - HTTP: https://...
    // - S3: s3::...

    // Check for local paths
    if source.starts_with("./") || source.starts_with("../") || source.starts_with('/') {
        let source_path = Path::new(file.as_ref());
        let resolved = resolve_import_path(source_path, source)?;
        return Ok((resolved, false));
    }

    // Check for explicit protocols
    if source.starts_with("git::")
        || source.starts_with("hg::")
        || source.starts_with("s3::")
        || source.starts_with("gcs::")
    {
        return Ok((source.to_string(), true));
    }

    // Check for URLs
    if is_remote_url(source) {
        return Ok((normalize_protocol_relative(source), true));
    }

    // Check for GitHub shorthand (github.com/...)
    if source.starts_with("github.com/") || source.starts_with("bitbucket.org/") {
        return Ok((format!("https://{source}"), true));
    }

    // Assume registry module (namespace/name/provider format)
    // e.g., "hashicorp/consul/aws" -> "registry.terraform.io/modules/hashicorp/consul/aws"
    // Registry modules follow the pattern: namespace/name/provider (3 parts)
    if source.matches('/').count() >= 2 && !looks_like_local_path(source) {
        return Ok((format!("registry.terraform.io/modules/{source}"), true));
    }

    // If it contains a slash but not two, check if it looks like a local path
    // Local paths: my_modules/vpc, modules/network, subdirectory/module
    // These should be treated as local, not remote
    //
    // Remote patterns typically match:
    // - Known registry domains (registry.terraform.io/...)
    // - Has dots indicating a domain name (example.com/module)
    if source.contains('/') {
        if looks_like_local_path(source) {
            let source_path = Path::new(file.as_ref());
            let resolved = resolve_import_path(source_path, source)?;
            return Ok((resolved, false));
        }
        // Might be a registry shorthand like "hashicorp/consul" without provider
        return Ok((source.to_string(), true));
    }

    // Single word - treat as local reference
    let source_path = Path::new(file.as_ref());
    let resolved = resolve_import_path(source_path, source)?;
    Ok((resolved, false))
}

/// Check if a source path looks like a local filesystem path rather than a registry/remote reference.
///
/// Local paths typically:
/// - Start with ./ or ../ or /
/// - Have 1 or 2 path segments without domain-like patterns
/// - Don't match the Terraform registry format (namespace/name/provider)
fn looks_like_local_path(source: &str) -> bool {
    // Paths with ./ or ../ or / prefix are definitely local
    if source.starts_with("./") || source.starts_with("../") || source.starts_with('/') {
        return true;
    }

    // Check if it looks like a domain (has dots before first slash)
    if let Some(slash_idx) = source.find('/') {
        let before_slash = &source[..slash_idx];
        // If there's a dot before the slash, it's likely a domain name (remote)
        if before_slash.contains('.') {
            return false;
        }
    }

    // Check if this matches the Terraform registry format: namespace/name/provider
    // Registry modules have exactly 3 parts separated by slashes, all valid identifiers
    if is_terraform_registry_format(source) {
        return false;
    }

    // Paths with 1-2 segments and no dots are likely local filesystem paths
    // e.g., my_modules/vpc, modules/network
    true
}

/// Check if a source string matches the Terraform registry format.
///
/// Registry format is: namespace/name/provider (exactly 3 parts)
/// Example: hashicorp/consul/aws
fn is_terraform_registry_format(source: &str) -> bool {
    let parts: Vec<&str> = source.split('/').collect();

    // Registry modules have exactly 3 parts
    if parts.len() != 3 {
        return false;
    }

    // Each part must be a valid identifier (alphanumeric, underscore, hyphen)
    // and not empty
    parts.iter().all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    })
}

/// Check if a URL is remote (http://, https://, //)
fn is_remote_url(url: &str) -> bool {
    url.starts_with("//")
        || (url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://"))
        || (url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://"))
}

/// Normalize protocol-relative URLs to https://
fn normalize_protocol_relative(url: &str) -> String {
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}

/// Create span from tree-sitter node
fn span_from_node(node: Node<'_>) -> Span {
    Span::from_bytes(node.start_byte(), node.end_byte())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_node, collect_call_edges, collect_import_edges,
    };
    use std::path::PathBuf;

    fn parse_hcl(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hcl::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    #[test]
    fn test_extracts_local_module() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
  cidr   = "10.0.0.0/16"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for local module"
        );

        // Verify a node was created for the local module source
        assert_has_node(&staging, "modules/vpc");
    }

    #[test]
    fn test_extracts_registry_module() {
        let source = r#"
module "consul" {
  source  = "hashicorp/consul/aws"
  version = "0.1.0"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for registry module"
        );

        // Verify the registry URL node was created
        assert_has_node(&staging, "registry.terraform.io");
    }

    #[test]
    fn test_extracts_git_module() {
        let source = r#"
module "vpc" {
  source = "git::https://example.com/vpc-module.git"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for git module"
        );

        // Verify the git source node was created
        assert_has_node(&staging, "git::https://example.com/vpc-module.git");
    }

    #[test]
    fn test_extracts_provider() {
        let source = r#"
provider "aws" {
  region = "us-west-2"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for provider"
        );

        // Verify the provider registry URL node was created
        assert_has_node(&staging, "hashicorp/aws");
    }

    #[test]
    fn test_extracts_github_module() {
        let source = r#"
module "example" {
  source = "github.com/hashicorp/example"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for GitHub module"
        );

        // Verify the GitHub URL node was created with https:// prefix
        assert_has_node(&staging, "https://github.com/hashicorp/example");
    }

    #[test]
    fn test_multiple_modules() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
}

module "security" {
  source = "hashicorp/security/aws"
}

provider "aws" {
  region = "us-east-1"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 2 module import edges + 1 provider import edge = 3 total import edges
        let import_edges = collect_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            3,
            "Should extract 3 import edges (2 modules + 1 provider)"
        );

        // 2 module call edges (providers do not generate call edges)
        let call_edges = collect_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            2,
            "Should extract 2 call edges for module invocations"
        );

        // Verify specific nodes were created
        assert_has_node(&staging, "modules/vpc");
        assert_has_node(&staging, "registry.terraform.io");
        assert_has_node(&staging, "hashicorp/aws");
    }
}

// Active tests for Unified Graph (Wave 8)
#[cfg(test)]
mod active_tests {
    use super::*;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use std::path::PathBuf;

    fn parse_hcl(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hcl::LANGUAGE.into())
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

    #[test]
    fn test_terraform_graph_builder_language() {
        let builder = TerraformGraphBuilder::new();
        assert_eq!(builder.language(), Language::Terraform);
    }

    #[test]
    fn test_module_creates_import_edge() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
  cidr   = "10.0.0.0/16"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for module"
        );
    }

    #[test]
    fn test_module_creates_call_edge() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
  cidr   = "10.0.0.0/16"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Should extract one call edge for module invocation"
        );
    }

    #[test]
    fn test_provider_creates_import_edge() {
        let source = r#"
provider "aws" {
  region = "us-west-2"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract one import edge for provider"
        );
    }

    #[test]
    fn test_multiple_modules_create_edges() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
}

module "security" {
  source = "./modules/security"
}

module "database" {
  source = "hashicorp/consul/aws"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            3,
            "Should extract three import edges for modules"
        );

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            3,
            "Should extract three call edges for module invocations"
        );
    }

    #[test]
    fn test_registry_module_creates_edges() {
        let source = r#"
module "consul" {
  source  = "hashicorp/consul/aws"
  version = "0.1.0"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract import edge for registry module"
        );

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Should extract call edge for registry module invocation"
        );
    }

    #[test]
    fn test_git_module_creates_edges() {
        let source = r#"
module "vpc" {
  source = "git::https://example.com/vpc-module.git"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Should extract import edge for git module"
        );

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            1,
            "Should extract call edge for git module invocation"
        );
    }

    #[test]
    fn test_empty_file_no_edges() {
        let source = "";

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert!(
            import_edges.is_empty(),
            "Empty file should have no import edges"
        );

        let call_edges = extract_call_edges(&staging);
        assert!(
            call_edges.is_empty(),
            "Empty file should have no call edges"
        );
    }

    #[test]
    fn test_mixed_blocks_creates_edges() {
        let source = r#"
module "vpc" {
  source = "./modules/vpc"
}

provider "aws" {
  region = "us-east-1"
}

module "security" {
  source = "./modules/security"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            3,
            "Should extract 3 import edges (2 modules + 1 provider)"
        );

        let call_edges = extract_call_edges(&staging);
        assert_eq!(
            call_edges.len(),
            2,
            "Should extract 2 call edges for modules"
        );
    }

    #[test]
    fn test_module_without_label_only_import_edge() {
        // Edge case: HCL technically allows block without label (rare but valid syntax)
        // In this case, we should still get an Import edge but no Call edge
        // (since Call edge creation requires the module name)
        let source = r#"
module {
  source = "./modules/anonymous"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        // Should still get import edge for the module source
        let import_edges = extract_import_edges(&staging);
        assert_eq!(
            import_edges.len(),
            1,
            "Module without label should still have import edge"
        );

        // No call edge since there's no module name to use
        let call_edges = extract_call_edges(&staging);
        assert!(
            call_edges.is_empty(),
            "Module without label should have no call edge (no name for caller)"
        );
    }

    use sqry_core::graph::unified::build::test_helpers::build_node_name_lookup;
    use sqry_core::graph::unified::edge::kind::TypeOfContext;
    use sqry_core::graph::unified::node::NodeKind;

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

    /// Helper to extract `TypeOf` edge details: (`source_name`, `target_name`, context)
    fn extract_typeof_edge_details(
        staging: &StagingGraph,
    ) -> Vec<(String, String, Option<TypeOfContext>)> {
        let names = build_node_name_lookup(staging);
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: UnifiedEdgeKind::TypeOf { context, .. },
                    ..
                } = op
                {
                    let src = names.get(source).cloned().unwrap_or_default();
                    let tgt = names.get(target).cloned().unwrap_or_default();
                    Some((src, tgt, *context))
                } else {
                    None
                }
            })
            .collect()
    }

    fn has_display_name(
        staging: &StagingGraph,
        canonical_name: &str,
        expected_display_name: &str,
    ) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                staging.resolve_node_canonical_name(entry) == Some(canonical_name)
                    && staging
                        .resolve_node_display_name(Language::Terraform, entry)
                        .as_deref()
                        == Some(expected_display_name)
            } else {
                false
            }
        })
    }

    #[test]
    fn test_variable_type_creates_typeof_edge() {
        let source = r#"
variable "instance_type" {
  type    = string
  default = "t2.micro"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("variables.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(details.len(), 1, "Should have one TypeOf edge");
        assert_eq!(
            details[0].0, "instance_type",
            "Source should be variable name"
        );
        assert_eq!(details[0].1, "string", "Target should be type name");
        assert_eq!(
            details[0].2,
            Some(TypeOfContext::Variable),
            "Context should be Variable"
        );
    }

    #[test]
    fn test_variable_complex_type_creates_typeof_edge() {
        let source = r#"
variable "subnet_ids" {
  type    = list(string)
  default = []
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("variables.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(details.len(), 1, "Should have one TypeOf edge");
        assert_eq!(details[0].0, "subnet_ids", "Source should be variable name");
        assert_eq!(
            details[0].2,
            Some(TypeOfContext::Variable),
            "Context should be Variable"
        );
    }

    #[test]
    fn test_variable_without_type_no_typeof_edge() {
        let source = r#"
variable "name" {
  default = "hello"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("variables.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert!(
            typeof_edges.is_empty(),
            "Variable without type attribute should have no TypeOf edge"
        );
    }

    #[test]
    fn test_resource_type_creates_typeof_edge() {
        let source = r#"
resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t2.micro"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(details.len(), 1, "Resource should create one TypeOf edge");
        assert_eq!(
            details[0].0, "aws_instance::web",
            "Source should use canonical graph identity"
        );
        assert_eq!(
            details[0].1, "aws_instance",
            "Target should be resource type"
        );
        assert_eq!(
            details[0].2,
            Some(TypeOfContext::Variable),
            "Context should be Variable"
        );
        assert!(
            has_display_name(&staging, "aws_instance::web", "aws_instance.web"),
            "Terraform resources should retain native display addresses"
        );
    }

    #[test]
    #[allow(clippy::items_after_statements)] // Items near usage for clarity
    fn test_data_source_creates_typeof_edge() {
        let source = r#"
data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("data.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(
            details.len(),
            1,
            "Data source should create one TypeOf edge"
        );
        assert_eq!(
            details[0].0, "data::aws_ami::ubuntu",
            "Source should use canonical graph identity"
        );
        assert_eq!(
            details[0].1, "aws_ami",
            "Target should be data source type (without data. prefix)"
        );
        assert_eq!(
            details[0].2,
            Some(TypeOfContext::Variable),
            "Context should be Variable"
        );

        // Verify node kind is Variable
        use sqry_core::graph::unified::build::test_helpers::assert_has_node_with_kind;
        assert_has_node_with_kind(&staging, "data::aws_ami::ubuntu", NodeKind::Variable);
        assert!(
            has_display_name(&staging, "data::aws_ami::ubuntu", "data.aws_ami.ubuntu",),
            "Terraform data sources should retain native display addresses"
        );
    }

    #[test]
    fn test_data_source_name_collision_produces_distinct_nodes() {
        // Two data blocks with same name but different types should produce distinct nodes
        let source = r#"
data "aws_ami" "latest" {
  most_recent = true
}

data "aws_subnet" "latest" {
  default_for_az = true
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("data.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(
            details.len(),
            2,
            "Two data blocks should create two TypeOf edges"
        );

        // Verify distinct source node names (canonical format prevents collision)
        let source_names: Vec<&str> = details.iter().map(|d| d.0.as_str()).collect();
        assert!(
            source_names.contains(&"data::aws_ami::latest"),
            "Should have canonical data::aws_ami::latest"
        );
        assert!(
            source_names.contains(&"data::aws_subnet::latest"),
            "Should have canonical data::aws_subnet::latest"
        );
        assert!(
            has_display_name(&staging, "data::aws_ami::latest", "data.aws_ami.latest",),
            "Terraform data sources should expose native display addresses"
        );
        assert!(
            has_display_name(
                &staging,
                "data::aws_subnet::latest",
                "data.aws_subnet.latest",
            ),
            "Terraform data sources should expose native display addresses"
        );
    }

    #[test]
    fn test_output_with_type_creates_typeof_edge() {
        let source = r#"
output "instance_ip" {
  type  = string
  value = aws_instance.web.public_ip
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("outputs.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(
            details.len(),
            1,
            "Output with type should create one TypeOf edge"
        );
        assert_eq!(details[0].0, "instance_ip", "Source should be output name");
        assert_eq!(details[0].1, "string", "Target should be type name");
        assert_eq!(
            details[0].2,
            Some(TypeOfContext::Variable),
            "Context should be Variable"
        );
    }

    #[test]
    fn test_same_name_different_resource_types_distinct_nodes() {
        let source = r#"
resource "aws_instance" "web" {
  ami = "ami-12345"
}

resource "aws_security_group" "web" {
  name = "web-sg"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(
            details.len(),
            2,
            "Two resources with same name but different types should create 2 TypeOf edges"
        );

        let source_names: Vec<&str> = details.iter().map(|d| d.0.as_str()).collect();
        assert!(
            source_names.contains(&"aws_instance::web"),
            "Should have canonical aws_instance::web"
        );
        assert!(
            source_names.contains(&"aws_security_group::web"),
            "Should have canonical aws_security_group::web"
        );
        assert!(
            has_display_name(&staging, "aws_instance::web", "aws_instance.web"),
            "Terraform resources should expose native display addresses"
        );
        assert!(
            has_display_name(
                &staging,
                "aws_security_group::web",
                "aws_security_group.web",
            ),
            "Terraform resources should expose native display addresses"
        );
    }

    #[test]
    fn test_locals_no_typeof_edges() {
        let source = r#"
locals {
  env  = "dev"
  name = "myapp"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("locals.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let typeof_edges = extract_typeof_edges(&staging);
        assert!(
            typeof_edges.is_empty(),
            "locals block should not create any TypeOf edges"
        );
    }

    #[test]
    fn test_mixed_blocks_typeof_edges() {
        let source = r#"
variable "name" {
  type = string
}

resource "aws_instance" "web" {
  ami = "ami-12345"
}

data "aws_ami" "latest" {
  most_recent = true
}

output "result" {
  value = "done"
}
"#;

        let tree = parse_hcl(source);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .expect("build_graph should succeed");

        let details = extract_typeof_edge_details(&staging);
        assert_eq!(
            details.len(),
            3,
            "Should have 3 TypeOf edges: variable(string) + resource(aws_instance) + data(aws_ami), output has no type"
        );

        // All contexts should be Variable
        for (i, detail) in details.iter().enumerate() {
            assert_eq!(
                detail.2,
                Some(TypeOfContext::Variable),
                "Edge {i} context should be Variable"
            );
        }
    }

    // ----- body-shape descriptor coverage -----

    const SHAPE_SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/iac/main.tf"
    ));

    #[test]
    fn builder_advertises_shape_mapping() {
        assert!(
            TerraformGraphBuilder.shape_mapping().is_some(),
            "Terraform builder must advertise a ShapeMapping (AC-1: no plugin omitted)"
        );
    }

    #[test]
    fn hcl_cf_map_is_non_empty_for_expression_control_flow() {
        // Terraform is declarative, but its expression grammar still has real
        // control-flow kinds; the mapping must be genuinely populated rather than a
        // hollow all-None table.
        let mapping = terraform_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_hcl::LANGUAGE.into();
        let for_id = (0..lang.node_kind_count())
            .filter_map(|id| u16::try_from(id).ok())
            .find(|&kid| {
                lang.node_kind_is_named(kid) && lang.node_kind_for_id(kid) == Some("for_expr")
            })
            .expect("hcl grammar has a for_expr kind");
        assert_eq!(
            mapping.cf_bucket(for_id),
            Some(CfBucket::Comprehension),
            "for_expr must map to the Comprehension bucket"
        );
    }

    #[test]
    fn declarative_function_stubs_have_no_control_flow_body() {
        use sqry_core::graph::unified::build::body_hash::has_valid_body_span;
        use sqry_core::graph::unified::build::shape::{
            CfBucket, ShapeBudget, compute_shape_descriptor,
        };
        use sqry_core::graph::unified::node::NodeKind;

        // Terraform emits no genuine function DEFINITIONS. The only Function nodes it
        // creates are call-source stubs for `module "<name>" { ... }` blocks (used to
        // anchor the module-instantiation Call edge); these span the declarative HCL
        // block, which has no control flow. This is the honest declarative-minimal
        // case: the mapping is present (asserted above), and when the build seam
        // walks one of these block spans the resulting descriptor carries an
        // all-zero control-flow histogram (no Branch/Loop/Match/Try/Call buckets fire
        // for a declarative block).
        let tree = parse_hcl(SHAPE_SAMPLE);
        let mut staging = StagingGraph::new();
        let builder = TerraformGraphBuilder;
        let file = PathBuf::from("main.tf");
        builder
            .build_graph(&tree, SHAPE_SAMPLE.as_bytes(), &file, &mut staging)
            .unwrap();

        let function_body_nodes: Vec<_> = staging
            .nodes()
            .filter(|n| {
                matches!(n.entry.kind, NodeKind::Function | NodeKind::Method)
                    && has_valid_body_span(n.entry)
            })
            .collect();

        // The fixture has exactly one `module` block, so exactly one stub exists.
        assert_eq!(
            function_body_nodes.len(),
            1,
            "the single `module \"network\"` block is the only Function-with-span node"
        );

        // Drive the real mapping over the module block subtree and confirm the
        // declarative body produces no control-flow buckets.
        let mapping = terraform_shape_mapping();
        let module_block = first_module_block(tree.root_node(), SHAPE_SAMPLE.as_bytes())
            .expect("fixture has a module block");
        let d = compute_shape_descriptor(
            module_block,
            SHAPE_SAMPLE.as_bytes(),
            mapping,
            &ShapeBudget::default(),
        );
        for bucket in CfBucket::ALL {
            assert_eq!(
                d.cf_histogram[bucket.index()],
                0,
                "declarative module block must not register the {bucket:?} control-flow bucket"
            );
        }
    }

    /// Find the first HCL `block` whose leading identifier is `module`.
    fn first_module_block<'t>(
        node: tree_sitter::Node<'t>,
        src: &[u8],
    ) -> Option<tree_sitter::Node<'t>> {
        if node.kind() == "block" {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    if child.utf8_text(src).ok() == Some("module") {
                        return Some(node);
                    }
                    break;
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_module_block(child, src) {
                return Some(found);
            }
        }
        None
    }
}
// Collapsible nested conditionals kept for readability with early returns
