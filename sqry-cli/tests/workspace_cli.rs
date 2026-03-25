mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.env("NO_COLOR", "1");
    cmd
}

#[test]
fn workspace_help_lists_subcommands() {
    sqry_cmd()
        .args(["workspace", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Commands:"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("query"))
        .stdout(predicate::str::contains("stats"));
}

#[test]
fn workspace_query_and_stats_flow() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();

    // Create two repositories with simple Rust files
    for repo in ["service-a", "service-b"] {
        let repo_path = workspace_path.join(repo);
        fs::create_dir_all(repo_path.join("src")).unwrap();
        let mut file = fs::File::create(repo_path.join("src/lib.rs")).unwrap();
        writeln!(
            file,
            "pub fn {}_func() {{}}\npub fn shared() {{}}",
            repo.replace('-', "_")
        )
        .unwrap();

        // Build index for the repository
        sqry_cmd()
            .arg("index")
            .current_dir(&repo_path)
            .assert()
            .success();
    }

    let workspace_str = workspace_path.to_str().unwrap();
    let service_a = workspace_path.join("service-a");
    let service_b = workspace_path.join("service-b");

    // Initialise workspace registry
    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "Payments"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Workspace initialised"));

    assert!(workspace_path.join(".sqry-workspace").exists());

    // Add repositories
    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_str,
            service_a.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added repository"));

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_str,
            service_b.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added repository"));

    // Query across both repos
    let query_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_str,
            "kind:function AND name:shared",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&query_output).unwrap();
    let array = json
        .as_array()
        .expect("expected array from workspace query");
    assert!(
        array
            .iter()
            .any(|entry| entry["repo"]["name"] == "service-a"),
        "expected hits from service-a: {array:?}"
    );
    assert!(
        array
            .iter()
            .any(|entry| entry["repo"]["name"] == "service-b"),
        "expected hits from service-b: {array:?}"
    );

    // Stats output
    let stats_output = sqry_cmd()
        .args(["workspace", "stats", workspace_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stats: Value = serde_json::from_slice(&stats_output).unwrap();
    assert_eq!(stats["repositories"]["total"], 2);
    assert_eq!(stats["repositories"]["indexed"], 2);
    assert!(
        stats["symbols"]["total"].is_number(),
        "expected numeric total symbol count"
    );

    // Text output includes repo column
    let text_output = sqry_cmd()
        .args(["workspace", "query", workspace_str, "kind:function"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(text_output).unwrap();
    assert!(
        text.contains("repo service-a") || text.contains("repo service-b"),
        "expected repo column in output: {text}"
    );
}

#[test]
fn workspace_query_qualified_names_for_relations() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();

    // Create two Ruby repos with namespaced code
    for repo in ["api-service", "auth-service"] {
        let repo_path = workspace_path.join(repo);
        fs::create_dir_all(&repo_path).unwrap();

        let ruby_code = format!(
            r#"
module {}
  class Controller
    def execute
      render_view()
    end
  end
end

def render_view
  puts "rendering"
end
"#,
            repo.replace('-', "_").to_uppercase()
        );

        let mut file = fs::File::create(repo_path.join("app.rb")).unwrap();
        write!(file, "{}", ruby_code).unwrap();

        // Index the repository
        sqry_cmd()
            .arg("index")
            .current_dir(&repo_path)
            .assert()
            .success();
    }

    let workspace_str = workspace_path.to_str().unwrap();
    let api_service = workspace_path.join("api-service");
    let auth_service = workspace_path.join("auth-service");

    // Initialize workspace
    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "Services"])
        .assert()
        .success();

    // Add repositories
    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_str,
            api_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_str,
            auth_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test qualified names for individual repo relation query (where it actually works)
    // This tests the --qualified-names flag in a real scenario with caller identities
    let single_repo_qualified = sqry_cmd()
        .args(["query", "--qualified-names", "callers:render_view"])
        .current_dir(&api_service)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let single_repo_text = String::from_utf8(single_repo_qualified).unwrap();

    // Critical assertion: verify --qualified-names actually shows namespace-qualified caller
    // This would fail if the flag was ignored or formatter broke
    assert!(
        single_repo_text.contains("API_SERVICE::Controller#execute"),
        "CRITICAL: --qualified-names MUST show namespace-qualified caller identity (expected API_SERVICE::Controller#execute): {single_repo_text}"
    );

    // Test workspace query finds symbols across repos with qualified names
    // NOTE: Workspace relation queries (callers:, callees:) NOW WORK as of BUG-3 fix (2025-11-20)
    // This test verifies workspace symbol queries work correctly for multi-repo searches,
    // including --qualified-names flag support (BUG-1/BUG-2 fixed: Symbol.qualified_name now populated)
    let workspace_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_str,
            "kind:method",
            "--qualified-names",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let workspace_text = String::from_utf8(workspace_output).unwrap();

    // Verify both repos appear in workspace results
    assert!(
        workspace_text.contains("repo api-service"),
        "Expected api-service repo in workspace results: {workspace_text}"
    );
    assert!(
        workspace_text.contains("repo auth-service"),
        "Expected auth-service repo in workspace results: {workspace_text}"
    );

    // Critical assertion: verify --qualified-names works in workspace mode
    // This catches regressions where the flag is ignored (BUG-1)
    // The api-service repo has Controller#execute method that should show as qualified
    assert!(
        workspace_text.contains("API_SERVICE::Controller#execute")
            || workspace_text.contains("AUTH_SERVICE::Controller#process"),
        "CRITICAL: workspace --qualified-names MUST show namespace-qualified function names: {workspace_text}"
    );

    let workspace_json_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_str,
            "kind:method",
            "--qualified-names",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let workspace_json_text = String::from_utf8(workspace_json_output).unwrap();
    assert!(
        workspace_json_text.contains("\"qualified_name\": \"API_SERVICE::Controller#execute\"")
            || workspace_json_text
                .contains("\"qualified_name\": \"AUTH_SERVICE::Controller#process\""),
        "CRITICAL: workspace JSON MUST preserve Ruby-facing qualified names: {workspace_json_text}"
    );

    // Verify workspace query returns expected number of results
    // Each repo has 1 method: execute (api-service) and process (auth-service)
    // Note: render_view is a function, not a method
    let method_count = workspace_text.matches("method").count();
    assert!(
        method_count >= 2,
        "Expected at least 2 method results (1 per repo), got {method_count}: {workspace_text}"
    );
}

/// Test workspace relation queries (callers:, callees:) across multiple repositories
/// Regression test for BUG-3: Workspace relation queries now work (fixed 2025-11-20)
#[test]
fn workspace_relation_queries_cross_repo() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Create two Ruby repositories with caller/callee relationships
    // API Service: Controller#execute calls render_view
    let api_service = workspace_path.join("api-service");
    fs::create_dir_all(&api_service).unwrap();
    fs::write(
        api_service.join("app.rb"),
        r#"module API_SERVICE
  class Controller
    def execute
      render_view()
    end
  end
end

def render_view
  puts "rendering"
end
"#,
    )
    .unwrap();

    // Auth Service: Controller#process calls render_view
    let auth_service = workspace_path.join("auth-service");
    fs::create_dir_all(&auth_service).unwrap();
    fs::write(
        auth_service.join("app.rb"),
        r#"module AUTH_SERVICE
  class Controller
    def process
      render_view()
    end
  end
end

def render_view
  puts "rendering"
end
"#,
    )
    .unwrap();

    // Index both repositories
    sqry_cmd()
        .args(["index", api_service.to_str().unwrap()])
        .assert()
        .success();

    sqry_cmd()
        .args(["index", auth_service.to_str().unwrap()])
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
            api_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_path.to_str().unwrap(),
            auth_service.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Test 1: Workspace callers: query (BUG-3 fix validation)
    let callers_output = sqry_cmd()
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

    let callers_text = String::from_utf8(callers_output).unwrap();

    // Verify both repos return caller results
    assert!(
        callers_text.contains("repo api-service"),
        "Expected api-service repo in callers query: {callers_text}"
    );
    assert!(
        callers_text.contains("repo auth-service"),
        "Expected auth-service repo in callers query: {callers_text}"
    );

    // Verify caller method names appear
    assert!(
        callers_text.contains("method")
            && (callers_text.contains("execute") || callers_text.contains("process")),
        "Expected caller method names (execute/process) in results: {callers_text}"
    );

    // Test 2: Workspace callers: query with --qualified-names
    let qualified_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "callers:render_view",
            "--qualified-names",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let qualified_text = String::from_utf8(qualified_output).unwrap();

    // Verify qualified caller names appear
    assert!(
        qualified_text.contains("API_SERVICE::Controller#execute")
            || qualified_text.contains("AUTH_SERVICE::Controller#process"),
        "Expected qualified caller names (API_SERVICE::Controller#execute or AUTH_SERVICE::Controller#process): {qualified_text}"
    );

    // Test 3: Workspace callees: query
    let callees_output = sqry_cmd()
        .args([
            "workspace",
            "query",
            workspace_path.to_str().unwrap(),
            "callees:execute",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let callees_text = String::from_utf8(callees_output).unwrap();

    // Verify callee (render_view) appears
    assert!(
        callees_text.contains("render_view"),
        "Expected callee 'render_view' in results: {callees_text}"
    );

    // Verify at least one repo appears (execute is only in api-service)
    assert!(
        callees_text.contains("repo api-service"),
        "Expected api-service repo in callees query: {callees_text}"
    );
}
