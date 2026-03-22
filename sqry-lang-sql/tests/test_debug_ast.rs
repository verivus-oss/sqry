//! Debug test to inspect SQL AST structure

use tree_sitter::Parser;

fn print_tree(node: tree_sitter::Node, source: &[u8], indent: usize) {
    let prefix = "  ".repeat(indent);
    let text = node.utf8_text(source).unwrap_or("<error>");
    let short_text = if text.len() > 40 {
        format!("{}...", &text[..40])
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
fn debug_materialized_view_ast() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("set SQL language");

    let code = b"CREATE MATERIALIZED VIEW user_stats AS SELECT * FROM users;";
    println!("\n{}", "=".repeat(70));
    println!("MATERIALIZED VIEW AST:");
    println!("{}", "=".repeat(70));

    if let Some(tree) = parser.parse(code, None) {
        print_tree(tree.root_node(), code, 0);
    } else {
        panic!("Failed to parse");
    }
}

#[test]
fn debug_regular_view_ast() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("set SQL language");

    let code = b"CREATE VIEW active_users AS SELECT * FROM users;";
    println!("\n{}", "=".repeat(70));
    println!("REGULAR VIEW AST:");
    println!("{}", "=".repeat(70));

    if let Some(tree) = parser.parse(code, None) {
        print_tree(tree.root_node(), code, 0);
    } else {
        panic!("Failed to parse");
    }
}

#[test]
fn debug_plpgsql_function_ast() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("set SQL language");

    let code = b"CREATE FUNCTION add(a INT, b INT) RETURNS INT AS $$
BEGIN
    RETURN a + b;
END;
$$ LANGUAGE plpgsql;";

    println!("\n{}", "=".repeat(70));
    println!("PL/pgSQL FUNCTION AST:");
    println!("{}", "=".repeat(70));

    if let Some(tree) = parser.parse(code, None) {
        print_tree(tree.root_node(), code, 0);
    } else {
        panic!("Failed to parse");
    }
}
