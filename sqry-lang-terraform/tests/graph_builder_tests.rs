//! Graph builder tests for the Terraform (HCL) language plugin.
//!
//! Covers:
//! - Module node creation
//! - Module source references (local, registry, git)
//! - Provider dependency edges
//! - Variable and output extraction
//! - Resource block handling
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_terraform::TerraformGraphBuilder;
use std::path::Path;

fn parse_terraform(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_hcl::LANGUAGE.into())
        .expect("failed to set HCL language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Terraform code")
}

fn count_edges_of_kind(staging: &StagingGraph, kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

fn count_import_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Imports { .. }))
}

fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::InternString { value, .. } = op {
            value.contains(pattern)
        } else {
            false
        }
    })
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.tf"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Terraform file should succeed");
}

#[test]
fn test_comments_only() {
    let source = r"
# This is a comment
// Another comment style
";
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.tf"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Comments-only Terraform file should succeed"
    );
}

// ==================== Module References ====================

#[test]
fn test_local_module_reference() {
    let source = r#"
module "vpc" {
  source = "./modules/vpc"

  cidr_block = "10.0.0.0/16"
}

module "subnets" {
  source = "./modules/subnets"

  vpc_id = module.vpc.id
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("main.tf"), &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for local module, got {import_count}"
    );
}

#[test]
fn test_registry_module_reference() {
    let source = r#"
module "eks" {
  source  = "terraform-aws-modules/eks/aws"
  version = "~> 19.0"

  cluster_name = "my-cluster"
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("eks.tf"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for registry module, got {import_count}"
    );
    assert!(
        has_interned_string_containing(&staging, "terraform-aws-modules")
            || has_interned_string_containing(&staging, "eks"),
        "Expected registry module reference in staging"
    );
}

#[test]
fn test_git_module_reference() {
    let source = r#"
module "security" {
  source = "git::https://github.com/myorg/terraform-modules.git//security"

  environment = "production"
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("security.tf"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for git module, got {import_count}"
    );
}

// ==================== Provider Blocks ====================

#[test]
fn test_provider_block() {
    let source = r#"
provider "aws" {
  region = "us-east-1"
}

provider "kubernetes" {
  host = "https://my-cluster.example.com"
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("providers.tf"),
        &mut staging,
    );
    assert!(result.is_ok(), "Provider blocks should succeed");
}

// ==================== Resource Blocks ====================

#[test]
fn test_resource_block() {
    let source = r#"
resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"

  tags = {
    Name = "main-vpc"
  }
}

resource "aws_subnet" "public" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.1.0/24"
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("vpc.tf"), &mut staging);
    assert!(result.is_ok(), "Resource blocks should succeed");
}

// ==================== Variable and Output Blocks ====================

#[test]
fn test_variable_block() {
    let source = r#"
variable "region" {
  description = "AWS region"
  type        = string
  default     = "us-east-1"
}

variable "environment" {
  description = "Deployment environment"
  type        = string
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("variables.tf"),
        &mut staging,
    );
    assert!(result.is_ok(), "Variable blocks should succeed");
}

#[test]
fn test_output_block() {
    let source = r#"
output "vpc_id" {
  description = "The ID of the VPC"
  value       = aws_vpc.main.id
}

output "subnet_ids" {
  description = "List of subnet IDs"
  value       = aws_subnet.public[*].id
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("outputs.tf"),
        &mut staging,
    );
    assert!(result.is_ok(), "Output blocks should succeed");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = TerraformGraphBuilder::new();
    assert_eq!(builder.language(), Language::Terraform);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TerraformGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_terraform() {
    // Incomplete HCL - tree-sitter is error-tolerant
    let source = r#"
module "broken" {
  source =
"#; // incomplete
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.tf"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_complete_infrastructure() {
    let source = r#"
provider "aws" {
  region = var.region
}

variable "region" {
  type    = string
  default = "us-west-2"
}

module "network" {
  source = "./modules/network"

  cidr = "10.0.0.0/16"
}

resource "aws_instance" "app" {
  ami           = "ami-12345678"
  instance_type = "t3.micro"

  subnet_id = module.network.subnet_id

  tags = {
    Name = "app-server"
  }
}

output "instance_ip" {
  value = aws_instance.app.public_ip
}
"#;
    let tree = parse_terraform(source);
    let mut staging = StagingGraph::new();
    let builder = TerraformGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("main.tf"), &mut staging);
    assert!(
        result.is_ok(),
        "Complete infrastructure should succeed: {:?}",
        result.err()
    );
}
