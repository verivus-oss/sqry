//! Black-box feature-surface exercise for the `sqry` CLI.

mod common;

use common::sqry_bin;
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create destination directory");
    for entry in fs::read_dir(src).expect("read source directory") {
        let entry = entry.expect("read dir entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy fixture file");
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn setup_fixture() -> TempDir {
    let temp = TempDir::new().expect("create temp fixture");
    let src = workspace_root().join("test-fixtures/e2e-scenarios/multi-lang");
    copy_dir_recursive(&src, temp.path());
    fs::create_dir_all(temp.path().join(".home")).expect("create isolated home");
    fs::create_dir_all(temp.path().join(".xdg/config")).expect("create isolated config");
    fs::create_dir_all(temp.path().join(".xdg/cache")).expect("create isolated cache");
    fs::create_dir_all(temp.path().join(".xdg/data")).expect("create isolated data");
    temp
}

fn run<I, S>(project: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(sqry_bin())
        .args(args)
        .current_dir(project)
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("SQRY_REDACTION_PRESET", "none")
        .env("HOME", project.join(".home"))
        .env("XDG_CONFIG_HOME", project.join(".xdg/config"))
        .env("XDG_CACHE_HOME", project.join(".xdg/cache"))
        .env("XDG_DATA_HOME", project.join(".xdg/data"))
        .output()
        .expect("run sqry")
}

fn assert_success(name: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(name: &str, output: &Output) {
    assert!(
        !output.status.success(),
        "{name} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_json(name: &str, output: &Output) -> Value {
    assert_success(name, output);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{name} did not emit JSON: {error}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn assert_non_empty_stdout(name: &str, output: &Output) {
    assert_success(name, output);
    assert!(
        !output.stdout.is_empty(),
        "{name} produced empty stdout\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("run git");
    assert_success(&format!("git {}", args.join(" ")), &output);
}

fn make_git_history(project: &Path) {
    git(project, &["init", "--initial-branch", "main"]);
    git(project, &["config", "user.name", "sqry e2e"]);
    git(
        project,
        &["config", "user.email", "sqry-e2e@example.invalid"],
    );
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "baseline"]);

    let lib = project.join("src/lib.rs");
    let mut content = fs::read_to_string(&lib).expect("read lib.rs");
    content.push_str("\npub fn surface_added(value: i32) -> i32 { value + 7 }\n");
    fs::write(&lib, content).expect("write changed lib.rs");
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "add surface function"]);
}

#[test]
fn installed_cli_feature_surface_matrix() {
    let project = setup_fixture();
    make_git_history(project.path());
    let batch_file = project.path().join("queries.sqry");
    fs::write(&batch_file, "kind:function\nname:process\n").expect("write batch queries");

    assert_success("index", &run(project.path(), ["index", "."]));
    assert_success(
        "index status",
        &run(project.path(), ["index", "--status", "--json", "."]),
    );

    let successful_commands: &[(&str, &[&str])] = &[
        ("root search", &["process", "."]),
        ("search", &["search", "process", "."]),
        (
            "semantic search",
            &["--semantic", "--limit", "5", "process", "."],
        ),
        ("text search", &["--text", "--limit", "5", "process", "."]),
        ("query", &["query", "kind:function", "."]),
        ("query json", &["--json", "query", "kind:function", "."]),
        ("ask", &["ask", "find functions named process", "."]),
        ("plan-query", &["plan-query", "kind:function", "."]),
        ("hier", &["hier", "process", "--path", "."]),
        ("update", &["update", "."]),
        ("analyze", &["analyze", "."]),
        ("repair", &["repair", "."]),
        ("graph status", &["graph", "--path", ".", "status"]),
        (
            "graph stats",
            &["graph", "--path", ".", "--format", "json", "stats"],
        ),
        (
            "graph nodes",
            &["graph", "--path", ".", "--format", "json", "nodes"],
        ),
        (
            "graph edges",
            &["graph", "--path", ".", "--format", "json", "edges"],
        ),
        (
            "graph direct-callers",
            &["graph", "--path", ".", "direct-callers", "helper"],
        ),
        (
            "graph direct-callees",
            &["graph", "--path", ".", "direct-callees", "process"],
        ),
        (
            "graph call-hierarchy",
            &["graph", "--path", ".", "call-hierarchy", "process"],
        ),
        (
            "graph is-in-cycle",
            &["graph", "--path", ".", "is-in-cycle", "helper"],
        ),
        (
            "graph cycles",
            &["graph", "--path", ".", "--format", "json", "cycles"],
        ),
        (
            "graph complexity",
            &["graph", "--path", ".", "--format", "json", "complexity"],
        ),
        (
            "graph dependency-tree",
            &["graph", "--path", ".", "dependency-tree", "process"],
        ),
        ("duplicates", &["duplicates", "."]),
        ("cycles", &["cycles", "."]),
        ("unused", &["unused", "."]),
        ("impact", &["impact", "process", "--path", "."]),
        ("diff", &["diff", "HEAD~1", "HEAD", "--path", "."]),
        (
            "explain",
            &["explain", "src/lib.rs", "process", "--path", "."],
        ),
        (
            "similar",
            &["similar", "src/lib.rs", "process", "--path", "."],
        ),
        ("subgraph", &["subgraph", "process", "--path", "."]),
        (
            "visualize",
            &["visualize", "callees:process", "--path", "."],
        ),
        ("export", &["export", "--format", "json", "."]),
        ("daemon status", &["daemon", "status"]),
        ("config init", &["config", "init", "--path", "."]),
        ("config show", &["config", "show", "--path", ".", "--json"]),
        ("cache stats", &["cache", "stats"]),
        ("workspace init", &["workspace", "init", "."]),
        (
            "workspace add",
            &["workspace", "add", ".", ".", "--name", "fixture"],
        ),
        ("workspace stats", &["workspace", "stats", "."]),
        ("alias list", &["alias", "list"]),
        ("history stats", &["history", "stats"]),
        ("completions", &["completions", "bash"]),
        ("mcp status", &["mcp", "status", "--json"]),
        (
            "batch",
            &[
                "batch",
                "--queries",
                batch_file.to_str().expect("batch path is utf8"),
                "--output",
                "json",
                ".",
            ],
        ),
        ("insights", &["insights", "status"]),
        ("troubleshoot", &["troubleshoot", "--help"]),
        ("watch help", &["watch", "--help"]),
        ("lsp help", &["lsp", "--help"]),
        ("shell help", &["shell", "--help"]),
    ];

    for (name, args) in successful_commands {
        let output = run(project.path(), *args);
        assert_success(name, &output);
    }

    let json_commands: &[(&str, &[&str])] = &[
        (
            "query json parse",
            &["--json", "query", "kind:function", "."],
        ),
        (
            "graph stats json parse",
            &["graph", "--path", ".", "--format", "json", "stats"],
        ),
        (
            "config show json parse",
            &["config", "show", "--path", ".", "--json"],
        ),
        ("mcp status json parse", &["mcp", "status", "--json"]),
    ];

    for (name, args) in json_commands {
        let json = assert_json(name, &run(project.path(), *args));
        match *name {
            "query json parse" => assert!(
                json["stats"]["total_matches"].as_u64().unwrap_or(0) > 0,
                "query JSON must report matches: {json}"
            ),
            "graph stats json parse" => assert!(
                json["node_count"].as_u64().unwrap_or(0) > 0
                    || json["total_nodes"].as_u64().unwrap_or(0) > 0
                    || json["nodes"].as_u64().unwrap_or(0) > 0,
                "graph stats JSON must report node counts: {json}"
            ),
            "config show json parse" => assert_eq!(
                json["schema_version"].as_u64(),
                Some(1),
                "config JSON must report schema_version=1: {json}"
            ),
            "mcp status json parse" => assert!(
                json.is_object(),
                "mcp status JSON must be an object: {json}"
            ),
            _ => {}
        }
    }

    assert_non_empty_stdout(
        "completions output",
        &run(project.path(), ["completions", "bash"]),
    );
    assert_failure(
        "invalid path",
        &run(project.path(), ["index", "/definitely/not/a/sqry/path"]),
    );
}
