//! CLI coverage for graph nodes/edges listing with Rust-specific edges.

mod common;

use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    Command::new(sqry_bin())
}

fn write_file<P: AsRef<Path>>(root: &Path, rel: P, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    let mut f = fs::File::create(&path).expect("create file");
    f.write_all(contents.as_bytes()).expect("write file");
}

fn build_rust_fixture() -> TempDir {
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "Cargo.toml",
        r#"[package]
name = "graph_nodes_edges_fixture"
version = "0.1.0"
edition = "2024"
"#,
    );
    write_file(
        project.path(),
        "src/lib.rs",
        r"
#[derive(Debug, Clone)]
pub struct RefHolder<'a> {
    data: &'a str,
}

pub fn example<'a, 'b>(x: &'a str, y: &'b str) -> &'a str
where
    'b: 'a,
{
    let _ = y;
    x
}
",
    );
    project
}

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("valid JSON output")
}

fn assert_object_keys(value: &Value, keys: &[&str]) {
    let obj = value.as_object().expect("expected JSON object");
    for key in keys {
        assert!(obj.contains_key(*key), "missing key: {key}");
    }
}

#[test]
fn graph_nodes_edges_rust_lifetime_and_macro() {
    let project = build_rust_fixture();

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let nodes_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("nodes")
        .arg("--kind")
        .arg("LiFeTiMe")
        .arg("--file")
        .arg("SRC/LIB.RS")
        .output()
        .expect("run sqry graph nodes");
    assert!(nodes_output.status.success(), "graph nodes failed");
    let nodes_json = parse_json(&nodes_output.stdout);
    let nodes_count = nodes_json.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(nodes_count > 0, "expected lifetime nodes");

    let macro_edges_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("edges")
        .arg("--kind")
        .arg("macro_expansion")
        .output()
        .expect("run sqry graph edges (macro_expansion)");
    assert!(
        macro_edges_output.status.success(),
        "graph edges macro_expansion failed"
    );
    let macro_edges_json = parse_json(&macro_edges_output.stdout);
    let macro_count = macro_edges_json
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert!(macro_count > 0, "expected macro expansion edges");
    let macro_edges = macro_edges_json
        .get("edges")
        .and_then(Value::as_array)
        .expect("edges array");
    let macro_metadata = macro_edges[0]
        .get("metadata")
        .and_then(Value::as_object)
        .expect("macro metadata");
    assert!(macro_metadata.contains_key("expansion_kind"));

    let lifetime_edges_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("edges")
        .arg("--kind")
        .arg("lifetime_constraint")
        .output()
        .expect("run sqry graph edges (lifetime_constraint)");
    assert!(
        lifetime_edges_output.status.success(),
        "graph edges lifetime_constraint failed"
    );
    let lifetime_edges_json = parse_json(&lifetime_edges_output.stdout);
    let lifetime_count = lifetime_edges_json
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert!(lifetime_count > 0, "expected lifetime constraint edges");
    let lifetime_edges = lifetime_edges_json
        .get("edges")
        .and_then(Value::as_array)
        .expect("edges array");
    let lifetime_metadata = lifetime_edges[0]
        .get("metadata")
        .and_then(Value::as_object)
        .expect("lifetime metadata");
    assert!(lifetime_metadata.contains_key("constraint_kind"));
}

#[test]
#[allow(clippy::too_many_lines)] // Integration test verifying all node/edge types
fn graph_nodes_edges_json_fields_and_paging() {
    let project = build_rust_fixture();

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let nodes_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("nodes")
        .arg("--kind")
        .arg("lifetime")
        .arg("--limit")
        .arg("1")
        .arg("--offset")
        .arg("1")
        .output()
        .expect("run sqry graph nodes (paging)");
    assert!(nodes_output.status.success(), "graph nodes paging failed");
    let nodes_json = parse_json(&nodes_output.stdout);
    let nodes_count = nodes_json.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(nodes_count >= 2, "expected at least two lifetime nodes");
    let nodes = nodes_json
        .get("nodes")
        .and_then(Value::as_array)
        .expect("nodes array");
    assert_eq!(nodes.len(), 1, "expected paged nodes length 1");
    assert_object_keys(
        &nodes_json,
        &["count", "limit", "offset", "truncated", "nodes"],
    );
    let node = &nodes[0];
    assert_object_keys(
        node,
        &[
            "id",
            "name",
            "qualified_name",
            "kind",
            "language",
            "file",
            "location",
            "byte_range",
            "signature",
            "doc",
            "visibility",
            "is_async",
            "is_static",
        ],
    );
    assert_object_keys(node.get("id").expect("node id"), &["index", "generation"]);
    assert_object_keys(
        node.get("location").expect("node location"),
        &["start_line", "start_column", "end_line", "end_column"],
    );
    assert_object_keys(
        node.get("byte_range").expect("node byte_range"),
        &["start", "end"],
    );

    let edges_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("edges")
        .arg("--kind")
        .arg("macro_expansion")
        .arg("--limit")
        .arg("1")
        .arg("--offset")
        .arg("1")
        .output()
        .expect("run sqry graph edges (paging)");
    assert!(edges_output.status.success(), "graph edges paging failed");
    let edges_json = parse_json(&edges_output.stdout);
    let edges_count = edges_json.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(
        edges_count >= 2,
        "expected at least two macro expansion edges"
    );
    let edges = edges_json
        .get("edges")
        .and_then(Value::as_array)
        .expect("edges array");
    assert_eq!(edges.len(), 1, "expected paged edges length 1");
    assert_object_keys(
        &edges_json,
        &["count", "limit", "offset", "truncated", "edges"],
    );
    let edge = &edges[0];
    assert_object_keys(edge, &["source", "target", "kind", "file", "metadata"]);
    assert_object_keys(
        edge.get("source").expect("source"),
        &[
            "id",
            "name",
            "qualified_name",
            "language",
            "file",
            "location",
        ],
    );
    assert_object_keys(
        edge.get("target").expect("target"),
        &[
            "id",
            "name",
            "qualified_name",
            "language",
            "file",
            "location",
        ],
    );
    assert_object_keys(
        edge.get("source")
            .and_then(|value| value.get("id"))
            .expect("source id"),
        &["index", "generation"],
    );
    assert_object_keys(
        edge.get("target")
            .and_then(|value| value.get("id"))
            .expect("target id"),
        &["index", "generation"],
    );
    assert_object_keys(
        edge.get("source")
            .and_then(|value| value.get("location"))
            .expect("source location"),
        &["start_line", "start_column", "end_line", "end_column"],
    );
    assert_object_keys(
        edge.get("target")
            .and_then(|value| value.get("location"))
            .expect("target location"),
        &["start_line", "start_column", "end_line", "end_column"],
    );
    let metadata = edge.get("metadata").expect("metadata");
    assert_object_keys(metadata, &["expansion_kind", "is_verified"]);
}

#[test]
fn graph_nodes_edges_offset_beyond_results() {
    let project = build_rust_fixture();

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let nodes_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("nodes")
        .arg("--kind")
        .arg("lifetime")
        .arg("--offset")
        .arg("9999")
        .output()
        .expect("run sqry graph nodes (offset beyond results)");
    assert!(nodes_output.status.success(), "graph nodes offset failed");
    let nodes_json = parse_json(&nodes_output.stdout);
    let nodes_count = nodes_json.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(nodes_count > 0, "expected lifetime nodes");
    let nodes = nodes_json
        .get("nodes")
        .and_then(Value::as_array)
        .expect("nodes array");
    assert!(nodes.is_empty(), "expected empty nodes page");
    let truncated = nodes_json
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    assert!(!truncated, "expected truncated=false for empty page");

    let edges_output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("edges")
        .arg("--kind")
        .arg("macro_expansion")
        .arg("--offset")
        .arg("9999")
        .output()
        .expect("run sqry graph edges (offset beyond results)");
    assert!(edges_output.status.success(), "graph edges offset failed");
    let edges_json = parse_json(&edges_output.stdout);
    let edges_count = edges_json.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(edges_count > 0, "expected macro expansion edges");
    let edges = edges_json
        .get("edges")
        .and_then(Value::as_array)
        .expect("edges array");
    assert!(edges.is_empty(), "expected empty edges page");
    let truncated = edges_json
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    assert!(!truncated, "expected truncated=false for empty page");
}
