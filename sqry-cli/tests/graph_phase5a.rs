//! FR-2025-009 Phase 5A — CLI end-to-end coverage for Lua, Shell, Perl, Groovy.
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

    // edge_count should be >= 1 for our fixtures (each includes at least one import/call)
    let edge_count = stats
        .get("edge_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        edge_count >= 1,
        "expected at least 1 edge, found {edge_count}"
    );
}

#[test]
fn phase5a_lua_cli_e2e() {
    // Minimal Lua module with import edges via require + loadfile
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "main.lua",
        r"
local M = {}

function M.add(a, b)
  return a + b
end

local json = require('dkjson')
loadfile('scripts/init.lua')()

return M
",
    );
    write_file(project.path(), "scripts/init.lua", "return true\n");

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
    assert_nodes_and_edges_for_language(&stats, "Lua");
}

#[test]
fn phase5a_shell_cli_e2e() {
    // Shell script sourcing another script to create an import edge
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "lib/common.sh",
        r"#!/usr/bin/env bash
helper() { echo helper; }
",
    );
    write_file(
        project.path(),
        "run.sh",
        r"#!/usr/bin/env bash
source ./lib/common.sh
main() { helper; }
main
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
    assert_nodes_and_edges_for_language(&stats, "Shell");
}

#[test]
fn phase5a_perl_cli_e2e() {
    // Perl script using 'use' and 'require' to create import edges
    let project = TempDir::new().expect("create temp project");
    write_file(project.path(), "lib/common.pm", "package common; 1;\n");
    write_file(
        project.path(),
        "main.pl",
        r"use strict; use warnings;
use lib './lib';
use JSON qw(encode_json);
require 'common.pm';

sub main { my $x = 1; return $x; }
main();
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
    assert_nodes_and_edges_for_language(&stats, "Perl");
}

#[test]
fn phase5a_groovy_cli_e2e() {
    // Simple Groovy class with a static call to ensure nodes and a call edge
    let project = TempDir::new().expect("create temp project");
    write_file(
        project.path(),
        "App.groovy",
        r"class App {
  static void helper() { }
  static void main(String[] args) {
    helper()
  }
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
    assert_nodes_and_edges_for_language(&stats, "Groovy");
}
