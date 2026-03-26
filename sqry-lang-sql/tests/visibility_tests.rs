//! Visibility tests for SQL language plugin
//!
//! SQL visibility is determined by schema context:
//! - public schema (or no schema) → public
//! - private/internal/pg_* schemas → private

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, node::NodeKind},
};
use sqry_lang_sql::SqlGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_sql(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("Failed to set SQL language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse SQL code")
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
fn test_function_public_schema() {
    // Functions in public schema are public
    let source = r"
        CREATE FUNCTION public.get_user(user_id INT)
        RETURNS TEXT AS $$
        BEGIN
            RETURN 'user';
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "get_user");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Function in public schema should be public"
    );
}

#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
#[test]
fn test_function_no_schema() {
    // Functions without schema prefix default to public
    let source = r"
        CREATE FUNCTION calculate_total(amount INT)
        RETURNS INT AS $$
        BEGIN
            RETURN amount * 2;
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "calculate_total");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Function without schema should default to public"
    );
}

#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
#[test]
fn test_function_private_schema() {
    // Functions in private schema are private
    let source = r"
        CREATE FUNCTION private.internal_helper(value INT)
        RETURNS INT AS $$
        BEGIN
            RETURN value + 1;
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "internal_helper");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Function in private schema should be private"
    );
}

#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
#[test]
fn test_function_internal_schema() {
    // Functions in internal schema are private
    let source = r"
        CREATE FUNCTION internal.secret_logic(x INT)
        RETURNS INT AS $$
        BEGIN
            RETURN x * x;
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "secret_logic");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Function in internal schema should be private"
    );
}

#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
#[test]
fn test_function_pg_system_schema() {
    // Functions in pg_* schemas (system schemas) are private
    let source = r"
        CREATE FUNCTION pg_catalog.custom_func(n INT)
        RETURNS INT AS $$
        BEGIN
            RETURN n;
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "custom_func");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Function in pg_* schema should be private"
    );
}

#[ignore = "SQL visibility metadata not yet implemented - P1 baseline feature pending"]
#[test]
fn test_mixed_visibility() {
    // Test multiple functions with different visibilities
    let source = r"
        CREATE FUNCTION public.api_function(x INT)
        RETURNS INT AS $$
        BEGIN
            RETURN x;
        END;
        $$ LANGUAGE plpgsql;

        CREATE FUNCTION private.helper_function(y INT)
        RETURNS INT AS $$
        BEGIN
            RETURN y * 2;
        END;
        $$ LANGUAGE plpgsql;
    ";

    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let api_visibility = find_function_visibility(&staging, "api_function");
    assert_eq!(
        api_visibility,
        Some("public".to_string()),
        "Public schema function should be public"
    );

    let helper_visibility = find_function_visibility(&staging, "helper_function");
    assert_eq!(
        helper_visibility,
        Some("private".to_string()),
        "Private schema function should be private"
    );
}
