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
#[allow(clippy::too_many_lines)] // CLI integration test covers many subcommands
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
    // Issue #515 regression: `workspace stats` used to report 0 symbols
    // for member repos that `workspace query` (above) had just returned
    // real hits from, because discovery never populated the registry's
    // cached `symbol_count`. `is_number()` alone let that regress
    // silently (0 is a number too), so this must assert nonzero.
    assert!(
        stats["symbols"]["total"].as_u64().unwrap_or(0) > 0,
        "expected a nonzero total symbol count now that both service-a and \
         service-b are indexed, got: {stats:?}"
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

/// Regression test for issue #515: `sqry workspace stats` reported
/// `Total symbols: 0` for a workspace whose member repos were indexed and
/// queryable (`sqry workspace query` returned real hits from the same
/// members). Root cause: `WorkspaceRepository::new` always defaults
/// `symbol_count` to `None`, and neither `discover_repositories` nor
/// `sqry workspace add` ever populated it, so `DetailedWorkspaceStats`'s
/// `filter_map(|r| r.symbol_count)` summed zero every time, no matter how
/// many symbols the member graphs actually held.
///
/// This test pins the fix to an exact, independently-verifiable number:
/// it reads each member's own `.sqry/graph/manifest.json` `node_count`
/// (the same manifest `sqry graph status --json`'s `symbol_count` field
/// is sourced from) and asserts the workspace-level total equals their
/// sum precisely, not just "some nonzero number".
#[test]
fn workspace_stats_symbol_count_matches_member_manifests() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();

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

        sqry_cmd()
            .arg("index")
            .current_dir(&repo_path)
            .assert()
            .success();
    }

    let workspace_str = workspace_path.to_str().unwrap();
    let service_a = workspace_path.join("service-a");
    let service_b = workspace_path.join("service-b");

    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "manifest-sum"])
        .assert()
        .success();

    for repo_path in [&service_a, &service_b] {
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_str,
                repo_path.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Ground truth: read node_count straight out of each member's own
    // manifest.json, independently of anything the workspace registry
    // computed.
    let expected_total: u64 = [&service_a, &service_b]
        .iter()
        .map(|repo_path| {
            let manifest_path = repo_path.join(".sqry/graph/manifest.json");
            let manifest: Value =
                serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
            manifest["node_count"]
                .as_u64()
                .expect("manifest.json must carry a numeric node_count")
        })
        .sum();
    assert!(
        expected_total > 0,
        "test fixture sanity check: indexed repos must have a nonzero node_count"
    );

    let stats_output = sqry_cmd()
        .args(["workspace", "stats", workspace_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stats: Value = serde_json::from_slice(&stats_output).unwrap();

    assert_eq!(
        stats["symbols"]["total"].as_u64(),
        Some(expected_total),
        "workspace stats total_symbols must equal the sum of both members' \
         manifest.json node_count, got: {stats:?}"
    );
    assert!(
        (stats["symbols"]["avg_per_repo"].as_f64().unwrap() - (expected_total as f64 / 2.0)).abs()
            < f64::EPSILON,
        "avg_per_repo must be total_symbols / 2 (both members contributed a known count): {stats:?}"
    );
    assert_eq!(
        stats["symbols"]["unknown_count_repos"].as_u64(),
        Some(0),
        "both members have a readable manifest.json, so unknown_count_repos must be 0: {stats:?}"
    );

    // Sibling counters that read from the same registry must stay
    // consistent with the now-correct symbol totals, not regress
    // alongside a partial fix.
    assert_eq!(stats["repositories"]["total"], 2);
    assert_eq!(stats["repositories"]["indexed"], 2);
    assert_eq!(stats["repositories"]["unindexed"], 0);
    assert_eq!(stats["freshness"]["never_indexed"], 0);

    // The text-mode banner must show the same nonzero total, not a
    // format-specific divergence between --json and text output.
    let text_output = sqry_cmd()
        .args(["workspace", "stats", workspace_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text_output).unwrap();
    assert!(
        text.contains(&format!("Total symbols: {expected_total} ")),
        "text output must report the same total as --json: {text}"
    );
}

/// Staleness regression from the #515 cross-LLM gate: the original fix
/// populated `WorkspaceRepository::symbol_count` only at `workspace
/// init` / `workspace add` time and cached it in the registry, so `sqry
/// workspace stats` kept reporting the *registration-time* count forever
/// after a member was reindexed directly (`sqry index --force`) without
/// a matching `workspace remove` + `workspace add` round-trip. Meanwhile
/// `sqry workspace query` always read member graphs live and reflected
/// the change immediately, so the two commands silently disagreed.
///
/// This test reindexes `service-a` in place (adding a third function, so
/// its `node_count` grows) with no `workspace remove`/`add` in between,
/// and asserts `workspace stats` picks up the new total on the very next
/// run, matching each member's live `.sqry/graph/manifest.json`
/// `node_count` exactly.
#[test]
fn workspace_stats_reflects_reindex_without_add_remove() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();

    let service_a = workspace_path.join("service-a");
    let service_b = workspace_path.join("service-b");

    for (repo_path, repo) in [(&service_a, "service-a"), (&service_b, "service-b")] {
        fs::create_dir_all(repo_path.join("src")).unwrap();
        let mut file = fs::File::create(repo_path.join("src/lib.rs")).unwrap();
        writeln!(
            file,
            "pub fn {}_func() {{}}\npub fn shared() {{}}",
            repo.replace('-', "_")
        )
        .unwrap();

        sqry_cmd()
            .arg("index")
            .current_dir(repo_path)
            .assert()
            .success();
    }

    let workspace_str = workspace_path.to_str().unwrap();

    sqry_cmd()
        .args([
            "workspace",
            "init",
            workspace_str,
            "--name",
            "reindex-drift",
        ])
        .assert()
        .success();

    for repo_path in [&service_a, &service_b] {
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_str,
                repo_path.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    let read_node_count = |repo_path: &std::path::Path| -> u64 {
        let manifest_path = repo_path.join(".sqry/graph/manifest.json");
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["node_count"]
            .as_u64()
            .expect("manifest.json must carry a numeric node_count")
    };

    let stats_json = |workspace_str: &str| -> Value {
        let output = sqry_cmd()
            .args(["workspace", "stats", workspace_str, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };

    let node_count_a_before = read_node_count(&service_a);
    let node_count_b = read_node_count(&service_b);

    let stats_before = stats_json(workspace_str);
    assert_eq!(
        stats_before["symbols"]["total"].as_u64(),
        Some(node_count_a_before + node_count_b),
        "before reindex, stats must match both members' current manifests: {stats_before:?}"
    );

    // Reindex service-a directly (`sqry index --force`), growing its
    // node_count by adding a third function, WITHOUT running `workspace
    // remove` / `workspace add` again. This is the exact drift the gate
    // reproduced (member 8 -> 18 nodes with stats stuck at the old
    // total).
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(service_a.join("src/lib.rs"))
        .unwrap();
    writeln!(file, "pub fn extra_func() {{}}").unwrap();
    drop(file);

    sqry_cmd()
        .args(["index", "--force"])
        .current_dir(&service_a)
        .assert()
        .success();

    let node_count_a_after = read_node_count(&service_a);
    assert!(
        node_count_a_after > node_count_a_before,
        "test fixture sanity check: reindexing after adding a function must grow node_count \
         (before: {node_count_a_before}, after: {node_count_a_after})"
    );

    let stats_after = stats_json(workspace_str);
    assert_eq!(
        stats_after["symbols"]["total"].as_u64(),
        Some(node_count_a_after + node_count_b),
        "workspace stats must reflect the reindexed service-a manifest immediately, \
         without a workspace remove/add round-trip: before={stats_before:?} after={stats_after:?}"
    );
    assert_ne!(
        stats_after["symbols"]["total"].as_u64(),
        stats_before["symbols"]["total"].as_u64(),
        "the total must actually change after the reindex, proving stats did not just \
         echo the stale registration-time count: {stats_after:?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
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
        write!(file, "{ruby_code}").unwrap();

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
#[allow(clippy::too_many_lines)] // End-to-end integration test exercises many query/assertion combinations
#[allow(clippy::similar_names)] // callers_output/callers_text and callees_output/callees_text are intentional
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
    #[allow(clippy::similar_names)] // Test variables: expected_nodes/expected_edges
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
