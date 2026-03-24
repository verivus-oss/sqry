/// Temporary test to explore the tree-sitter-zig AST structure
use tree_sitter::{Node, Parser};

fn print_ast(node: Node, source: &str, indent: usize) {
    let kind = node.kind();
    let text = node.utf8_text(source.as_bytes()).unwrap_or("<error>");
    let truncated = if text.len() > 50 {
        format!("{}...", &text[..50])
    } else {
        text.to_string()
    };

    println!(
        "{:indent$}{} \"{}\"",
        "",
        kind,
        truncated,
        indent = indent * 2
    );

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_ast(child, source, indent + 1);
    }
}

#[test]
fn explore_function_ast() {
    let code = "pub fn add(a: i32, b: i32) i32 {
    return a + b;
}";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Function AST ===");
    print_ast(tree.root_node(), code, 0);
}

#[test]
fn explore_test_ast() {
    let code = "test \"basic addition\" {
    try std.testing.expect(1 + 1 == 2);
}";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Test AST ===");
    print_ast(tree.root_node(), code, 0);
}

#[test]
fn explore_const_ast() {
    let code = "const std = @import(\"std\");";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Const/Import AST ===");
    print_ast(tree.root_node(), code, 0);
}

#[test]
fn explore_struct_ast() {
    let code = "const Point = struct {
    x: f32,
    y: f32,
};";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Struct AST ===");
    print_ast(tree.root_node(), code, 0);
}

#[test]
fn explore_usingnamespace_ast() {
    let code = "const std = @import(\"std\");\npub usingnamespace std.testing;";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Usingnamespace AST ===");
    print_ast(tree.root_node(), code, 0);
}

#[test]
fn explore_optional_type_ast() {
    let code = "var maybe_user: ?User = null;";

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(code, None).unwrap();

    println!("\n=== Optional Type AST ===");
    print_ast(tree.root_node(), code, 0);
}
