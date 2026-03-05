use tree_sitter::{Node, Parser};

#[test]
fn inspect_method_parent_chain_shows_class_available() {
    let mut parser = Parser::new();
    let language = tree_sitter_php::LANGUAGE_PHP.into();
    parser.set_language(&language).unwrap();

    let source_code = r#"<?php
namespace App\Controller;

class UserController {
    public function index() {
        return "index";
    }
}
"#;

    let tree = parser.parse(source_code, None).unwrap();
    let root = tree.root_node();

    println!("\n=== Testing Parent Traversal from Method Node ===");
    find_and_walk_method(&root, source_code.as_bytes());
}

fn find_and_walk_method(node: &Node, source: &[u8]) {
    if node.kind() == "method_declaration" {
        if let Some(method_name) = node_name(node, source) {
            println!("\nFound method: '{}'", method_name);

            println!("\nWalking up parent chain from method node:");
            walk_parent_chain(node, source);
        }
        return;
    }

    walk_children(node, source);
}

fn node_name<'a>(node: &Node, source: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("name")
        .and_then(|name_node| name_node.utf8_text(source).ok())
}

fn walk_parent_chain(node: &Node, source: &[u8]) {
    let mut current = node.parent();
    let mut level = 1;

    while let Some(parent) = current {
        print!("  Level {}: {:30}", level, parent.kind());
        describe_parent_node(&parent, source);
        current = parent.parent();
        level += 1;
    }
}

fn describe_parent_node(parent: &Node, source: &[u8]) {
    match parent.kind() {
        "class_declaration" | "trait_declaration" | "interface_declaration" => {
            if let Some(name) = node_name(parent, source) {
                println!(" -> ✅ Name: '{}'", name);
                println!(
                    "\n  CONCLUSION: Tree-sitter DOES provide class name via parent traversal!"
                );
                println!(
                    "  We can use: node.parent() chain to find '{}' from method node",
                    name
                );
            } else {
                println!();
            }
        }
        "namespace_definition" => {
            if let Some(name) = node_name(parent, source) {
                println!(" -> Namespace: '{}'", name);
            } else {
                println!();
            }
        }
        _ => println!(),
    }
}

fn walk_children(node: &Node, source: &[u8]) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_and_walk_method(&child, source);
    }
}
