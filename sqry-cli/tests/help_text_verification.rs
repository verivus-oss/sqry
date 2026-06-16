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

#[test]
fn test_top_level_help_omits_ask_subcommand() {
    // The natural-language `sqry ask` command was removed completely.
    // `sqry --help` must no longer advertise an `ask` subcommand line.
    // (The top-level CLI has a default search-pattern positional, so
    // `sqry ask "..."` is now treated as an ordinary search for the
    // pattern `ask`, not a translation request and not an ONNX error.)
    let path = sqry_bin();
    Command::new(path)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  ask").not());
}

#[test]
fn test_ask_query_takes_no_nl_or_onnx_path() {
    // Running `sqry ask <query>` must never surface the removed
    // natural-language translation or ONNX Runtime error path; `ask` is
    // just a search pattern now.
    let path = sqry_bin();
    Command::new(path)
        .arg("ask")
        .arg("who calls authenticate")
        .assert()
        .stderr(predicate::str::contains("ONNX").not())
        .stderr(predicate::str::contains("natural language").not())
        .stderr(predicate::str::contains("translat").not());
}
