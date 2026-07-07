//! DB18 golden tests: CLI graph subcommand migration to sqry-db.
//!
//! These tests lock the post-DB18 contract for the six CLI handlers
//! migrated in this unit:
//!
//! * `graph direct-callers` (`callers`) — name-keyed predicate,
//!   sqry-db `mcp_callers_query`
//! * `graph direct-callees` (`callees`) — name-keyed predicate,
//!   sqry-db `mcp_callees_query`
//! * `graph call-chain-depth` — NodeId-anchored BFS, frontier
//!   invariant (no same-name broadening)
//! * `graph dependency-tree` (`deps`) — NodeId-anchored BFS, frontier
//!   invariant
//! * `impact` — NodeId-anchored incoming BFS, frontier invariant
//! * `unused` — name-keyed predicate, sqry-db `UnusedQuery` with
//!   post-filter completeness
//!
//! The direction of callers/callees is the `graph_eval` convention
//! (MCP-compatible), locked at the CLI via sqry-db's
//! `dispatch::mcp_callers_query` / `mcp_callees_query` inversion
//! wrappers. See `sqry_db::queries::dispatch` module docs.
//!
//! The same-name frontier invariant tests use the DB16/DB17 followup
//! pattern (`AlphaMarker::helper` + `BetaMarker::helper` in disjoint
//! inherent impls). For **NodeId-anchored** handlers (`impact`,
//! `dependency-tree`, `call-chain-depth`), a regression that
//! re-introduced the DB15-class frontier-broadening bug would surface
//! here. For **name-keyed** handlers (`direct-callers`,
//! `direct-callees`), DB18 intentionally adopts sqry-db's segment-aware
//! union semantic (matching MCP since DB15), and the tests lock that
//! new canonical contract rather than asserting the pre-DB18
//! NodeId-walking behavior.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Shared fixture: two same-simple-name `helper()` methods on disjoint
/// inherent impls so the Rust plugin emits distinct qualified names
/// `AlphaMarker::helper` / `BetaMarker::helper`. This is the
/// minimal reproducer for the DB15-class same-simple-name frontier
/// broadening bug.
fn write_same_name_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db18_cli_same_name_frontier"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r"pub struct AlphaMarker;
impl AlphaMarker {
    pub fn helper() {}
}
pub fn caller_a() {
    AlphaMarker::helper();
}
pub fn root_a() {
    caller_a();
}

pub struct BetaMarker;
impl BetaMarker {
    pub fn helper() {}
}
pub fn caller_b() {
    BetaMarker::helper();
}
pub fn root_b() {
    caller_b();
}
",
    )
    .unwrap();
}

/// Simple chain: `fetch` and `process` both call `helper`.
fn write_simple_callers_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub fn helper() -> i32 {
    42
}

pub fn fetch() -> i32 {
    helper()
}

pub fn process() -> i32 {
    helper()
}
",
    )
    .unwrap();
}

/// Index a workspace at `root`.
fn index(root: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

// ============================================================================
// direct-callers
// ============================================================================

#[test]
fn cli_direct_callers_graph_eval_direction() {
    // `callers` under the graph_eval convention: `callers('helper')`
    // returns nodes that CALL helper, not the target itself. Lock the
    // direction through sqry-db's mcp_callers_query inversion.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callers")
        .arg("helper")
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_direct_callers_segment_aware_union_semantic() {
    // DB18 behavior shift lock: sqry-db's CallersQuery is name-keyed
    // and *segment-aware* (matches trailing method segment for Calls
    // edges — see sqry_db::queries::relation::method_segment_matches).
    // On a fixture with two disjoint inherent impls sharing a simple
    // method name (`AlphaMarker::helper`, `BetaMarker::helper`), a
    // query for either qualified name returns the UNION of callers —
    // i.e. both `caller_a` and `caller_b`. This matches MCP's DB15
    // behavior exactly, so CLI and MCP share one cache behavior.
    // Regressions that flipped the inversion (e.g. dispatching to
    // CalleesQuery for `direct-callers`) would return callees
    // instead of callers and fail this test.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callers")
        .arg("AlphaMarker::helper")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("caller_a"),
        "caller_a must be in direct-callers(AlphaMarker::helper) — \
         it is a direct caller of the alpha-side helper, stdout = {stdout}"
    );
    assert!(
        stdout.contains("caller_b"),
        "caller_b must ALSO be in direct-callers(AlphaMarker::helper) \
         — sqry-db's method-segment fallback unions callers across \
         nodes with the same simple method name (this is the DB18 \
         behavior shift aligning CLI with MCP), stdout = {stdout}"
    );
    // The callers must NOT include the helper targets themselves
    // (they are called; they do not call helper).
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let callers = parsed["callers"].as_array().unwrap();
    for caller in callers {
        let name = caller["name"].as_str().unwrap_or("");
        assert!(
            name != "helper",
            "helper must not appear as its own caller, got caller = {caller:?}"
        );
    }
}

#[test]
fn cli_direct_callers_json_schema_stable() {
    // Lock the JSON schema: {symbol, callers: [{name, qualified_name,
    // kind, file, line, language}], total, truncated}. This is the
    // pre-DB18 shape; DB18 must not change it.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callers")
        .arg("helper")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert!(parsed.get("symbol").is_some(), "missing 'symbol'");
    assert!(parsed.get("callers").is_some(), "missing 'callers'");
    assert!(parsed.get("total").is_some(), "missing 'total'");
    assert!(parsed.get("truncated").is_some(), "missing 'truncated'");
    let callers = parsed["callers"].as_array().unwrap();
    assert!(!callers.is_empty(), "expected at least one caller");
    for caller in callers {
        for field in ["name", "qualified_name", "kind", "file", "line", "language"] {
            assert!(
                caller.get(field).is_some(),
                "caller row missing field '{field}': {caller:?}"
            );
        }
    }
}

// ============================================================================
// direct-callees
// ============================================================================

#[test]
fn cli_direct_callees_graph_eval_direction() {
    // `callees` under the graph_eval convention: `callees('fetch')`
    // returns nodes that fetch CALLS (i.e. helper). Lock direction.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callees")
        .arg("fetch")
        .assert()
        .success()
        .stdout(predicate::str::contains("helper"));
}

#[test]
fn cli_direct_callees_from_unique_caller_stays_anchored() {
    // `caller_a` is a uniquely-named function that calls only
    // `AlphaMarker::helper`. direct-callees(caller_a) must include
    // `helper` (alpha-side) and must NOT include `caller_b` (which
    // is reached only via a different caller). This locks the
    // direction (callees, not callers) — a regression that flipped
    // the inversion would return callers of caller_a (nothing) or
    // leak caller_a's own identity back into the output.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callees")
        .arg("caller_a")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("helper"),
        "direct-callees(caller_a) must include helper (the alpha-side \
         inherent method), stdout = {stdout}"
    );
    assert!(
        !stdout.contains("caller_b"),
        "caller_b must NOT appear in direct-callees(caller_a) — \
         caller_b is not called by caller_a and a regression that \
         broadened through a shared name would leak it in, stdout = {stdout}"
    );
}

#[test]
fn cli_direct_callees_json_schema_stable() {
    // Lock the JSON schema: {symbol, callees: [{name, qualified_name,
    // kind, file, line, language}], total, truncated}. The `direct-callees`
    // surface shares the `emit_direct_call_output` helper with
    // `direct-callers` and must maintain the same envelope shape with the
    // "callees" key (not "callers").
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callees")
        .arg("fetch")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    // Top-level envelope
    assert!(parsed.get("symbol").is_some(), "missing 'symbol'");
    assert!(parsed.get("callees").is_some(), "missing 'callees'");
    assert!(parsed.get("total").is_some(), "missing 'total'");
    assert!(parsed.get("truncated").is_some(), "missing 'truncated'");
    assert!(
        parsed["total"].is_number(),
        "'total' must be a number, got {:?}",
        parsed["total"]
    );
    assert!(
        parsed["truncated"].is_boolean(),
        "'truncated' must be a boolean, got {:?}",
        parsed["truncated"]
    );
    // Row schema
    let callees = parsed["callees"].as_array().unwrap();
    assert!(!callees.is_empty(), "expected at least one callee");
    for callee in callees {
        for field in ["name", "qualified_name", "kind", "file", "language"] {
            assert!(
                callee.get(field).is_some(),
                "callee row missing string field '{field}': {callee:?}"
            );
            assert!(
                callee[field].is_string(),
                "callee field '{field}' must be a string: {callee:?}"
            );
        }
        assert!(
            callee.get("line").is_some(),
            "callee row missing 'line': {callee:?}"
        );
        assert!(
            callee["line"].is_number(),
            "callee 'line' must be a number: {callee:?}"
        );
    }
}

// ============================================================================
// call-chain-depth
// ============================================================================

#[test]
fn cli_call_chain_depth_same_name_frontier_invariant() {
    // caller_a → AlphaMarker::helper (depth 1). caller_b's chain
    // through BetaMarker::helper is disjoint. If call-chain-depth
    // re-resolved "helper" at depth 1, it would broaden through
    // BetaMarker::helper and pull in beta-side chains — frontier
    // invariant locks that out.
    //
    // Positive witness: `caller_a` must appear in the results (it is
    // the seeded symbol). Negative witness: `BetaMarker` / `caller_b`
    // / `root_b` must NOT appear (beta-side is disjoint from caller_a).
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("call-chain-depth")
        .arg("caller_a")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    // Positive: caller_a (the seeded symbol) must appear in results.
    let results = parsed["results"]
        .as_array()
        .expect("top-level 'results' must be an array");
    assert!(
        !results.is_empty(),
        "call-chain-depth(caller_a) must return at least one result, stdout = {stdout}"
    );
    let has_caller_a = results
        .iter()
        .any(|r| r["symbol"].as_str().is_some_and(|s| s.contains("caller_a")));
    assert!(
        has_caller_a,
        "caller_a must appear in call-chain-depth results as the seeded symbol, stdout = {stdout}"
    );

    // Negative: beta-side symbols must not leak in via frontier broadening.
    assert!(
        !stdout.contains("BetaMarker"),
        "BetaMarker must NOT appear in call-chain-depth(caller_a) — \
         DB18 frontier regression freeze, stdout = {stdout}"
    );
    assert!(
        !stdout.contains("caller_b"),
        "caller_b must NOT appear in call-chain-depth(caller_a) — \
         it belongs to the disjoint beta-side chain, stdout = {stdout}"
    );
    assert!(
        !stdout.contains("root_b"),
        "root_b must NOT appear in call-chain-depth(caller_a), stdout = {stdout}"
    );
}

#[test]
fn cli_call_chain_depth_json_schema_stable() {
    // Lock the JSON schema: {results: [{symbol, language, depth}], count}.
    // "file" is present only in verbose mode; do not assert it in the
    // default (non-verbose) output. "chains" is present only when
    // --show-chain is passed; lock the base shape here.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("call-chain-depth")
        .arg("helper")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    // Top-level envelope
    assert!(parsed.get("results").is_some(), "missing 'results'");
    assert!(parsed.get("count").is_some(), "missing 'count'");
    assert!(
        parsed["count"].is_number(),
        "'count' must be a number, got {:?}",
        parsed["count"]
    );
    let results = parsed["results"]
        .as_array()
        .expect("'results' must be an array");
    assert!(!results.is_empty(), "expected at least one result entry");
    // Per-item schema
    for item in results {
        assert!(
            item.get("symbol").is_some() && item["symbol"].is_string(),
            "result item missing string 'symbol': {item:?}"
        );
        assert!(
            item.get("language").is_some() && item["language"].is_string(),
            "result item missing string 'language': {item:?}"
        );
        assert!(
            item.get("depth").is_some() && item["depth"].is_number(),
            "result item missing number 'depth': {item:?}"
        );
    }
}

// ============================================================================
// dependency-tree
// ============================================================================

#[test]
fn cli_dependency_tree_same_name_frontier_invariant() {
    // dependency-tree caller_a must surface AlphaMarker::helper in
    // its outgoing-dependency walk without pulling in beta-side
    // nodes. This freezes the frontier invariant for `deps`.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("dependency-tree")
        .arg("caller_a")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Positive witness: at least one of the alpha-side symbols must appear
    // in the dependency tree. caller_a calls AlphaMarker::helper, so either
    // the callee (helper) or caller_a itself must be in the node list.
    let has_alpha_side = stdout.contains("caller_a") || stdout.contains("helper");
    assert!(
        has_alpha_side,
        "dependency-tree(caller_a) must include at least one alpha-side symbol \
         (caller_a or helper); an empty or broken traversal would fail this, \
         stdout = {stdout}"
    );

    // Negative witness: beta-side symbols must NOT appear in caller_a's
    // dependency tree.
    assert!(
        !stdout.contains("BetaMarker"),
        "BetaMarker symbols must NOT leak into dependency-tree(caller_a), stdout = {stdout}"
    );
    assert!(
        !stdout.contains("caller_b"),
        "caller_b must NOT leak into dependency-tree(caller_a), stdout = {stdout}"
    );
}

#[test]
fn cli_dependency_tree_json_schema_stable() {
    // Lock the JSON schema: {nodes: [{id, name, language}],
    // edges: [{from, to, kind}], node_count, edge_count}.
    // "file" / "line" appear only in verbose mode; lock the base shape.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("dependency-tree")
        .arg("fetch")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    // Top-level envelope
    assert!(
        parsed.get("nodes").is_some() && parsed["nodes"].is_array(),
        "missing array 'nodes'"
    );
    assert!(
        parsed.get("edges").is_some() && parsed["edges"].is_array(),
        "missing array 'edges'"
    );
    assert!(
        parsed.get("node_count").is_some() && parsed["node_count"].is_number(),
        "missing number 'node_count'"
    );
    assert!(
        parsed.get("edge_count").is_some() && parsed["edge_count"].is_number(),
        "missing number 'edge_count'"
    );
    // Node schema: spot-check first node
    let nodes = parsed["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty(), "expected at least one node");
    let first_node = &nodes[0];
    for field in ["id", "name", "language"] {
        assert!(
            first_node.get(field).is_some() && first_node[field].is_string(),
            "node missing string field '{field}': {first_node:?}"
        );
    }
    // Edge schema: spot-check first edge if any edges exist
    let edges = parsed["edges"].as_array().unwrap();
    if !edges.is_empty() {
        let first_edge = &edges[0];
        for field in ["from", "to", "kind"] {
            assert!(
                first_edge.get(field).is_some() && first_edge[field].is_string(),
                "edge missing string field '{field}': {first_edge:?}"
            );
        }
    }
}

// ============================================================================
// impact
// ============================================================================

#[test]
fn cli_impact_same_name_frontier_invariant() {
    // Impact is an incoming BFS (reverse dependents). impact on
    // AlphaMarker::helper must surface caller_a (direct dependent)
    // and root_a (transitive), but NOT caller_b / root_b — those
    // depend on BetaMarker::helper, which shares only the simple
    // name "helper".
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("impact")
        .arg("AlphaMarker::helper")
        .arg("--path")
        .arg(temp.path())
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("caller_a"),
        "impact(AlphaMarker::helper) must include caller_a, stdout = {stdout}"
    );
    assert!(
        !stdout.contains("caller_b"),
        "caller_b must NOT leak into impact(AlphaMarker::helper) — \
         DB18 frontier regression freeze, stdout = {stdout}"
    );
}

#[test]
fn cli_impact_json_schema_stable() {
    // Lock the JSON schema for `sqry impact --json`:
    // {symbol: str, direct: [{name, qualified_name, kind, file, line,
    // relation, depth}], stats: {direct_count, indirect_count,
    // total_affected, affected_files_count, max_depth}}.
    // `indirect` and `affected_files` are omitted when empty
    // (skip_serializing_if). Lock the always-present fields.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("impact")
        .arg("AlphaMarker::helper")
        .arg("--path")
        .arg(temp.path())
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    // Top-level envelope
    assert!(
        parsed.get("symbol").is_some() && parsed["symbol"].is_string(),
        "missing string 'symbol'"
    );
    assert!(
        parsed.get("direct").is_some() && parsed["direct"].is_array(),
        "missing array 'direct'"
    );
    assert!(parsed.get("stats").is_some(), "missing object 'stats'");
    // Stats sub-object
    let stats = &parsed["stats"];
    for field in [
        "direct_count",
        "indirect_count",
        "total_affected",
        "affected_files_count",
        "max_depth",
    ] {
        assert!(
            stats.get(field).is_some() && stats[field].is_number(),
            "stats missing number field '{field}': {stats:?}"
        );
    }
    // Direct-dependent row schema: spot-check first entry
    let direct = parsed["direct"].as_array().unwrap();
    assert!(!direct.is_empty(), "expected at least one direct dependent");
    let first = &direct[0];
    for field in ["name", "qualified_name", "kind", "file", "relation"] {
        assert!(
            first.get(field).is_some() && first[field].is_string(),
            "direct item missing string field '{field}': {first:?}"
        );
    }
    assert!(
        first.get("line").is_some() && first["line"].is_number(),
        "direct item missing number 'line': {first:?}"
    );
    assert!(
        first.get("depth").is_some() && first["depth"].is_number(),
        "direct item missing number 'depth': {first:?}"
    );
}

// ============================================================================
// unused
// ============================================================================

/// Fixture with one public entry + one orphan dead function (private)
/// so `unused --scope private` returns the orphan without false
/// positives on the entry.
fn write_unused_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db18_cli_unused"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn entry_point() -> i32 {
    41
}

fn definitely_unused_orphan() -> i32 {
    7
}
",
    )
    .unwrap();
}

#[test]
fn cli_unused_routes_through_sqry_db() {
    // Lock the sqry-db dispatch path end-to-end: `unused --scope all`
    // must find the orphan function via `UnusedQuery`. The exact name
    // must appear in the output. This is the minimal positive
    // existence test.
    let temp = TempDir::new().unwrap();
    write_unused_fixture(temp.path());
    index(temp.path());

    Command::new(sqry_bin())
        .arg("unused")
        .arg("--scope")
        .arg("all")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("definitely_unused_orphan"));
}

#[test]
fn cli_unused_scope_values_unchanged() {
    // Lock the CLI --scope arg values: public|private|function|struct|all.
    // The DB18 migration preserves the CLI-facing string values exactly
    // (sqry-db's UnusedScope enum is the same sqry-core enum CLI already
    // used via `UnusedScope::try_parse`).
    let temp = TempDir::new().unwrap();
    write_unused_fixture(temp.path());
    index(temp.path());

    for scope in &["public", "private", "function", "struct", "all"] {
        Command::new(sqry_bin())
            .arg("unused")
            .arg("--scope")
            .arg(scope)
            .arg(temp.path())
            .assert()
            .success();
    }
}

#[test]
fn cli_unused_post_filter_completeness_with_lang_filter() {
    // Lock the MCP-style post-filter superset path: when --lang is
    // supplied, sqry-db is asked for the full candidate pool so
    // candidates that the post-filter rejects cannot push valid later
    // matches out of the window (Codex DB16 follow-up finding class).
    //
    // Fixture: one Rust orphan function. `unused --lang rust` must
    // still find it (the post-filter uses substring-match on the
    // sqry-db-provided language label).
    let temp = TempDir::new().unwrap();
    write_unused_fixture(temp.path());
    index(temp.path());

    Command::new(sqry_bin())
        .arg("unused")
        .arg("--scope")
        .arg("all")
        .arg("--lang")
        .arg("rust")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("definitely_unused_orphan"));
}

#[test]
fn cli_unused_json_schema_stable() {
    // Lock the JSON schema for `sqry --json unused`:
    // array of {file: str, count: number, symbols: [{name, qualified_name,
    // kind, file, line, language, visibility}]}.
    let temp = TempDir::new().unwrap();
    write_unused_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("unused")
        .arg("--scope")
        .arg("all")
        .arg(temp.path())
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    // Top level must be an array of file groups
    let groups = parsed
        .as_array()
        .expect("output must be a JSON array of file groups");
    assert!(
        !groups.is_empty(),
        "expected at least one file group (the orphan function must be present)"
    );
    // Per-group schema
    let first_group = &groups[0];
    assert!(
        first_group.get("file").is_some() && first_group["file"].is_string(),
        "group missing string 'file': {first_group:?}"
    );
    assert!(
        first_group.get("count").is_some() && first_group["count"].is_number(),
        "group missing number 'count': {first_group:?}"
    );
    assert!(
        first_group.get("symbols").is_some() && first_group["symbols"].is_array(),
        "group missing array 'symbols': {first_group:?}"
    );
    // Per-symbol schema
    let symbols = first_group["symbols"].as_array().unwrap();
    assert!(!symbols.is_empty(), "group must have at least one symbol");
    let first_sym = &symbols[0];
    for field in [
        "name",
        "qualified_name",
        "kind",
        "file",
        "language",
        "visibility",
    ] {
        assert!(
            first_sym.get(field).is_some() && first_sym[field].is_string(),
            "symbol missing string field '{field}': {first_sym:?}"
        );
    }
    assert!(
        first_sym.get("line").is_some() && first_sym["line"].is_number(),
        "symbol missing number 'line': {first_sym:?}"
    );
}

// ============================================================================
// DB19: cycles / is-in-cycle / subgraph / visualize migrations
// ============================================================================

/// 3-node SCC fixture: `a` calls `b`, `b` calls `c`, `c` calls `a`.
/// Lock the `cycles` CLI's ability to detect a 3-cycle via sqry-db's
/// `CyclesQuery`.
fn write_three_cycle_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db19_cli_three_cycle"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn node_alpha() {
    node_beta();
}
pub fn node_beta() {
    node_gamma();
}
pub fn node_gamma() {
    node_alpha();
}
",
    )
    .unwrap();
}

#[test]
fn cli_cycles_detects_three_node_scc() {
    // Lock the sqry-db dispatch path: `cycles --cycle-type calls` must
    // find the {node_alpha, node_beta, node_gamma} SCC and report it.
    let temp = TempDir::new().unwrap();
    write_three_cycle_fixture(temp.path());
    index(temp.path());

    Command::new(sqry_bin())
        .arg("cycles")
        .arg("--type")
        .arg("calls")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("node_alpha"))
        .stdout(predicate::str::contains("node_beta"))
        .stdout(predicate::str::contains("node_gamma"));
}

#[test]
fn cli_cycles_respects_min_depth() {
    // `--min-depth 4` on a 3-node SCC must return zero cycles. Locks
    // the `CycleBounds::min_depth` plumbing from CLI → sqry-db.
    let temp = TempDir::new().unwrap();
    write_three_cycle_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("cycles")
        .arg("--type")
        .arg("calls")
        .arg("--min-depth")
        .arg("4")
        .arg(temp.path())
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    let cycles = parsed.as_array().expect("output must be an array");
    assert!(
        cycles.is_empty(),
        "min-depth=4 on a 3-node SCC must return no cycles, got {cycles:?}"
    );
}

#[test]
fn cli_cycles_json_schema_stable() {
    // Lock the JSON schema: array of {depth, nodes: [name]}.
    let temp = TempDir::new().unwrap();
    write_three_cycle_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("cycles")
        .arg("--type")
        .arg("calls")
        .arg(temp.path())
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    let cycles = parsed.as_array().expect("output must be array");
    assert!(!cycles.is_empty(), "expected at least one cycle");
    for cycle in cycles {
        assert!(cycle.get("depth").is_some(), "missing 'depth'");
        assert!(cycle.get("nodes").is_some(), "missing 'nodes'");
        assert!(cycle["nodes"].as_array().is_some(), "'nodes' must be array");
    }
}

#[test]
fn cli_is_in_cycle_true_case() {
    // Lock sqry-db dispatch via IsInCycleQuery. `node_alpha` is in the
    // 3-cycle; response must report in_cycle=true.
    let temp = TempDir::new().unwrap();
    write_three_cycle_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("is-in-cycle")
        .arg("node_alpha")
        .arg("--cycle-type")
        .arg("calls")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert_eq!(
        parsed["in_cycle"].as_bool(),
        Some(true),
        "node_alpha must be in_cycle=true, got {parsed:?}"
    );
}

#[test]
fn cli_is_in_cycle_false_case() {
    // A uniquely-named non-cyclic function must return in_cycle=false.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("is-in-cycle")
        .arg("helper")
        .arg("--cycle-type")
        .arg("calls")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert_eq!(
        parsed["in_cycle"].as_bool(),
        Some(false),
        "helper must be in_cycle=false, got {parsed:?}"
    );
}

#[test]
fn cli_is_in_cycle_ambiguous_name_fails_strict() {
    // DB19 strict-resolution policy: a simple name that resolves to
    // multiple candidates (via `AlphaMarker::helper` /
    // `BetaMarker::helper`) must fail with a non-zero exit code and
    // report ambiguity in stderr. This locks the ResolutionMode::Strict
    // path in `sqry-cli/src/commands/graph.rs` — any regression that
    // silently picks one candidate (or returns a merged answer) would
    // exit 0, causing this test to fail.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("is-in-cycle")
        .arg("helper")
        .arg("--cycle-type")
        .arg("calls")
        .output()
        .expect("command failed");
    // The strict resolver must fail with a non-zero exit code…
    assert!(
        !output.status.success(),
        "is-in-cycle(helper) must exit non-zero when name is ambiguous \
         (strict resolution policy). status = {:?}, stdout = {}",
        output.status,
        String::from_utf8_lossy(&output.stdout)
    );
    // …and the error message must indicate ambiguity so the caller knows
    // to supply a fully-qualified name.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("ambiguous") || combined.contains("candidates"),
        "is-in-cycle(helper) error output must mention 'ambiguous' or \
         'candidates'; combined = {combined}"
    );
}

#[test]
fn cli_subgraph_same_name_frontier_invariant() {
    // Seed is `AlphaMarker::helper`. The subgraph walk must not
    // broaden through `BetaMarker::helper` at depth ≥ 1. Locks the
    // DB19 frontier invariant for `sqry subgraph` (NodeId-keyed
    // traverse kernel).
    //
    // Positive witness: `caller_a` (the only caller of AlphaMarker::helper)
    // must appear in the subgraph output. An empty or badly-regressed
    // output that simply omits caller_b vacuously would fail this.
    // Negative witness: `caller_b` must NOT appear (it only calls
    // BetaMarker::helper; a frontier-broadening regression would pull it
    // in via the shared simple name "helper").
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("subgraph")
        .arg("--path")
        .arg(temp.path())
        .arg("AlphaMarker::helper")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Positive: the alpha-side caller must be present in the traversal.
    assert!(
        stdout.contains("caller_a") || stdout.contains("AlphaMarker"),
        "subgraph(AlphaMarker::helper) must include an alpha-side symbol \
         (caller_a or AlphaMarker); got empty/regressed output: {stdout}"
    );
    // Negative: caller_b depends only on BetaMarker::helper and must
    // not leak in through the shared simple name.
    assert!(
        !stdout.contains("caller_b"),
        "caller_b must NOT leak into subgraph(AlphaMarker::helper), stdout = {stdout}"
    );
}

#[test]
fn cli_visualize_same_name_matches_graph_direct_callers() {
    // Seed is `AlphaMarker::helper` via the `callers:` relation.
    //
    // `visualize` now resolves relation roots with the same segment-aware
    // matching `sqry graph direct-callers` and `sqry query callers:` use
    // (DB18), so a `Type::method` query unions same-method-segment candidates
    // (`AlphaMarker::helper` and `BetaMarker::helper`). This is the
    // verivus-oss/sqry#516 parity fix: before it, `visualize` truncated to
    // a single arbitrary root and diverged from `graph` / `query`.
    //
    // The kernel frontier invariant (DB19: no name re-resolution at depth >=
    // 1) is unchanged; `caller_b` appears because `BetaMarker::helper` is a
    // legitimate segment-aware root seed, not because the kernel broadened.
    let temp = TempDir::new().unwrap();
    write_same_name_fixture(temp.path());
    index(temp.path());

    // Baseline: what `graph direct-callers` resolves for the same name. The
    // graph subcommand discovers the index from the working directory.
    let graph_out = Command::new(sqry_bin())
        .current_dir(temp.path())
        .arg("graph")
        .arg("direct-callers")
        .arg("AlphaMarker::helper")
        .output()
        .expect("graph command failed");
    assert!(graph_out.status.success(), "graph failed: {graph_out:?}");
    let graph_rendered = String::from_utf8_lossy(&graph_out.stdout);
    // Confirm the baseline unions both callers (DB18 segment-aware behavior),
    // so the parity assertion below is not vacuous.
    assert!(
        graph_rendered.contains("caller_a") && graph_rendered.contains("caller_b"),
        "graph direct-callers baseline must union both callers, got {graph_rendered}"
    );

    let out_file = temp.path().join("visualize.dot");
    let output = Command::new(sqry_bin())
        .arg("visualize")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("graphviz")
        .arg("--output-file")
        .arg(&out_file)
        .arg("callers:AlphaMarker::helper")
        .output()
        .expect("command failed");
    assert!(output.status.success(), "command failed: {output:?}");
    let rendered = fs::read_to_string(&out_file).unwrap_or_default();
    // Positive: the rendered diagram must contain an alpha-side symbol.
    assert!(
        rendered.contains("caller_a") || rendered.contains("AlphaMarker"),
        "visualize(callers:AlphaMarker::helper) must include an alpha-side \
         symbol (caller_a or AlphaMarker) in the diagram; got empty/regressed \
         output: {rendered}"
    );
    // Parity: visualize must resolve the same segment-aware root set as
    // `graph direct-callers`, so `caller_b` (a caller of the sibling
    // `BetaMarker::helper`) appears in both (verivus-oss/sqry#516).
    assert!(
        rendered.contains("caller_b"),
        "visualize must match graph direct-callers for a qualified name; \
         caller_b missing from {rendered}"
    );
}

#[test]
fn cli_visualize_format_switches_work() {
    // Smoke-test that the --format switch produces a non-empty
    // rendering for graphviz, mermaid, and d2. This is a shallow
    // regression gate for the kernel → diagram path post-DB19.
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    for format in &["graphviz", "mermaid", "d2"] {
        let out_file = temp.path().join(format!("visualize.{format}"));
        Command::new(sqry_bin())
            .arg("visualize")
            .arg("--path")
            .arg(temp.path())
            .arg("--format")
            .arg(format)
            .arg("--output-file")
            .arg(&out_file)
            .arg("callers:helper")
            .assert()
            .success();
        let rendered = fs::read_to_string(&out_file).unwrap_or_default();
        assert!(
            !rendered.is_empty(),
            "visualize --format {format} produced empty output"
        );
    }
}
