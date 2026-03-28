//! Tests for Terraform module export edge creation.

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::build::test_helpers::build_node_name_lookup;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_terraform::TerraformPlugin;
use std::fs;
use tempfile::TempDir;

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = TerraformPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.tf");
    fs::write(&file, source).expect("write test source");
    let tree = plugin.parse_ast(source).expect("parse source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

fn has_export_edge(staging: &StagingGraph, exported_name: &str) -> bool {
    let nodes = build_node_name_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let target_name = nodes.get(target).map(String::as_str);
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

fn has_display_name(
    staging: &StagingGraph,
    canonical_name: &str,
    expected_display_name: &str,
    expected_kind: NodeKind,
) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            entry.kind == expected_kind
                && staging.resolve_node_canonical_name(entry) == Some(canonical_name)
                && staging
                    .resolve_node_display_name(Language::Terraform, entry)
                    .as_deref()
                    == Some(expected_display_name)
        } else {
            false
        }
    })
}

// ===== Export Edge Tests =====

#[test]
fn test_output_blocks_exported() {
    let content = b"\
output \"vpc_id\" {
  description = \"The ID of the VPC\"
  value       = aws_vpc.main.id
}

output \"subnet_ids\" {
  description = \"List of subnet IDs\"
  value       = aws_subnet.private[*].id
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "vpc_id"),
        "Expected export edge for output vpc_id"
    );
    assert!(
        has_export_edge(&staging, "subnet_ids"),
        "Expected export edge for output subnet_ids"
    );
}

#[test]
fn test_variable_blocks_exported() {
    let content = b"\
variable \"region\" {
  description = \"AWS region\"
  type        = string
  default     = \"us-east-1\"
}

variable \"instance_count\" {
  description = \"Number of instances\"
  type        = number
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "region"),
        "Expected export edge for variable region"
    );
    assert!(
        has_export_edge(&staging, "instance_count"),
        "Expected export edge for variable instance_count"
    );
}

#[test]
fn test_resource_blocks_exported() {
    let content = b"\
resource \"aws_vpc\" \"main\" {
  cidr_block = \"10.0.0.0/16\"

  tags = {
    Name = \"main-vpc\"
  }
}

resource \"aws_subnet\" \"private\" {
  vpc_id     = aws_vpc.main.id
  cidr_block = \"10.0.1.0/24\"
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "aws_vpc::main"),
        "Expected export edge for resource aws_vpc::main"
    );
    assert!(
        has_export_edge(&staging, "aws_subnet::private"),
        "Expected export edge for resource aws_subnet::private"
    );
    assert!(
        has_display_name(
            &staging,
            "aws_vpc::main",
            "aws_vpc.main",
            NodeKind::Variable
        ),
        "Expected native Terraform display name for aws_vpc::main"
    );
    assert!(
        has_display_name(
            &staging,
            "aws_subnet::private",
            "aws_subnet.private",
            NodeKind::Variable,
        ),
        "Expected native Terraform display name for aws_subnet::private"
    );
}

#[test]
fn test_mixed_blocks() {
    let content = b"\
variable \"environment\" {
  type    = string
  default = \"dev\"
}

resource \"aws_s3_bucket\" \"data\" {
  bucket = \"my-data-bucket\"
}

output \"bucket_arn\" {
  value = aws_s3_bucket.data.arn
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "environment"),
        "Expected export edge for variable environment"
    );
    assert!(
        has_export_edge(&staging, "aws_s3_bucket::data"),
        "Expected export edge for resource aws_s3_bucket::data"
    );
    assert!(
        has_export_edge(&staging, "bucket_arn"),
        "Expected export edge for output bucket_arn"
    );
    assert!(
        has_display_name(
            &staging,
            "aws_s3_bucket::data",
            "aws_s3_bucket.data",
            NodeKind::Variable,
        ),
        "Expected native Terraform display name for aws_s3_bucket::data"
    );
}

#[test]
fn test_data_source_blocks_exported() {
    let content = b"\
data \"aws_ami\" \"ubuntu\" {
  most_recent = true
  owners      = [\"099720109477\"]
}

data \"aws_vpc\" \"default\" {
  default = true
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "data::aws_ami::ubuntu"),
        "Expected export edge for data source data::aws_ami::ubuntu"
    );
    assert!(
        has_export_edge(&staging, "data::aws_vpc::default"),
        "Expected export edge for data source data::aws_vpc::default"
    );
    assert!(
        has_display_name(
            &staging,
            "data::aws_ami::ubuntu",
            "data.aws_ami.ubuntu",
            NodeKind::Variable,
        ),
        "Expected native Terraform display name for data::aws_ami::ubuntu"
    );
    assert!(
        has_display_name(
            &staging,
            "data::aws_vpc::default",
            "data.aws_vpc.default",
            NodeKind::Variable,
        ),
        "Expected native Terraform display name for data::aws_vpc::default"
    );
}
