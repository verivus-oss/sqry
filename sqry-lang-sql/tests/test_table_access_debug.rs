//! Debug test to understand SELECT/INSERT/UPDATE/DELETE AST structures

use tree_sitter::Parser;

fn parse_sql(sql: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("Failed to set SQL language");
    parser.parse(sql, None).expect("Failed to parse SQL")
}

#[test]
fn debug_select_structure() {
    let sql = "SELECT * FROM accounts WHERE id = 1;";
    let tree = parse_sql(sql);
    eprintln!("SELECT tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_insert_structure() {
    let sql = "INSERT INTO accounts (balance) VALUES (100);";
    let tree = parse_sql(sql);
    eprintln!("INSERT tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_update_structure() {
    let sql = "UPDATE accounts SET balance = 200 WHERE id = 1;";
    let tree = parse_sql(sql);
    eprintln!("UPDATE tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_delete_structure() {
    let sql = "DELETE FROM accounts WHERE id = 1;";
    let tree = parse_sql(sql);
    eprintln!("DELETE tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_function_with_select() {
    let sql = r"
        CREATE FUNCTION calculate_total()
        RETURNS INT AS $$
            SELECT SUM(amount) FROM orders;
        $$ LANGUAGE sql;
    ";
    let tree = parse_sql(sql);
    eprintln!("FUNCTION with SELECT tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_function_name_query() {
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Query, QueryCursor};

    let sql = r#"
        CREATE FUNCTION public.calculate_total(order_id INT)
        RETURNS DECIMAL(10,2)
        AS $$
            SELECT SUM(amount) FROM order_items WHERE order_id = $1;
        $$ LANGUAGE sql;
    "#;

    let tree = parse_sql(sql);

    // The query from SqlGraphBuilder
    let query = Query::new(
        &tree_sitter_sequel::LANGUAGE.into(),
        r"
        (create_function
          (object_reference
            name: (identifier) @func.name)) @func
        ",
    )
    .expect("Query should compile");

    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(&query, tree.root_node(), sql.as_bytes());

    eprintln!("AST: {}", tree.root_node().to_sexp());
    eprintln!("\nCapture names: {:?}", capture_names);
    eprintln!("\nMatches:");

    let mut match_count = 0;
    while let Some(m) = matches.next() {
        match_count += 1;
        eprintln!("  Match pattern: {}", m.pattern_index);
        for capture in m.captures {
            let name = capture_names[capture.index as usize];
            let text = capture
                .node
                .utf8_text(sql.as_bytes())
                .unwrap_or("<invalid>");
            eprintln!("    {}: '{}'", name, text);
        }
    }

    assert!(match_count > 0, "Expected at least one match");
}

#[test]
fn debug_graph_builder_staging() {
    use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
    use sqry_lang_sql::relations::SqlGraphBuilder;

    let sql = r#"
        CREATE FUNCTION calculate_total()
        RETURNS INT AS $$
            SELECT SUM(amount) FROM orders;
        $$ LANGUAGE sql;
    "#;

    let tree = parse_sql(sql);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            sql.as_bytes(),
            std::path::Path::new("test.sql"),
            &mut staging,
        )
        .expect("Graph building should succeed");

    // Check what operations were staged
    eprintln!("Staging operations: {}", staging.operations().len());
    for (i, op) in staging.operations().iter().enumerate() {
        eprintln!("  Op {}: {:?}", i, op);
    }

    assert!(
        !staging.operations().is_empty(),
        "Expected at least one staging operation"
    );
}

#[test]
fn debug_invocation_ast() {
    let sql = "SELECT SUM(amount) FROM orders;";
    let tree = parse_sql(sql);
    eprintln!("Invocation AST: {}", tree.root_node().to_sexp());
}

#[test]
fn debug_simple_function_call_ast() {
    // Try different function call patterns
    let sql1 = "SELECT now();";
    let sql2 = "SELECT COUNT(*) FROM users;";

    let tree1 = parse_sql(sql1);
    let tree2 = parse_sql(sql2);

    eprintln!("now() AST: {}", tree1.root_node().to_sexp());
    eprintln!("\nCOUNT(*) AST: {}", tree2.root_node().to_sexp());
}

#[test]
fn debug_join_structure() {
    let sql = "SELECT * FROM users JOIN orders ON users.id = orders.user_id;";
    let tree = parse_sql(sql);
    eprintln!("JOIN tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_multiple_join_structure() {
    let sql = "SELECT * FROM users u JOIN orders o ON u.id = o.user_id LEFT JOIN products p ON o.product_id = p.id;";
    let tree = parse_sql(sql);
    eprintln!("Multiple JOIN tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}

#[test]
fn debug_subquery_structure() {
    let sql = "SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);";
    let tree = parse_sql(sql);
    eprintln!("Subquery tree:");
    eprintln!("{}", tree.root_node().to_sexp());
}
