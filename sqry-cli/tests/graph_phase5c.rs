//! FR-2025-009 Phase 5C — CLI end-to-end coverage for Haskell, Svelte, Vue, Zig.
//!
//! These tests exercise the sqry CLI graph loader end-to-end by creating small
//! temporary projects per language, indexing them, and asserting that graph
//! statistics reflect nodes and edges for each language. This validates that
//! `GraphBuilders` are registered in the CLI loader and functioning.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    Command::new(path)
}

fn write_file<P: AsRef<Path>>(root: &Path, rel: P, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    let mut f = fs::File::create(&path).expect("create file");
    f.write_all(contents.as_bytes()).expect("write file");
}

fn assert_nodes_and_edges_for_language(stats: &Value, lang: &str) {
    // nodes_by_language requires `--by-language`
    let nodes_by_language = stats
        .get("nodes_by_language")
        .and_then(|v| v.as_object())
        .expect("nodes_by_language map present");
    assert!(
        nodes_by_language.contains_key(lang),
        "expected language '{}' in nodes_by_language, got {:?}",
        lang,
        nodes_by_language.keys().collect::<Vec<_>>()
    );

    // node_count should be >= 1
    let node_count = stats
        .get("node_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        node_count >= 1,
        "expected at least 1 node for {lang}, found {node_count}"
    );

    // edge_count should be >= 1 for our fixtures (each includes at least one call)
    let edge_count = stats
        .get("edge_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        edge_count >= 1,
        "expected at least 1 edge for {lang}, found {edge_count}"
    );
}

#[test]
fn phase5c_haskell_cli_e2e() {
    // Minimal Haskell module with function declarations and calls
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "Main.hs",
        r"module Main where

import Data.List (sort)

add :: Int -> Int -> Int
add x y = x + y

main :: IO ()
main = print (add 1 2)
",
    );

    // Index and query graph stats
    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(output.status.success(), "graph stats failed");
    let stats: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_nodes_and_edges_for_language(&stats, "Haskell");
}

#[test]
fn phase5c_svelte_cli_e2e() {
    // Svelte component with script block containing function calls
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "Component.svelte",
        r#"<script>
  function greet(name) {
    return "Hello " + name;
  }

  function welcome() {
    const message = greet("World");
    console.log(message);
  }
</script>

<div>
  <h1>Svelte Component</h1>
</div>
"#,
    );

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(output.status.success(), "graph stats failed");
    let stats: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_nodes_and_edges_for_language(&stats, "Svelte");
}

#[test]
fn phase5c_vue_cli_e2e() {
    // Vue component with script block containing methods
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "Counter.vue",
        r#"<template>
  <div class="counter">
    <h1>{{ count }}</h1>
  </div>
</template>

<script>
export default {
  name: 'Counter',
  data() {
    return {
      count: 0
    };
  },
  methods: {
    increment() {
      this.count++;
      this.logChange();
    },
    logChange() {
      console.log('Count:', this.count);
    }
  }
};
</script>
"#,
    );

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(output.status.success(), "graph stats failed");
    let stats: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_nodes_and_edges_for_language(&stats, "Vue");
}

#[test]
fn phase5c_zig_cli_e2e() {
    // Zig file with functions and calls
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "main.zig",
        r#"const std = @import("std");

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

fn multiply(a: i32, b: i32) i32 {
    return a * b;
}

pub fn calculate(x: i32, y: i32) i32 {
    const sum = add(x, y);
    const product = multiply(x, y);
    return sum + product;
}
"#,
    );

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(output.status.success(), "graph stats failed");
    let stats: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_nodes_and_edges_for_language(&stats, "Zig");
}

#[test]
fn phase5c_polyglot_project() {
    // Test all Phase 5C languages in a single project
    let project = TempDir::new().expect("create temp project");

    // Haskell file
    write_file(
        project.path(),
        "lib/Utils.hs",
        r#"module Utils where

helper :: String -> String
helper x = "processed: " ++ x
"#,
    );

    // Svelte component
    write_file(
        project.path(),
        "components/Button.svelte",
        r#"<script>
  function onClick() {
    console.log("clicked");
  }
</script>

<button on:click={onClick}>Click me</button>
"#,
    );

    // Vue component
    write_file(
        project.path(),
        "components/Input.vue",
        r#"<template>
  <input @input="handleInput" />
</template>

<script>
export default {
  methods: {
    handleInput(event) {
      this.process(event.target.value);
    },
    process(value) {
      console.log(value);
    }
  }
};
</script>
"#,
    );

    // Zig file
    write_file(
        project.path(),
        "src/math.zig",
        r"pub fn square(x: i32) i32 {
    return x * x;
}

pub fn cube(x: i32) i32 {
    return x * square(x);
}
",
    );

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(output.status.success(), "graph stats failed");
    let stats: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // Verify all Phase 5C languages are present
    assert_nodes_and_edges_for_language(&stats, "Haskell");
    assert_nodes_and_edges_for_language(&stats, "Svelte");
    assert_nodes_and_edges_for_language(&stats, "Vue");
    assert_nodes_and_edges_for_language(&stats, "Zig");

    // Verify overall statistics
    let total_nodes = stats
        .get("node_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        total_nodes >= 10,
        "expected at least 10 nodes across all languages, found {total_nodes}"
    );

    let total_edges = stats
        .get("edge_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        total_edges >= 4,
        "expected at least 4 edges across all languages, found {total_edges}"
    );
}
