//! Debug test for PL/pgSQL function calls

use tree_sitter::Parser;

fn print_tree(node: tree_sitter::Node, source: &[u8], indent: usize) {
    let prefix = "  ".repeat(indent);
    let text = node.utf8_text(source).unwrap_or("<error>");
    let short_text = if text.len() > 60 {
        format!("{}...", &text[..60])
    } else {
        text.to_string()
    };

    if node.is_named() {
        if node.child_count() == 0 {
            println!("{}{} = '{}'", prefix, node.kind(), short_text);
        } else {
            println!("{}{}", prefix, node.kind());
        }
    } else {
        println!("{}[{}] = '{}'", prefix, node.kind(), short_text);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_tree(child, source, indent + 1);
    }
}

#[test]
fn debug_nested_function_calls() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("set SQL language");

    let code = b"CREATE FUNCTION add(a INT, b INT) RETURNS INT AS $$
BEGIN
    RETURN a + b;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION compute(x INT, y INT, z INT) RETURNS INT AS $$
DECLARE
    sum_val INT;
BEGIN
    sum_val := add(x, y);
    RETURN sum_val;
END;
$$ LANGUAGE plpgsql;";

    println!("\n{}", "=".repeat(70));
    println!("NESTED FUNCTION CALLS AST:");
    println!("{}", "=".repeat(70));

    if let Some(tree) = parser.parse(code, None) {
        print_tree(tree.root_node(), code, 0);
    } else {
        panic!("Failed to parse");
    }
}
