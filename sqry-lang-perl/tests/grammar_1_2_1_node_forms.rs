//! Node-form coverage for the tree-sitter-perl v1.2.1 grammar refresh
//! (issue #479).
//!
//! Each test drives the real Perl plugin end to end over a fixture that
//! exercises a modern-Perl construct introduced or expanded upstream:
//! `variable_group`, `refalias_variable`, `async_block_expression`,
//! `format_statement`, typed lexical declarations, Unicode package/identifier
//! handling, bare `eval`, the contextual `class`/`role`/`method` keywords, and
//! native `try`/`catch`. Every case asserts the grammar parses without an
//! error node AND that the graph builder captures the surrounding subroutine
//! symbols (and relevant call edges) instead of silently dropping them.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_perl::PerlPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = staging
                .resolve_node_canonical_name(entry)
                .map(str::to_owned)
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

/// Parse a fixture and build its staging graph. Panics if parsing or graph
/// construction fails, which is itself the primary regression guard for a
/// grammar refresh (a broken parser or scanner surfaces here).
fn build_staging_from_fixture(name: &str) -> StagingGraph {
    let plugin = PerlPlugin::default();
    let path = PathBuf::from(format!("tests/fixtures/{name}"));
    let content = std::fs::read(&path).expect("read fixture");
    let (prepared_content, tree) = plugin.prepare_ast(&content).expect("parse fixture");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, prepared_content.as_ref(), &path, &mut staging)
        .expect("build graph");
    staging
}

/// Assert the fixture parses with no `ERROR` node in the tree. A regressed
/// grammar (wrong ABI, missing scanner header, renamed rule) typically shows up
/// as error recovery rather than an outright parse failure, so this is checked
/// independently of graph construction.
fn assert_parses_clean(name: &str) {
    let plugin = PerlPlugin::default();
    let path = PathBuf::from(format!("tests/fixtures/{name}"));
    let content = std::fs::read(&path).expect("read fixture");
    let (_prepared, tree) = plugin.prepare_ast(&content).expect("parse fixture");
    assert!(
        !tree.root_node().has_error(),
        "fixture {name} parsed with an error node under the v1.2.1 grammar"
    );
}

fn has_function_suffix(staging: &StagingGraph, suffix: &str) -> bool {
    build_node_lookup(staging).values().any(|(name, kind)| {
        matches!(kind, NodeKind::Function) && (name.ends_with(suffix) || name == suffix)
    })
}

#[allow(clippy::similar_names)] // caller/callee test variables
fn has_call_edge(staging: &StagingGraph, caller: &str, callee: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            if !matches!(kind, EdgeKind::Calls { .. }) {
                continue;
            }
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(caller) && target_name == Some(callee) {
                return true;
            }
        }
    }
    false
}

fn has_module_node(staging: &StagingGraph, name: &str) -> bool {
    build_node_lookup(staging)
        .values()
        .any(|(node_name, kind)| matches!(kind, NodeKind::Module) && node_name == name)
}

#[test]
fn variable_group_declarations_preserve_subs() {
    assert_parses_clean("variable_group.pl");
    let staging = build_staging_from_fixture("variable_group.pl");
    assert!(has_function_suffix(&staging, "Grp::VarGroup::totals"));
    assert!(has_function_suffix(&staging, "Grp::VarGroup::reset_group"));
    // The call inside reset_group is still attributed to its enclosing sub.
    assert!(has_call_edge(
        &staging,
        "Grp::VarGroup::reset_group",
        "totals"
    ));
}

#[test]
fn refalias_variable_preserves_subs() {
    assert_parses_clean("refalias.pl");
    let staging = build_staging_from_fixture("refalias.pl");
    assert!(has_function_suffix(&staging, "Grp::RefAlias::make_alias"));
    assert!(has_function_suffix(
        &staging,
        "Grp::RefAlias::caller_of_alias"
    ));
    assert!(has_call_edge(
        &staging,
        "Grp::RefAlias::caller_of_alias",
        "make_alias"
    ));
}

#[test]
fn async_block_expression_captures_inner_calls() {
    assert_parses_clean("async_block.pl");
    let staging = build_staging_from_fixture("async_block.pl");
    assert!(has_function_suffix(
        &staging,
        "Grp::AsyncBlock::build_future"
    ));
    assert!(has_function_suffix(&staging, "Grp::AsyncBlock::compute"));
    // `compute()` is invoked inside `async { ... }`; the recursive call walk
    // still attributes it to the enclosing named subroutine.
    assert!(has_call_edge(
        &staging,
        "Grp::AsyncBlock::build_future",
        "compute"
    ));
}

#[test]
fn format_statement_preserves_subs() {
    assert_parses_clean("format_statement.pl");
    let staging = build_staging_from_fixture("format_statement.pl");
    assert!(has_function_suffix(
        &staging,
        "Grp::FormatStmt::emit_report"
    ));
    assert!(has_function_suffix(&staging, "Grp::FormatStmt::write_line"));
    assert!(has_call_edge(
        &staging,
        "Grp::FormatStmt::emit_report",
        "write_line"
    ));
}

#[test]
fn typed_lexical_declarations_preserve_subs() {
    assert_parses_clean("typed_lexical.pl");
    let staging = build_staging_from_fixture("typed_lexical.pl");
    assert!(has_function_suffix(&staging, "Grp::TypedLexical::tally"));
    assert!(has_function_suffix(
        &staging,
        "Grp::TypedLexical::accumulate"
    ));
    assert!(has_call_edge(
        &staging,
        "Grp::TypedLexical::tally",
        "accumulate"
    ));
}

#[test]
fn unicode_package_and_identifiers() {
    assert_parses_clean("unicode_ident.pl");
    let staging = build_staging_from_fixture("unicode_ident.pl");
    // Unicode package name and subroutine identifiers round-trip through utf8.
    assert!(has_function_suffix(&staging, "Café::Ωmega::función"));
    assert!(has_function_suffix(&staging, "Café::Ωmega::precißión"));
    assert!(has_module_node(&staging, "Café::Ωmega"));
}

#[test]
fn bare_eval_block_captures_inner_calls() {
    assert_parses_clean("bare_eval.pl");
    let staging = build_staging_from_fixture("bare_eval.pl");
    assert!(has_function_suffix(&staging, "Grp::BareEval::guarded"));
    assert!(has_function_suffix(&staging, "Grp::BareEval::risky_call"));
    assert!(has_function_suffix(&staging, "Grp::BareEval::fallback"));
    assert!(has_call_edge(
        &staging,
        "Grp::BareEval::guarded",
        "risky_call"
    ));
    assert!(has_call_edge(
        &staging,
        "Grp::BareEval::guarded",
        "fallback"
    ));
}

#[test]
fn contextual_class_role_method() {
    assert_parses_clean("contextual_oo.pl");
    let staging = build_staging_from_fixture("contextual_oo.pl");
    // Methods declared inside contextual `class`/`role` blocks are captured.
    assert!(has_function_suffix(&staging, "::coordinates"));
    assert!(has_function_suffix(&staging, "::draw"));
    assert!(has_function_suffix(&staging, "::summarize"));
    assert!(
        has_call_edge(
            &staging,
            // `summarize` is a top-level sub in the `main` package.
            "main::coordinates",
            "summarize"
        ) || has_call_edge(&staging, "Shape::Point::coordinates", "summarize")
    );
}

#[test]
fn native_try_catch_captures_inner_calls() {
    assert_parses_clean("try_catch.pl");
    let staging = build_staging_from_fixture("try_catch.pl");
    assert!(has_function_suffix(&staging, "Grp::TryCatch::attempt"));
    assert!(has_function_suffix(&staging, "Grp::TryCatch::risky"));
    assert!(has_function_suffix(&staging, "Grp::TryCatch::recover"));
    // Calls inside both the try and catch blocks attribute to the enclosing sub.
    assert!(has_call_edge(&staging, "Grp::TryCatch::attempt", "risky"));
    assert!(has_call_edge(&staging, "Grp::TryCatch::attempt", "recover"));
}
