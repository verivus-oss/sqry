//! DSL test for Ruby controller DSL edges.
//!
//! Verifies that `RubyGraphBuilder` correctly translates `before_action` DSL
//! declarations into `Calls` edges in the unified graph `StagingGraph` API.

use sqry_core::graph::{GraphBuilder, Language, unified::StagingGraph};
use sqry_lang_ruby::RubyGraphBuilder;
use sqry_test_support::graph_helpers::{
    assert_has_call_edge, assert_has_call_edge_for_language, collect_call_edges,
    collect_call_edges_for_language,
};
use std::path::Path;
use tree_sitter::Parser;

fn parse_ruby(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("error loading Ruby grammar");
    parser.parse(source, None).expect("ruby parse failed")
}

#[test]
fn test_controller_dsl_edges() {
    let source = include_str!("fixtures/graph/controller_dsl.rb");
    let tree = parse_ruby(source);

    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();
    let path = Path::new("test.rb");

    builder
        .build_graph(&tree, source.as_bytes(), path, &mut staging)
        .unwrap();

    // Verify the builder produced nodes and edges from the controller DSL
    assert!(
        staging.node_count() > 0,
        "Should have staged nodes from controller DSL"
    );
    assert!(
        staging.edge_count() > 0,
        "Should have staged edges from controller DSL"
    );

    // before_action :require_login, only: [:new, :create]
    // should generate Calls edges from the filtered actions to the callback
    assert_has_call_edge(
        &staging,
        "UsersController::new",
        "UsersController::require_login",
    );
    assert_has_call_edge(
        &staging,
        "UsersController::create",
        "UsersController::require_login",
    );
    assert_has_call_edge_for_language(
        &staging,
        Language::Ruby,
        "UsersController#new",
        "UsersController#require_login",
    );
    assert_has_call_edge_for_language(
        &staging,
        Language::Ruby,
        "UsersController#create",
        "UsersController#require_login",
    );

    // `show` is NOT in the `only:` list, so no edge to require_login
    let call_edges = collect_call_edges(&staging);
    let ruby_call_edges = collect_call_edges_for_language(&staging, Language::Ruby);
    assert!(
        !call_edges.iter().any(|edge| {
            edge.caller.contains("UsersController::show")
                && edge.callee.contains("UsersController::require_login")
        }),
        "Should not have edge from show to require_login"
    );
    assert!(
        !ruby_call_edges.iter().any(|edge| {
            edge.caller.contains("UsersController#show")
                && edge.callee.contains("UsersController#require_login")
        }),
        "Ruby display names should not have edge from show to require_login"
    );

    // The DSL helper `before_action` itself should not appear as a call target
    assert!(
        !call_edges
            .iter()
            .any(|edge| edge.callee.contains("before_action")),
        "Controller DSL should not emit call edges to before_action helper"
    );
    assert!(
        !ruby_call_edges
            .iter()
            .any(|edge| edge.callee.contains("before_action")),
        "Ruby display names should not emit call edges to before_action helper"
    );
}
