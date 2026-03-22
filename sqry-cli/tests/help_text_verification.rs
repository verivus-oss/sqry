mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_search_help_mentions_query() {
    let path = sqry_bin();
    Command::new(path)
        .arg("search")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("use 'query' instead"));
}

#[test]
fn test_query_help_mentions_search() {
    let path = sqry_bin();
    Command::new(path)
        .arg("query")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("use 'search' instead"));
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
