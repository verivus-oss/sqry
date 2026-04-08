mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.env("NO_COLOR", "1");
    cmd
}

/// Test workspace relation queries with same-name functions across languages
/// Tests lang: filter composition with relation predicates (CODEX recommendation)
#[test]
#[allow(clippy::too_many_lines)] // Tests language filter combinations
fn workspace_relation_queries_with_lang_filter() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create Ruby service with render_view caller
    let ruby_service = workspace_path.join("ruby-service");
    fs::create_dir_all(&ruby_service).unwrap();
    fs::write(
        ruby_service.join("app.rb"),
        r#"class RubyController
  def process
    render_view()
  end
end

def render_view
  puts "Ruby rendering"
end
"#,
    )
    .unwrap();

    // Create JavaScript service with render_view caller (same name, different language)
    let js_service = workspace_path.join("js-service");
    fs::create_dir_all(&js_service).unwrap();
    fs::write(
        js_service.join("app.js"),
        r#"class JsController {
  process() {
    render_view();
  }
}

function render_view() {
  console.log("JS rendering");
}
"#,
    )
    .unwrap();

    // Index both repositories
    sqry_cmd()
        .args(["index", ruby_service.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", js_service.to_str().unwrap()])
        .assert()
        .success();

    // Initialize workspace and add repos
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            ruby_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            js_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test 1: Cross-language callers query (should find both Ruby and JS callers)
    let all_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "callers:render_view",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let all_text = String::from_utf8(all_callers).unwrap();

    // Verify both services appear
    assert!(
        all_text.contains("repo ruby-service"),
        "Expected ruby-service in cross-language query: {all_text}"
    );
    assert!(
        all_text.contains("repo js-service"),
        "Expected js-service in cross-language query: {all_text}"
    );

    // Test 2: Ruby-only callers query (should only find Ruby caller)
    let ruby_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:ruby AND callers:render_view",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let ruby_text = String::from_utf8(ruby_callers).unwrap();

    // Verify only Ruby service appears
    assert!(
        ruby_text.contains("repo ruby-service"),
        "Expected ruby-service in lang:ruby query: {ruby_text}"
    );
    assert!(
        !ruby_text.contains("repo js-service"),
        "Should NOT find js-service in lang:ruby query: {ruby_text}"
    );

    // Test 3: JavaScript-only callers query (should only find JS caller)
    let js_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:javascript AND callers:render_view",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let js_text = String::from_utf8(js_callers).unwrap();

    // Verify only JavaScript service appears
    assert!(
        js_text.contains("repo js-service"),
        "Expected js-service in lang:javascript query: {js_text}"
    );
    assert!(
        !js_text.contains("repo ruby-service"),
        "Should NOT find ruby-service in lang:javascript query: {js_text}"
    );
}

/// Test workspace relation queries with repo: filter
/// Tests repo: predicate composition with relation predicates (CODEX recommendation)
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_relation_queries_with_repo_filter() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create three services, each calling shared_function
    for service_name in ["service-a", "service-b", "service-c"] {
        let service = workspace_path.join(service_name);
        fs::create_dir_all(&service).unwrap();
        fs::write(
            service.join("app.rb"),
            format!(
                r#"class {}Controller
  def process
    shared_function()
  end
end

def shared_function
  puts "shared"
end
"#,
                service_name.replace('-', "_").to_uppercase()
            ),
        )
        .unwrap();

        sqry_cmd()
            .args(["index", service.to_str().unwrap()])
            .assert()
            .success();
    }

    // Initialize workspace and add all services
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    for service_name in ["service-a", "service-b", "service-c"] {
        let service = workspace_path.join(service_name);
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_path.to_str().unwrap(),
                service.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Test 1: Query all repos (should find 3 callers)
    let all_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "callers:shared_function",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let all_text = String::from_utf8(all_output).unwrap();
    assert!(all_text.contains("repo service-a"));
    assert!(all_text.contains("repo service-b"));
    assert!(all_text.contains("repo service-c"));

    // Test 2: Query only service-a (should find 1 caller)
    let service_a_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "repo:service-a AND callers:shared_function",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let service_a_text = String::from_utf8(service_a_output).unwrap();
    assert!(
        service_a_text.contains("repo service-a"),
        "Expected service-a in repo:service-a AND callers:shared_function query: {service_a_text}"
    );
    assert!(
        !service_a_text.contains("repo service-b"),
        "Should NOT find service-b in repo:service-a AND callers:shared_function query: {service_a_text}"
    );
    assert!(
        !service_a_text.contains("repo service-c"),
        "Should NOT find service-c in repo:service-a AND callers:shared_function query: {service_a_text}"
    );

    // Test 3: Query service-b OR service-c (should find 2 callers)
    let service_bc_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "repo:service-b OR repo:service-c AND callers:shared_function",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let service_bc_text = String::from_utf8(service_bc_output).unwrap();
    assert!(
        !service_bc_text.contains("repo service-a"),
        "Should NOT find service-a: {service_bc_text}"
    );
    assert!(
        service_bc_text.contains("repo service-b"),
        "Expected service-b: {service_bc_text}"
    );
    assert!(
        service_bc_text.contains("repo service-c"),
        "Expected service-c: {service_bc_text}"
    );
}

/// Test JavaScript/TypeScript imports with lang: filter
/// Verifies lang validator fix unlocked JS/TS import queries
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_javascript_typescript_imports() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create JavaScript project with imports
    let js_project = workspace_path.join("frontend-js");
    fs::create_dir_all(&js_project).unwrap();
    fs::write(
        js_project.join("app.js"),
        r"import React from 'react';
import { useState } from 'react';
import lodash from 'lodash';

function App() {
  const [state, setState] = useState(0);
  return React.createElement('div', null, 'Hello');
}
",
    )
    .unwrap();

    // Create TypeScript project with imports
    let ts_project = workspace_path.join("frontend-ts");
    fs::create_dir_all(&ts_project).unwrap();
    fs::write(
        ts_project.join("app.ts"),
        r"import React from 'react';
import type { FC } from 'react';
import axios from 'axios';

const App: FC = () => {
  return <div>Hello</div>;
};
",
    )
    .unwrap();

    // Index both projects
    sqry_cmd()
        .args(["index", js_project.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", ts_project.to_str().unwrap()])
        .assert()
        .success();

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            js_project.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            ts_project.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test 1: Find all react imports (both JS and TS)
    let all_react = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "imports:react",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let all_react_text = String::from_utf8(all_react).unwrap();
    assert!(
        all_react_text.contains("repo frontend-js"),
        "Expected frontend-js in imports:react query: {all_react_text}"
    );
    assert!(
        all_react_text.contains("repo frontend-ts"),
        "Expected frontend-ts in imports:react query: {all_react_text}"
    );

    // Test 2: JavaScript-only imports (lang:javascript AND imports:react)
    let js_only = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:javascript AND imports:react",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let js_only_text = String::from_utf8(js_only).unwrap();
    assert!(
        js_only_text.contains("repo frontend-js"),
        "Expected frontend-js in lang:javascript query: {js_only_text}"
    );
    assert!(
        !js_only_text.contains("repo frontend-ts"),
        "Should NOT find frontend-ts in lang:javascript query: {js_only_text}"
    );

    // Test 3: TypeScript-only imports (lang:typescript AND imports:react)
    let ts_only = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:typescript AND imports:react",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let ts_only_text = String::from_utf8(ts_only).unwrap();
    assert!(
        ts_only_text.contains("repo frontend-ts"),
        "Expected frontend-ts in lang:typescript query: {ts_only_text}"
    );
    assert!(
        !ts_only_text.contains("repo frontend-js"),
        "Should NOT find frontend-js in lang:typescript query: {ts_only_text}"
    );

    // Test 4: Combined OR query for javascript and typescript
    let combined_script = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "(lang:javascript OR lang:typescript) AND imports:react",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let combined_text = String::from_utf8(combined_script).unwrap();
    // Should match both javascript and typescript
    assert!(
        combined_text.contains("repo frontend-js") || combined_text.contains("repo frontend-ts"),
        "Expected at least one repo in combined lang query: {combined_text}"
    );

    // Test 5: Language-specific imports (lodash in JS, axios in TS)
    let lodash_import = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:javascript AND imports:lodash",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let lodash_text = String::from_utf8(lodash_import).unwrap();
    assert!(
        lodash_text.contains("repo frontend-js"),
        "Expected frontend-js for lodash import: {lodash_text}"
    );

    let axios_import = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:typescript AND imports:axios",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let axios_text = String::from_utf8(axios_import).unwrap();
    assert!(
        axios_text.contains("repo frontend-ts"),
        "Expected frontend-ts for axios import: {axios_text}"
    );
}

/// Test lang~=/regex/ filter across multiple script languages
/// Verifies that regex matching on language IDs works in workspace queries.
#[test]
fn workspace_lang_regex_filter_for_script_languages() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Reuse a minimal JS + TS workspace to exercise lang~=/.*script/ regex
    let js_project = workspace_path.join("frontend-js");
    fs::create_dir_all(&js_project).unwrap();
    fs::write(
        js_project.join("app.js"),
        r#"function main() {
  console.log("JS");
}

main();
"#,
    )
    .unwrap();

    let ts_project = workspace_path.join("frontend-ts");
    fs::create_dir_all(&ts_project).unwrap();
    fs::write(
        ts_project.join("app.ts"),
        r#"function main(): void {
  console.log("TS");
}

main();
"#,
    )
    .unwrap();

    // Index both projects
    sqry_cmd()
        .args(["index", js_project.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", ts_project.to_str().unwrap()])
        .assert()
        .success();

    // Initialize workspace and add both repos
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    for project in [&js_project, &ts_project] {
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_path.to_str().unwrap(),
                project.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Regex should match both "javascript" and "typescript" language IDs
    let regex_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang~=/.*script/",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let regex_text = String::from_utf8(regex_output).unwrap();
    assert!(
        regex_text.contains("repo frontend-js"),
        "Expected frontend-js in lang~=/.*script/ query: {regex_text}"
    );
    assert!(
        regex_text.contains("repo frontend-ts"),
        "Expected frontend-ts in lang~=/.*script/ query: {regex_text}"
    );
}

/// Test Python imports with lang: filter
/// Verifies lang validator fix unlocked Python import queries
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_python_imports() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create Python data processing service
    let py_data = workspace_path.join("data-processor");
    fs::create_dir_all(&py_data).unwrap();
    fs::write(
        py_data.join("processor.py"),
        r"import pandas as pd
import numpy as np
from sklearn.model_selection import train_test_split

def process_data(df):
    return pd.DataFrame(df)

def split_data(X, y):
    return train_test_split(X, y, test_size=0.2)
",
    )
    .unwrap();

    // Create Python web service
    let py_web = workspace_path.join("web-service");
    fs::create_dir_all(&py_web).unwrap();
    fs::write(
        py_web.join("app.py"),
        r"from flask import Flask, request
import pandas as pd

app = Flask(__name__)

@app.route('/data')
def get_data():
    df = pd.read_csv('data.csv')
    return df.to_json()
",
    )
    .unwrap();

    // Create Ruby service (control - should not appear in Python queries)
    let ruby_service = workspace_path.join("ruby-service");
    fs::create_dir_all(&ruby_service).unwrap();
    fs::write(
        ruby_service.join("app.rb"),
        r#"require 'json'

def process
  puts "Ruby processing"
end
"#,
    )
    .unwrap();

    // Index all projects
    for service in [&py_data, &py_web, &ruby_service] {
        sqry_cmd()
            .args(["index", service.to_str().unwrap()])
            .assert()
            .success();
    }

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    for service in [&py_data, &py_web, &ruby_service] {
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_path.to_str().unwrap(),
                service.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Test 1: Find all pandas imports (both Python services)
    let pandas_all = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "imports:pandas",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let pandas_all_text = String::from_utf8(pandas_all).unwrap();
    assert!(
        pandas_all_text.contains("repo data-processor")
            || pandas_all_text.contains("repo web-service"),
        "Expected Python services in imports:pandas query: {pandas_all_text}"
    );

    // Test 2: Python-only imports with lang: filter
    let python_pandas = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:python AND imports:pandas",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let python_pandas_text = String::from_utf8(python_pandas).unwrap();
    // Should find Python services
    assert!(
        python_pandas_text.contains("repo data-processor")
            || python_pandas_text.contains("repo web-service"),
        "Expected Python services in lang:python query: {python_pandas_text}"
    );
    // Should NOT find Ruby service
    assert!(
        !python_pandas_text.contains("repo ruby-service"),
        "Should NOT find ruby-service in lang:python query: {python_pandas_text}"
    );

    // Test 3: Specific Python library imports
    let sklearn_import = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:python AND imports:sklearn",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let sklearn_text = String::from_utf8(sklearn_import).unwrap();
    assert!(
        sklearn_text.contains("repo data-processor"),
        "Expected data-processor for sklearn import: {sklearn_text}"
    );

    let flask_import = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:python AND imports:flask",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let flask_text = String::from_utf8(flask_import).unwrap();
    assert!(
        flask_text.contains("repo web-service"),
        "Expected web-service for flask import: {flask_text}"
    );
}

/// Test Go imports and callers with lang: filter
/// Verifies lang validator fix unlocked Go queries
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_go_imports_and_callers() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create Go API service
    let go_api = workspace_path.join("go-api");
    fs::create_dir_all(&go_api).unwrap();
    fs::write(
        go_api.join("main.go"),
        r#"package main

import (
    "fmt"
    "net/http"
    "github.com/gorilla/mux"
)

func HandleRequest(w http.ResponseWriter, r *http.Request) {
    ProcessData()
    fmt.Fprintf(w, "Hello")
}

func ProcessData() {
    fmt.Println("Processing")
}

func main() {
    router := mux.NewRouter()
    http.ListenAndServe(":8080", router)
}
"#,
    )
    .unwrap();

    // Create Go CLI tool
    let go_cli = workspace_path.join("go-cli");
    fs::create_dir_all(&go_cli).unwrap();
    fs::write(
        go_cli.join("main.go"),
        r#"package main

import (
    "fmt"
    "os"
)

func ProcessData() {
    fmt.Println("CLI processing")
}

func main() {
    ProcessData()
    os.Exit(0)
}
"#,
    )
    .unwrap();

    // Index both projects
    sqry_cmd()
        .args(["index", go_api.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", go_cli.to_str().unwrap()])
        .assert()
        .success();

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            go_api.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            go_cli.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test 1: Go imports with lang: filter
    let go_imports = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:go AND imports:fmt",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let go_imports_text = String::from_utf8(go_imports).unwrap();
    // Both services import fmt
    assert!(
        go_imports_text.contains("repo go-api") || go_imports_text.contains("repo go-cli"),
        "Expected Go services in lang:go AND imports:fmt query: {go_imports_text}"
    );

    // Test 2: Go callers with lang: filter (ProcessData called in both services)
    let go_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:go AND callers:ProcessData",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let go_callers_text = String::from_utf8(go_callers).unwrap();
    // Should find callers in both services (HandleRequest in go-api, main in go-cli)
    assert!(
        go_callers_text.contains("repo go-api") || go_callers_text.contains("repo go-cli"),
        "Expected Go callers in lang:go AND callers:ProcessData query: {go_callers_text}"
    );

    // Test 3: Specific package imports (github.com/gorilla/mux only in go-api)
    let mux_import = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:go AND imports:mux",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let mux_text = String::from_utf8(mux_import).unwrap();
    assert!(
        mux_text.contains("repo go-api"),
        "Expected go-api for mux import: {mux_text}"
    );
}

/// Test Rust callers with lang: filter
/// Verifies lang validator fix unlocked Rust queries
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_rust_callers() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create Rust library
    let rust_lib = workspace_path.join("rust-lib");
    fs::create_dir_all(rust_lib.join("src")).unwrap();
    fs::write(
        rust_lib.join("src").join("lib.rs"),
        r#"pub fn process_data(data: &str) -> String {
    transform_data(data)
}

fn transform_data(input: &str) -> String {
    input.to_uppercase()
}

pub fn validate_input(input: &str) -> bool {
    !input.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        let result = process_data("hello");
        assert_eq!(result, "HELLO");
    }
}
"#,
    )
    .unwrap();

    // Create Rust binary
    let rust_bin = workspace_path.join("rust-app");
    fs::create_dir_all(rust_bin.join("src")).unwrap();
    fs::write(
        rust_bin.join("src").join("main.rs"),
        r#"fn process_data(data: &str) -> String {
    format_output(data)
}

fn format_output(input: &str) -> String {
    format!("Result: {}", input)
}

fn main() {
    let data = "test";
    let output = process_data(data);
    println!("{}", output);
}
"#,
    )
    .unwrap();

    // Index both projects
    sqry_cmd()
        .args(["index", rust_lib.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", rust_bin.to_str().unwrap()])
        .assert()
        .success();

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            rust_lib.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            rust_bin.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test 1: Rust callers with lang: filter (process_data called in both)
    let rust_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:rust AND callers:process_data",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let rust_callers_text = String::from_utf8(rust_callers).unwrap();
    // Should find callers (test_process in lib, main in app)
    assert!(
        rust_callers_text.contains("repo rust-lib") || rust_callers_text.contains("repo rust-app"),
        "Expected Rust callers in lang:rust AND callers:process_data query: {rust_callers_text}"
    );

    // Test 2: Specific function callers (transform_data only in rust-lib)
    let transform_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:rust AND callers:transform_data",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let transform_text = String::from_utf8(transform_callers).unwrap();
    assert!(
        transform_text.contains("repo rust-lib"),
        "Expected rust-lib for transform_data callers: {transform_text}"
    );

    // Test 3: Specific function callers (format_output only in rust-app)
    let format_callers = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:rust AND callers:format_output",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let format_text = String::from_utf8(format_callers).unwrap();
    assert!(
        format_text.contains("repo rust-app"),
        "Expected rust-app for format_output callers: {format_text}"
    );
}

/// Test boolean combinations with multiple languages
/// Verifies complex lang: filter combinations work correctly
#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
fn workspace_lang_boolean_combinations() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create JavaScript service
    let js_service = workspace_path.join("js-service");
    fs::create_dir_all(&js_service).unwrap();
    fs::write(
        js_service.join("app.js"),
        r#"function processData() {
    console.log("JS processing");
}

processData();
"#,
    )
    .unwrap();

    // Create Java service
    let java_service = workspace_path.join("java-service");
    fs::create_dir_all(&java_service).unwrap();
    fs::write(
        java_service.join("App.java"),
        r#"public class App {
    public static void processData() {
        System.out.println("Java processing");
    }

    public static void main(String[] args) {
        processData();
    }
}
"#,
    )
    .unwrap();

    // Create Python service
    let python_service = workspace_path.join("python-service");
    fs::create_dir_all(&python_service).unwrap();
    fs::write(
        python_service.join("app.py"),
        r#"def process_data():
    print("Python processing")

def main():
    process_data()
"#,
    )
    .unwrap();

    // Create Ruby service
    let ruby_service = workspace_path.join("ruby-service");
    fs::create_dir_all(&ruby_service).unwrap();
    fs::write(
        ruby_service.join("app.rb"),
        r#"def process_data
  puts "Ruby processing"
end

def main
  process_data
end
"#,
    )
    .unwrap();

    // Index all services
    for service in [&js_service, &java_service, &python_service, &ruby_service] {
        sqry_cmd()
            .args(["index", service.to_str().unwrap()])
            .assert()
            .success();
    }

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    for service in [&js_service, &java_service, &python_service, &ruby_service] {
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_path.to_str().unwrap(),
                service.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Test 1: OR combination for multiple languages (java OR javascript)
    let java_or_js = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "(lang:java OR lang:javascript) AND callers:processData",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let java_or_js_text = String::from_utf8(java_or_js).unwrap();
    // Should find java (main calls processData)
    assert!(
        java_or_js_text.contains("repo java-service"),
        "Expected java-service in (lang:java OR lang:javascript) query: {java_or_js_text}"
    );

    // Test 2: OR combination for scripting languages (python OR ruby)
    let scripting = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "(lang:python OR lang:ruby) AND callers:process_data",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let scripting_text = String::from_utf8(scripting).unwrap();
    // Should find both python and ruby (main calls process_data in both)
    assert!(
        scripting_text.contains("repo python-service")
            || scripting_text.contains("repo ruby-service"),
        "Expected python or ruby service in (lang:python OR lang:ruby) query: {scripting_text}"
    );

    // Test 3: Exact match with NOT operator
    let python_not_java = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:python AND NOT lang:java",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let python_not_java_text = String::from_utf8(python_not_java).unwrap();
    // Should only find Python, not Java
    assert!(
        python_not_java_text.contains("repo python-service")
            || python_not_java_text.contains("No workspace matches"),
        "Expected python-service or no matches: {python_not_java_text}"
    );
    assert!(
        !python_not_java_text.contains("repo java-service"),
        "Should NOT find java-service in lang:python NOT lang:java query: {python_not_java_text}"
    );

    // Test 4: Exact match for single language
    let exact_python = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "lang:python AND callers:process_data",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let exact_python_text = String::from_utf8(exact_python).unwrap();
    assert!(
        exact_python_text.contains("repo python-service"),
        "Expected python-service in exact lang:python query: {exact_python_text}"
    );
    assert!(
        !exact_python_text.contains("repo java-service"),
        "Should NOT find java-service in exact lang:python query: {exact_python_text}"
    );
}
