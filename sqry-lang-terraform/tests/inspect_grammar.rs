//! Inspect tree-sitter-hcl grammar structure
#![allow(unexpected_cfgs)]

use tree_sitter::{Node, Parser};

fn print_tree(node: Node, source: &[u8], indent: usize) {
    let kind = node.kind();
    let text = node.utf8_text(source).unwrap_or("");
    let text_preview = if text.len() > 40 {
        format!("{}...", &text[..40].replace('\n', "\\n"))
    } else {
        text.replace('\n', "\\n")
    };

    println!("{}{:20} | {}", "  ".repeat(indent), kind, text_preview);

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            print_tree(child, source, indent + 1);
        }
    }
}

#[test]
fn inspect_terraform_ast() {
    let code = br#"
resource "aws_s3_bucket" "my_bucket" {
  bucket = "example-bucket"
}

variable "instance_type" {
  type = string
}

module "vpc" {
  source = "./modules/vpc"
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

output "bucket_name" {
  value = "test"
}

provider "aws" {
  region = "us-east-1"
}

locals {
  env = "dev"
}
"#;

    let mut parser = Parser::new();
    // Try LANGUAGE_EXT if available, otherwise LANGUAGE
    #[cfg(feature = "tree_sitter_language_ext")]
    let lang = unsafe { tree_sitter_hcl::LANGUAGE_EXT };
    #[cfg(not(feature = "tree_sitter_language_ext"))]
    let lang = tree_sitter_hcl::LANGUAGE;

    parser.set_language(&lang.into()).unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== TERRAFORM AST STRUCTURE ===");
    print_tree(tree.root_node(), code, 0);
}
