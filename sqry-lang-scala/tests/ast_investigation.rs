//! AST Investigation for failing edge cases

use tree_sitter::Parser;

fn parse_scala(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .expect("Failed to set Scala language");
    parser.parse(source, None).expect("Failed to parse Scala")
}

fn print_ast(node: tree_sitter::Node, content: &[u8], indent: usize) {
    let indent_str = "  ".repeat(indent);
    let node_text = node.utf8_text(content).unwrap_or("<invalid utf8>");
    let text_preview = if node_text.len() > 60 {
        format!("{}...", &node_text[..57])
    } else {
        node_text.replace('\n', "\\n")
    };

    println!("{indent_str}[{}] '{}'", node.kind(), text_preview);

    // Print children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_ast(child, content, indent + 1);
    }
}

#[test]
#[ignore = "AST investigation - run manually"]
fn investigate_case_class_constructor() {
    let source = r#"
case class User(name: String, age: Int, email: String)
"#;

    let tree = parse_scala(source);
    println!("\n=== CASE CLASS CONSTRUCTOR AST ===");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "AST investigation - run manually"]
fn investigate_function_type() {
    let source = r#"
class Handler {
  val callback: (String, Int) => Boolean = null
}
"#;

    let tree = parse_scala(source);
    println!("\n=== FUNCTION TYPE AST ===");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "AST investigation - run manually"]
fn investigate_simple_function_params() {
    let source = r#"
def greet(name: String): Unit = println(name)
"#;

    let tree = parse_scala(source);
    println!("\n=== SIMPLE FUNCTION PARAMS AST ===");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}
