//! Integration tests for SQL language plugin (graph-native).
//!
//! Validates:
//! - Table node extraction (CREATE TABLE)
//! - View node extraction (CREATE VIEW)
//! - Function node extraction

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sql::SqlPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    name: &str,
    kind: NodeKind,
) -> Option<&'a NodeEntry> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return Some(entry);
            }
        }
    }
    None
}

fn build_graph(source: &[u8]) -> StagingGraph {
    let plugin = SqlPlugin::default();
    let file = PathBuf::from("test.sql");
    let tree = plugin.parse_ast(source).expect("parse failed");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
fn test_create_table() {
    let source = br"
CREATE TABLE users (
    id INT PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(100) UNIQUE
);

CREATE TABLE posts (
    id INT PRIMARY KEY,
    user_id INT,
    title VARCHAR(200),
    FOREIGN KEY (user_id) REFERENCES users(id)
);
";
    let staging = build_graph(source);

    assert!(
        find_node_entry(&staging, "users", NodeKind::Variable).is_some(),
        "users table not found"
    );
    assert!(
        find_node_entry(&staging, "posts", NodeKind::Variable).is_some(),
        "posts table not found"
    );
}

#[test]
fn test_create_view() {
    let source = br"
CREATE VIEW active_users AS
SELECT id, username, email
FROM users
WHERE last_login > CURRENT_DATE - INTERVAL '30 days';

CREATE VIEW user_posts AS
SELECT u.username, p.title, p.created_at
FROM users u
JOIN posts p ON u.id = p.user_id;
";
    let staging = build_graph(source);

    assert!(
        find_node_entry(&staging, "active_users", NodeKind::Variable).is_some(),
        "active_users view not found"
    );
    assert!(
        find_node_entry(&staging, "user_posts", NodeKind::Variable).is_some(),
        "user_posts view not found"
    );
}

#[test]
fn test_mixed_ddl_statements() {
    let source = br"
CREATE TABLE products (
    id INT PRIMARY KEY,
    name VARCHAR(100),
    price DECIMAL(10,2)
);

CREATE VIEW expensive_products AS
SELECT * FROM products WHERE price > 100;

CREATE TABLE orders (
    id INT PRIMARY KEY,
    product_id INT,
    quantity INT,
    FOREIGN KEY (product_id) REFERENCES products(id)
);

CREATE FUNCTION calculate_tax(amount DECIMAL)
RETURNS DECIMAL AS $$ BEGIN RETURN amount * 0.1; END; $$ LANGUAGE plpgsql;
";
    let staging = build_graph(source);

    assert!(
        find_node_entry(&staging, "products", NodeKind::Variable).is_some(),
        "products table not found"
    );
    assert!(
        find_node_entry(&staging, "expensive_products", NodeKind::Variable).is_some(),
        "expensive_products view not found"
    );
    assert!(
        find_node_entry(&staging, "orders", NodeKind::Variable).is_some(),
        "orders table not found"
    );
    assert!(
        find_node_entry(&staging, "calculate_tax", NodeKind::Function).is_some(),
        "calculate_tax function not found"
    );
}
