mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_search_help_mentions_query() {
    // Cluster-F (IMP-F §3.1) rewrote the cross-reference: `sqry search`
    // help now points users at `sqry query` for the structural-planner
    // surface. The substring `sqry query` is the new contract.
    let path = sqry_bin();
    Command::new(path)
        .arg("search")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sqry query"));
}

#[test]
fn test_query_help_mentions_search() {
    // Cluster-F (IMP-F §3.2) rewrote the cross-reference: `sqry query`
    // help now points users at `sqry search` for the regex / literal
    // pattern surface.
    let path = sqry_bin();
    Command::new(path)
        .arg("query")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sqry search"));
}

#[test]
fn test_graph_help_explains_noun_pattern() {
    let path = sqry_bin();
    Command::new(path)
        .arg("graph")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("noun-based"));
}
