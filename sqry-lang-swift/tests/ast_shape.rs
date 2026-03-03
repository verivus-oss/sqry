//! Swift AST shape tests.
//!
//! These are lightweight sanity checks to keep the Swift `GraphBuilder`
//! implementation aligned with the tree-sitter Swift grammar.

#![allow(deprecated)]

use tree_sitter::{Node, Parser, Tree};

fn parse_swift(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("load Swift grammar");
    parser.parse(source, None).expect("parse Swift source")
}

fn find_first_node<'a>(root: Node<'a>, predicate: &impl Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    if predicate(root) {
        return Some(root);
    }

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(found) = find_first_node(child, predicate) {
            return Some(found);
        }
    }

    None
}

#[test]
fn swift_call_node_exposes_callee_child() {
    let source = r#"
func validate(input: String) -> Bool { !input.isEmpty }
func process(data: String) {
  if validate(input: data) { }
  print(data)
}
process(data: "test")
"#;

    let tree = parse_swift(source);
    let root = tree.root_node();

    let call_node =
        find_first_node(root, &|node| node.kind().contains("call")).unwrap_or_else(|| {
            panic!(
                "Expected at least one call-like node in Swift AST. AST: {}",
                root.to_sexp()
            );
        });

    let callee_node = call_node.named_child(0);

    assert!(
        callee_node.is_some(),
        "Call-like node '{}' has no callee child. Node: {}",
        call_node.kind(),
        call_node.to_sexp()
    );
}
