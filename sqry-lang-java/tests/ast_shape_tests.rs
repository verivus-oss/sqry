//! AST shape validation for Java grammar assumptions.

use std::collections::VecDeque;
use tree_sitter::{Node, Tree};

fn parse_java(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_java::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Java grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Java code")
}

fn collect_nodes_by_kind<'a>(root: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root);
    while let Some(node) = queue.pop_front() {
        if node.kind() == kind {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            queue.push_back(child);
        }
    }
    out
}

fn node_has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return true;
        }
    }
    false
}

#[test]
fn ast_shape_switch_classic_has_group() {
    let content = r"
class C {
    void test(int value) {
        switch (value) {
            case 1:
                int x = 1;
                break;
            case 2:
                int y = 2;
                break;
        }
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let groups = collect_nodes_by_kind(root, "switch_block_statement_group");
    assert!(
        !groups.is_empty(),
        "Expected switch_block_statement_group in classic switch"
    );
}

#[test]
fn ast_shape_switch_arrow_has_rule() {
    let content = r"
class C {
    int test(int value) {
        return switch (value) {
            case 1 -> 1;
            default -> 2;
        };
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let rules = collect_nodes_by_kind(root, "switch_rule");
    assert!(!rules.is_empty(), "Expected switch_rule in arrow switch");
}

#[test]
fn ast_shape_instanceof_name_field() {
    let content = r"
class C {
    void test(Object obj) {
        if (obj instanceof String s) {
            System.out.println(s);
        }
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let instanceof_nodes = collect_nodes_by_kind(root, "instanceof_expression");
    assert!(
        !instanceof_nodes.is_empty(),
        "Expected instanceof_expression node"
    );
    let has_name = instanceof_nodes
        .iter()
        .any(|node| node.child_by_field_name("name").is_some());
    assert!(
        has_name,
        "Expected instanceof_expression to have name field"
    );
}

#[test]
fn ast_shape_switch_guard_node() {
    let content = r"
sealed interface Shape {}
record Point(int x) implements Shape {}

class C {
    int test(Shape shape) {
        return switch (shape) {
            case Point(int x) when x > 0 -> x;
            default -> 0;
        };
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let labels = collect_nodes_by_kind(root, "switch_label");
    assert!(!labels.is_empty(), "Expected switch_label nodes");
    let has_guard = labels.iter().any(|label| {
        label.child_by_field_name("guard").is_some() || node_has_child_kind(*label, "guard")
    });
    assert!(has_guard, "Expected guard node/field under switch_label");
}

#[test]
fn ast_shape_compact_constructor_and_record() {
    let content = r"
record R(int x) {
    R {
        System.out.println(x);
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let records = collect_nodes_by_kind(root, "record_declaration");
    let compact = collect_nodes_by_kind(root, "compact_constructor_declaration");
    assert!(!records.is_empty(), "Expected record_declaration");
    assert!(
        !compact.is_empty(),
        "Expected compact_constructor_declaration"
    );
}

#[test]
fn ast_shape_static_initializer_node() {
    let content = r"
class C {
    static {
        int x = 1;
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let statics = collect_nodes_by_kind(root, "static_initializer");
    assert!(!statics.is_empty(), "Expected static_initializer node");
}

#[test]
fn ast_shape_try_with_resources_node() {
    let content = r"
import java.io.Closeable;

class C {
    void test() throws Exception {
        try (Closeable c = null) {
            System.out.println(c);
        }
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let try_with = collect_nodes_by_kind(root, "try_with_resources_statement");
    if !try_with.is_empty() {
        return;
    }

    let try_nodes = collect_nodes_by_kind(root, "try_statement");
    let has_resources = try_nodes
        .iter()
        .any(|node| node.child_by_field_name("resources").is_some());
    assert!(
        has_resources,
        "Expected try_with_resources_statement or try_statement with resources field"
    );
}

#[test]
fn ast_shape_varargs_parameter_node() {
    let content = r"
class C {
    void test(String... args) {
        System.out.println(args.length);
    }
}
";
    let tree = parse_java(content);
    let root = tree.root_node();
    let spread_params = collect_nodes_by_kind(root, "spread_parameter");
    if !spread_params.is_empty() {
        return;
    }

    let formals = collect_nodes_by_kind(root, "formal_parameter");
    let has_ellipsis = formals.iter().any(|node| node_has_child_kind(*node, "..."));
    assert!(
        has_ellipsis,
        "Expected spread_parameter or formal_parameter with ellipsis"
    );
}
