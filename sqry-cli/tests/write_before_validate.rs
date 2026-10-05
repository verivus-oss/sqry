//! CLI commands check their inputs before they write anything (S7 class,
//! round 7).
//!
//! Each command below used to write and only then refuse an input it could
//! have checked first:
//! - `search` and `query` under `--validate fail --auto-rebuild` rebuilt a
//!   stale index before refusing a bad pattern, query or `--var`;
//! - `--save-as` with a name the alias store refuses ran the command and
//!   recorded its history entry first;
//! - `query` with a `$name` that `--var` does not define auto-built an index
//!   for an unindexed path (semantic, join and pipeline forms) first;
//! - `cache prune` without a retention policy created the cache root first;
//! - `history clear` with a bad `--older` or without `--confirm`,
//!   `alias rename` to a bad name and `alias import` of an unreadable or
//!   malformed file created the global config directory first;
//! - `diff` with a plugin-selection override created both git worktrees
//!   before refusing it.
//!
//! Round 8 widens the first item to every form those commands dispatch to:
//! the round-7 check covered only the structural query, so a pipeline or a
//! join with an unknown field, a `--text` query its regex engine refuses,
//! `--session` beside `--text`, a fuzzy or JSON-stream search with an
//! unknown `--fuzzy-algorithm`, and an aliased query of any of those forms
//! still rebuilt the stale index before refusing.
//!
//! Each test runs the bad input, asserts the refusal, and asserts the write
//! target was not created or changed; each has an accepted control that
//! shows the target is written when the input is good.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::sqry_bin;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// A scratch home for one test: the global config directory and the cache
/// root the commands would create live under it, and no daemon is reachable.
struct Scratch {
    dir: TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: TempDir::new().expect("scratch"),
        }
    }

    fn config_dir(&self) -> PathBuf {
        self.dir.path().join("cfg")
    }

    fn cache_root(&self) -> PathBuf {
        self.dir.path().join("cacheroot")
    }

    fn run_with(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = std::process::Command::new(sqry_bin());
        command
            .args(args)
            .current_dir(cwd)
            .env("SQRY_CONFIG_DIR", self.config_dir())
            .env("SQRY_CACHE_ROOT", self.cache_root())
            .env("SQRY_DAEMON_SOCKET", self.dir.path().join("no-daemon.sock"))
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("SQRY_AUTO_INDEX", "1")
            .env("NO_COLOR", "1")
            .env_remove("SQRY_NO_HISTORY")
            .env_remove("SQRY_INCLUDE_HIGH_COST")
            .env_remove("SQRY_EXCLUDE_HIGH_COST");
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().expect("run sqry");
        println!(
            "sqry {args:?}: {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_with(cwd, args, &[])
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A Rust workspace with `alpha` and `beta`, not indexed.
fn workspace() -> TempDir {
    let dir = TempDir::new().expect("workspace");
    std::fs::create_dir_all(dir.path().join("src")).expect("src");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"wbv\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "pub fn alpha() { beta(); }\npub fn beta() {}\n",
    )
    .expect("lib.rs");
    dir
}

fn snapshot_digest(root: &Path) -> String {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    hex::encode(Sha256::digest(
        std::fs::read(storage.snapshot_path()).expect("snapshot"),
    ))
}

/// An indexed workspace whose index is stale past the default 20% orphan
/// threshold: three of its four files were deleted after indexing, so
/// `--validate fail --auto-rebuild` rebuilds it.
fn stale_workspace(scratch: &Scratch) -> TempDir {
    let dir = workspace();
    for name in ["a", "b", "c"] {
        std::fs::write(
            dir.path().join("src").join(format!("{name}.rs")),
            format!("pub fn gone_{name}() {{}}\n"),
        )
        .expect("extra file");
    }
    assert!(scratch.run(dir.path(), &["index", "."]).status.success());
    for name in ["a", "b", "c"] {
        std::fs::remove_file(dir.path().join("src").join(format!("{name}.rs"))).expect("rm");
    }
    dir
}

/// `search --validate fail --auto-rebuild` checks the pattern before the
/// rebuild: a pattern the regex compiler refuses leaves the stale snapshot
/// as it was (it used to be rebuilt first). A good pattern is the control:
/// it rebuilds.
#[test]
fn search_checks_the_pattern_before_auto_rebuild() {
    let scratch = Scratch::new();
    let dir = stale_workspace(&scratch);
    let before = snapshot_digest(dir.path());
    let output = scratch.run(
        dir.path(),
        &["--validate", "fail", "--auto-rebuild", "search", "(", "."],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("Invalid regex pattern"),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot_digest(dir.path()), before, "nothing was rebuilt");

    let output = scratch.run(
        dir.path(),
        &[
            "--validate",
            "fail",
            "--auto-rebuild",
            "search",
            "alpha",
            ".",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_ne!(snapshot_digest(dir.path()), before, "the control rebuilds");
}

/// `query --validate fail --auto-rebuild` checks its arguments before the
/// rebuild: a malformed `--var`, a query that does not parse, and a `$name`
/// `--var` does not define each leave the stale snapshot as it was. A good
/// query is the control: it rebuilds.
#[test]
fn query_checks_its_arguments_before_auto_rebuild() {
    let scratch = Scratch::new();
    let dir = stale_workspace(&scratch);
    let before = snapshot_digest(dir.path());
    for (args, says) in [
        (
            vec!["query", "kind:function", ".", "--var", "bad"],
            "Invalid --var format",
        ),
        (vec!["query", "kind:function AND (", "."], "sqry query"),
        (
            vec!["query", "kind:function AND name:$x", ".", "--var", "y=1"],
            "Unresolved variable: $x",
        ),
    ] {
        let mut full = vec!["--validate", "fail", "--auto-rebuild"];
        full.extend(&args);
        let output = scratch.run(dir.path(), &full);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(says),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(
            snapshot_digest(dir.path()),
            before,
            "{args:?}: nothing was rebuilt"
        );
    }

    let output = scratch.run(
        dir.path(),
        &[
            "--validate",
            "fail",
            "--auto-rebuild",
            "query",
            "kind:function",
            ".",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_ne!(snapshot_digest(dir.path()), before, "the control rebuilds");
}

/// `--save-as` with a name the alias store refuses is refused before the
/// command runs: no result, and no history entry or alias store (the global
/// config directory is not created). A good name is the control: the
/// command runs and the alias is saved.
#[test]
fn save_as_checks_the_alias_name_before_running() {
    let scratch = Scratch::new();
    let dir = workspace();
    assert!(scratch.run(dir.path(), &["index", "."]).status.success());
    assert!(!scratch.config_dir().exists(), "precondition");
    for args in [
        vec!["search", "alpha", ".", "--save-as", "bad name!"],
        vec!["query", "kind:function", ".", "--save-as", "bad name!"],
    ] {
        let output = scratch.run(dir.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains("invalid --save-as alias name 'bad name!'"),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("alpha"),
            "{args:?}: the command did not run"
        );
        assert!(
            !scratch.config_dir().exists(),
            "{args:?}: no history entry or alias was written"
        );
    }

    let output = scratch.run(
        dir.path(),
        &["search", "alpha", ".", "--save-as", "good-name", "--global"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        scratch.config_dir().exists(),
        "the control writes the store"
    );
}

/// A `$name` that `--var` does not define is refused before the query
/// acquires a graph, so an unindexed path gets no index: the semantic, the
/// join and the pipeline forms each used to auto-build `.sqry` and then
/// fail variable resolution. Defining the variable is the control: the
/// query auto-indexes and runs.
#[test]
fn query_resolves_its_variables_before_auto_indexing() {
    let scratch = Scratch::new();
    for query in [
        "kind:function AND name:$x",
        "(kind:function AND name:$x) CALLS (kind:function)",
        "kind:function AND name:$x | count",
    ] {
        let dir = workspace();
        let output = scratch.run(dir.path(), &["query", query, ".", "--var", "y=1"]);
        assert!(!output.status.success(), "{query}");
        assert!(
            stderr(&output).contains("Variable resolution error: Unresolved variable: $x"),
            "{query}: {}",
            stderr(&output)
        );
        assert!(
            !dir.path().join(".sqry").exists(),
            "{query}: no index was built"
        );

        let output = scratch.run(dir.path(), &["query", query, ".", "--var", "x=alpha"]);
        assert!(output.status.success(), "{query}: {}", stderr(&output));
        assert!(
            dir.path().join(".sqry").exists(),
            "{query}: the control indexes"
        );
    }
}

/// `cache prune` without `--days` or `--size` is refused before the cache
/// root is created. A retention policy is the control.
#[test]
fn cache_prune_checks_its_policy_before_creating_the_cache() {
    let scratch = Scratch::new();
    let cwd = TempDir::new().expect("cwd");
    let output = scratch.run(cwd.path(), &["cache", "prune"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("At least one retention policy must be specified"),
        "{}",
        stderr(&output)
    );
    assert!(
        !scratch.cache_root().exists(),
        "the cache root was not created"
    );

    let output = scratch.run(cwd.path(), &["cache", "prune", "--days", "30"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(scratch.cache_root().exists(), "the control opens the cache");
}

/// `history clear` checks `--older` and `--confirm` before the history store
/// is opened: a malformed duration and a missing confirmation leave the
/// global config directory uncreated. `--confirm` is the control.
#[test]
fn history_clear_checks_its_arguments_before_opening_the_store() {
    let scratch = Scratch::new();
    let cwd = TempDir::new().expect("cwd");
    for (args, says) in [
        (
            vec!["history", "clear", "--older", "bogus"],
            "Invalid duration format 'bogus'",
        ),
        (
            vec!["history", "clear"],
            "Confirmation required to clear all history",
        ),
    ] {
        let output = scratch.run(cwd.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(says),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            !scratch.config_dir().exists(),
            "{args:?}: nothing was created"
        );
    }

    let output = scratch.run(cwd.path(), &["history", "clear", "--confirm"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(scratch.config_dir().exists(), "the control opens the store");
}

/// `alias rename` checks the new name, and `alias import` reads and parses
/// its file (with and without `--dry-run`), before the alias store is
/// opened: each refusal leaves the global config directory uncreated. A
/// good import is the control.
#[test]
fn alias_rename_and_import_check_their_input_before_opening_the_store() {
    let scratch = Scratch::new();
    let cwd = TempDir::new().expect("cwd");
    std::fs::write(cwd.path().join("array.json"), "[]").expect("array.json");
    for (args, says) in [
        (
            vec!["alias", "rename", "old", "bad name!"],
            "invalid alias name",
        ),
        (
            vec!["alias", "import", "/nonexistent/r7-aliases.json"],
            "Failed to read from /nonexistent/r7-aliases.json",
        ),
        (
            vec![
                "alias",
                "import",
                "/nonexistent/r7-aliases.json",
                "--dry-run",
            ],
            "Failed to read from /nonexistent/r7-aliases.json",
        ),
        (vec!["alias", "import", "array.json"], "a JSON array"),
    ] {
        let output = scratch.run(cwd.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(says),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            !scratch.config_dir().exists(),
            "{args:?}: nothing was created"
        );
    }

    let export = serde_json::json!({
        "version": 1,
        "exported_at": "2026-01-01T00:00:00Z",
        "aliases": {},
    });
    std::fs::write(cwd.path().join("ok.json"), export.to_string()).expect("ok.json");
    let output = scratch.run(cwd.path(), &["alias", "import", "ok.json", "--global"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(scratch.config_dir().exists(), "the control opens the store");
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

/// `diff` resolves its plugin selection, which refuses an override, before
/// it creates the two git worktrees. The order is observed through a
/// temporary directory that cannot be created (`TMPDIR` names a missing
/// directory): creating the worktrees fails there, so a refusal of the
/// override proves nothing was attempted first. Two refs naming one commit
/// refuse the override too (the shortcut used to drop it silently). The
/// control without an override reaches the worktrees.
#[test]
fn diff_refuses_a_plugin_override_before_creating_worktrees() {
    let scratch = Scratch::new();
    let repo = workspace();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "one"]);
    std::fs::write(
        repo.path().join("src").join("lib.rs"),
        "pub fn alpha() {}\npub fn gamma() {}\n",
    )
    .expect("edit");
    git(repo.path(), &["commit", "-q", "-am", "two"]);
    let missing_tmp = scratch.dir.path().join("no-such-tmp");
    let missing_tmp = missing_tmp.to_str().expect("utf-8");

    for refs in [["HEAD~1", "HEAD"], ["HEAD", "HEAD"]] {
        let output = scratch.run_with(
            repo.path(),
            &["diff", refs[0], refs[1]],
            &[("SQRY_INCLUDE_HIGH_COST", "1"), ("TMPDIR", missing_tmp)],
        );
        assert_eq!(output.status.code(), Some(1), "{refs:?}");
        let err = stderr(&output);
        assert!(
            err.contains("plugin-selection override refused: `sqry diff` builds both refs")
                && !err.contains("Failed to create git worktrees"),
            "{refs:?}: refused before the worktrees: {err}"
        );
    }

    let output = scratch.run_with(
        repo.path(),
        &["diff", "HEAD~1", "HEAD"],
        &[("TMPDIR", missing_tmp)],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Failed to create git worktrees"),
        "the control reaches the worktrees: {}",
        stderr(&output)
    );
}

/// The history `sqry` records for `cwd`'s store, as `--json history list`
/// gives it: `(command, args, success)` per entry, newest first.
fn history(scratch: &Scratch, cwd: &Path) -> Vec<(String, Vec<String>, bool)> {
    let output = scratch.run(cwd, &["--json", "history", "list"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let entries: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("history list is JSON");
    entries
        .as_array()
        .expect("an array")
        .iter()
        .map(|entry| {
            (
                entry["command"].as_str().expect("command").to_string(),
                entry["args"]
                    .as_array()
                    .expect("args")
                    .iter()
                    .map(|arg| arg.as_str().expect("arg").to_string())
                    .collect(),
                entry["success"].as_bool().expect("success"),
            )
        })
        .collect()
}

/// R7 notes: a search or query refused for its arguments under
/// `--validate fail --auto-rebuild` is answered as the same refusal is
/// without those flags. The flags make the arguments be checked before the
/// rebuild, and that early refusal used to skip the history entry and, for
/// a search, the "Search command failed:" prefix. Each refusal below gives
/// byte-identical stderr and exit code with and without the flags, and each
/// run records one failed entry. A revision search with `--ignore-case` is
/// one of them; the same search without `--ignore-case` is the control that
/// shows that flag is what refused it (it goes on to need a daemon).
#[test]
fn a_refused_search_or_query_reads_and_records_alike_under_auto_rebuild() {
    let scratch = Scratch::new();
    let dir = workspace();
    assert!(scratch.run(dir.path(), &["index", "."]).status.success());
    let flags = ["--validate", "fail", "--auto-rebuild"];
    for (args, says) in [
        (
            vec![
                "--ignore-case",
                "search",
                "alpha",
                ".",
                "--revision-ref",
                "HEAD",
            ],
            "Error: Search command failed: revision search does not support --ignore-case\n",
        ),
        (
            vec!["search", "al.*a", "."],
            "Error: Search command failed: query rejected: predicate `search /al.*a/`",
        ),
        (
            vec!["query", "kind:function", ".", "--var", "bad"],
            "Invalid --var format",
        ),
        (
            vec!["query", "name:$x", ".", "--var", "y=1"],
            "Variable resolution error: Unresolved variable: $x",
        ),
    ] {
        let plain = scratch.run(dir.path(), &args);
        let mut flagged_args = flags.to_vec();
        flagged_args.extend(&args);
        let before = history(&scratch, dir.path()).len();
        let flagged = scratch.run(dir.path(), &flagged_args);
        assert!(
            stderr(&plain).contains(says),
            "{args:?}: {}",
            stderr(&plain)
        );
        assert_eq!(
            plain.status.code(),
            flagged.status.code(),
            "{args:?}: exit code"
        );
        assert_ne!(plain.status.code(), Some(0), "{args:?}: refused");
        assert_eq!(stderr(&plain), stderr(&flagged), "{args:?}: stderr");
        let entries = history(&scratch, dir.path());
        assert_eq!(entries.len(), before + 1, "{args:?}: one entry recorded");
        let (command, recorded, success) = &entries[0];
        assert_eq!(
            command,
            args.iter().find(|a| !a.starts_with("--")).expect("command"),
            "{args:?}"
        );
        assert!(!success, "{args:?}: recorded as failed");
        assert!(
            recorded.iter().any(|arg| arg == "--auto-rebuild"),
            "{args:?}: the flagged run is the entry: {recorded:?}"
        );
    }

    let control = scratch.run(
        dir.path(),
        &["search", "alpha", ".", "--revision-ref", "HEAD"],
    );
    assert_eq!(control.status.code(), Some(1));
    let err = stderr(&control);
    assert!(
        err.contains("revision search requires sqryd") && !err.contains("--ignore-case"),
        "without --ignore-case the revision search gets past that refusal: {err}"
    );
}

/// `--text` evaluates the query as text, which resolves no `$name`, so a
/// `$name` `--var` does not define is not refused there (W4's check skips
/// it, as the executor does), with or without `--validate fail
/// --auto-rebuild`. The structural evaluation is the refused control.
#[test]
fn query_resolves_its_variables_only_where_the_executor_does() {
    let scratch = Scratch::new();
    let dir = workspace();
    assert!(scratch.run(dir.path(), &["index", "."]).status.success());
    for extra in [&[][..], &["--validate", "fail", "--auto-rebuild"][..]] {
        let mut args = extra.to_vec();
        args.extend(["--text", "query", "name:$x", ".", "--var", "y=1"]);
        let output = scratch.run(dir.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("0 text matches found")
                && !stderr(&output).contains("Unresolved variable"),
            "{args:?}: {}",
            stderr(&output)
        );

        let mut args = extra.to_vec();
        args.extend(["query", "name:$x", ".", "--var", "y=1"]);
        let output = scratch.run(dir.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&output).contains("Variable resolution error: Unresolved variable: $x"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

/// `--explain` prints the plan without reading the path, so a path that
/// does not exist is not refused there; `--validate fail --auto-rebuild`
/// keeps that (its early argument check skips the path under `--explain`,
/// as the query does). Without `--explain` the same path is refused, alike
/// with and without the flags.
#[test]
fn query_checks_the_path_only_where_the_query_reads_it() {
    let scratch = Scratch::new();
    let dir = workspace();
    let missing = dir.path().join("no-such-dir");
    let missing = missing.to_str().expect("utf-8");
    for extra in [&[][..], &["--validate", "fail", "--auto-rebuild"][..]] {
        let mut args = extra.to_vec();
        args.extend(["query", "kind:function", missing, "--explain"]);
        let output = scratch.run(dir.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("Query Plan:"),
            "{args:?}: the plan is printed: {}",
            stderr(&output)
        );

        let mut args = extra.to_vec();
        args.extend(["query", "kind:function", missing]);
        let output = scratch.run(dir.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&output).contains(&format!("invalid path {missing}: path does not exist")),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

/// `--json history clear` clears every entry without `--confirm` (a JSON
/// caller cannot answer the prompt), as before W6 moved the confirmation
/// check ahead of opening the store; the human form still needs it.
#[test]
fn history_clear_under_json_needs_no_confirmation() {
    let scratch = Scratch::new();
    let dir = workspace();
    assert!(scratch.run(dir.path(), &["index", "."]).status.success());
    assert!(
        scratch
            .run(dir.path(), &["search", "alpha", "."])
            .status
            .success()
    );
    assert_eq!(history(&scratch, dir.path()).len(), 1, "precondition");

    let output = scratch.run(dir.path(), &["history", "clear"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("Confirmation required to clear all history"));
    assert_eq!(history(&scratch, dir.path()).len(), 1, "nothing cleared");

    let output = scratch.run(dir.path(), &["--json", "history", "clear"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let cleared: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON answer");
    assert_eq!(cleared, serde_json::json!({ "cleared": 1 }));
    assert!(
        history(&scratch, dir.path()).is_empty(),
        "every entry cleared"
    );
}

/// A revision search or query reads a revision the daemon holds, never the
/// local index, so `--validate fail --auto-rebuild` neither validates nor
/// rebuilds the local index for it. A stale index used to be rebuilt first,
/// and the run then refused for want of the daemon (found by the S7 sweep
/// over a stale workspace). The refusal is now the daemon's alone and the
/// snapshot is unchanged, for every revision selector (`--revision-id`,
/// `--revision-ref`, `--revision-commit`, `--revision-tree` and
/// `--revision-dirty`; round 7 surfaces audit CL10 and CL13 found only
/// `--revision-ref` pinned); the same run without a revision flag is the
/// control that still rebuilds.
#[test]
fn a_revision_search_or_query_never_rebuilds_the_local_index() {
    let scratch = Scratch::new();
    let dir = stale_workspace(&scratch);
    let before = snapshot_digest(dir.path());
    let selectors: [&[&str]; 5] = [
        &["--revision-id", "r7-revision"],
        &["--revision-ref", "HEAD"],
        &["--revision-commit", "0123abc"],
        &["--revision-tree", "0123abc"],
        &["--revision-dirty"],
    ];
    for selector in selectors {
        for (command, says) in [
            (
                &["search", "alpha", "."][..],
                "revision search requires sqryd",
            ),
            (
                &["query", "kind:function", "."][..],
                "failed to connect to daemon",
            ),
        ] {
            for flags in [
                &["--validate", "fail", "--auto-rebuild"][..],
                &["--validate", "fail"][..],
            ] {
                let mut full = flags.to_vec();
                full.extend(command);
                full.extend(selector);
                let output = scratch.run(dir.path(), &full);
                assert_eq!(output.status.code(), Some(1), "{full:?}");
                assert!(
                    stderr(&output).contains(says),
                    "{full:?}: the daemon's refusal: {}",
                    stderr(&output)
                );
                assert_eq!(
                    snapshot_digest(dir.path()),
                    before,
                    "{full:?}: the local index was not rebuilt"
                );
            }
        }
    }

    let output = scratch.run(
        dir.path(),
        &[
            "--validate",
            "fail",
            "--auto-rebuild",
            "query",
            "kind:function",
            ".",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_ne!(snapshot_digest(dir.path()), before, "the control rebuilds");
}

/// Round 7 surfaces audit (CL9): the search's and the query's arguments are
/// checked ahead of validation only when `--auto-rebuild` lets validation
/// rebuild the index, the one write it could make first. Under
/// `--validate fail` alone validation writes nothing, so a stale index is
/// reported first, as it always was: exit 2 naming the stale index, no
/// history entry, the snapshot unchanged, even for a pattern or a `--var`
/// the command would refuse. With `--auto-rebuild` added the same arguments
/// are refused first (exit 1), the contrast that shows the check is keyed
/// on that flag.
#[test]
fn without_auto_rebuild_a_stale_index_is_reported_before_the_arguments() {
    let scratch = Scratch::new();
    let dir = stale_workspace(&scratch);
    let before = snapshot_digest(dir.path());
    for (args, refusal) in [
        (&["search", "(", "."][..], "Error: Search command failed: "),
        (
            &["query", "kind:function", ".", "--var", "bad"][..],
            "Invalid --var format",
        ),
    ] {
        let entries = history(&scratch, dir.path()).len();
        let mut full = vec!["--validate", "fail"];
        full.extend(args);
        let output = scratch.run(dir.path(), &full);
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{full:?}: {err}");
        assert!(
            err.contains("Error: Index is stale (75.0% of files missing).")
                && !err.contains(refusal),
            "{full:?}: the stale index is reported first: {err}"
        );
        assert_eq!(
            history(&scratch, dir.path()).len(),
            entries,
            "{full:?}: nothing was recorded"
        );
        assert_eq!(snapshot_digest(dir.path()), before, "{full:?}");

        let mut full = vec!["--validate", "fail", "--auto-rebuild"];
        full.extend(args);
        let output = scratch.run(dir.path(), &full);
        assert_eq!(output.status.code(), Some(1), "{full:?}");
        assert!(
            stderr(&output).contains(refusal),
            "{full:?}: {}",
            stderr(&output)
        );
        assert_eq!(snapshot_digest(dir.path()), before, "{full:?}: not rebuilt");
    }
}

/// The snapshot and the manifest of `root`'s index, each as its SHA-256 and
/// its modification time: two equal states mean neither file was rewritten.
fn index_state(root: &Path) -> [(String, std::time::SystemTime); 2] {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    [storage.snapshot_path(), storage.manifest_path()].map(|path| {
        let bytes = std::fs::read(path).expect("index file");
        let modified = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .expect("mtime");
        (hex::encode(Sha256::digest(bytes)), modified)
    })
}

/// One refused form for [`every_query_form_checks_its_arguments_before_auto_rebuild`]:
/// its arguments, the refusal, its exit code, and the control that rebuilds.
struct FormCase {
    form: &'static str,
    refused: &'static [&'static str],
    says: &'static str,
    code: i32,
    control: &'static [&'static str],
}

/// R8: every form `sqry query` and `sqry search` dispatch to checks its
/// arguments before `--validate fail --auto-rebuild` rebuilds a stale
/// index. The repair before this one checked only the structural query, so
/// a pipeline or a join with an unknown field, a `--text` query its regex
/// engine refuses, `--session` beside `--text`, and a fuzzy or JSON-stream
/// search with an unknown `--fuzzy-algorithm` each rebuilt the index and
/// then refused. Each case: the snapshot and the manifest keep their bytes
/// and modification times, the exit code and the refusal are the ones the
/// same arguments get without the flags, and the form's valid control
/// rebuilds the index.
#[test]
fn every_query_form_checks_its_arguments_before_auto_rebuild() {
    let cases = [
        FormCase {
            form: "structural",
            refused: &["query", "bogusfield:x", "."],
            says: "Unknown field 'bogusfield'",
            code: 2,
            control: &["query", "kind:function", "."],
        },
        FormCase {
            form: "pipeline",
            refused: &["query", "bogusfield:x | count", "."],
            says: "Unknown field 'bogusfield'",
            code: 2,
            control: &["query", "kind:function | count", "."],
        },
        FormCase {
            form: "join",
            refused: &["query", "(bogusfield:x) CALLS (kind:function)", "."],
            says: "Unknown field 'bogusfield'",
            code: 2,
            control: &["query", "(kind:function) CALLS (kind:function)", "."],
        },
        FormCase {
            form: "explain",
            refused: &["query", "--explain", "bogusfield:x", "."],
            says: "Unknown field 'bogusfield'",
            code: 2,
            control: &["query", "--explain", "kind:function", "."],
        },
        FormCase {
            form: "session",
            refused: &["query", "--session", "bogusfield:x", "."],
            says: "Unknown field 'bogusfield'",
            code: 2,
            control: &["query", "--session", "kind:function", "."],
        },
        FormCase {
            form: "session beside --text",
            refused: &["--text", "query", "--session", "kind:function", "."],
            says: "--session is only available for semantic queries (remove --text)",
            code: 1,
            control: &["query", "--session", "kind:function", "."],
        },
        FormCase {
            form: "text",
            refused: &["--text", "query", "(", "."],
            says: "Text search failed",
            code: 1,
            control: &["--text", "query", "alpha", "."],
        },
        FormCase {
            form: "fuzzy search",
            refused: &[
                "--fuzzy",
                "--fuzzy-algorithm",
                "bogus",
                "search",
                "alpha",
                ".",
            ],
            says: "Unknown fuzzy algorithm 'bogus'",
            code: 1,
            control: &["--fuzzy", "search", "alpha", "."],
        },
        FormCase {
            form: "JSON-stream search",
            refused: &[
                "--fuzzy",
                "--json-stream",
                "--fuzzy-algorithm",
                "bogus",
                "search",
                "alpha",
                ".",
            ],
            says: "Unknown fuzzy algorithm 'bogus'",
            code: 1,
            control: &["--fuzzy", "--json-stream", "search", "alpha", "."],
        },
    ];
    let flags = ["--validate", "fail", "--auto-rebuild"];
    let scratch = Scratch::new();
    // Every case runs and every miss is collected, so one run names each
    // form that fails rather than stopping at the first.
    let mut misses = Vec::new();
    for case in &cases {
        let form = case.form;
        let dir = stale_workspace(&scratch);
        let before = index_state(dir.path());

        let mut flagged = flags.to_vec();
        flagged.extend(case.refused);
        let output = scratch.run(dir.path(), &flagged);
        let err = stderr(&output);
        if output.status.code() != Some(case.code) || !err.contains(case.says) {
            misses.push(format!(
                "{form}: refused with {:?}, not {} saying {:?}",
                output.status.code(),
                case.code,
                case.says
            ));
        }
        if err.contains("Rebuilding because --auto-rebuild is set")
            || index_state(dir.path()) != before
        {
            misses.push(format!("{form}: rebuilt the index before refusing"));
            continue;
        }

        let plain = scratch.run(dir.path(), case.refused);
        if plain.status.code() != Some(case.code) || !stderr(&plain).contains(case.says) {
            misses.push(format!(
                "{form}: without the flags it is not the same refusal: {:?} {}",
                plain.status.code(),
                stderr(&plain)
            ));
        }
        assert_eq!(index_state(dir.path()), before, "{form}: still untouched");

        let mut control = flags.to_vec();
        control.extend(case.control);
        let output = scratch.run(dir.path(), &control);
        assert!(output.status.success(), "{form}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("Rebuilding because --auto-rebuild is set"),
            "{form}: {}",
            stderr(&output)
        );
        assert_ne!(
            index_state(dir.path())[0].0,
            before[0].0,
            "{form}: the control rebuilds the snapshot"
        );
    }
    assert!(misses.is_empty(), "{misses:#?}");
}

/// R8: a saved query runs as the query it expands to, so an aliased
/// pipeline with an unknown field is refused before `--validate fail
/// --auto-rebuild` rebuilds the stale index (it rebuilt and then refused).
/// An aliased valid pipeline is the control: it rebuilds.
#[test]
fn an_aliased_query_checks_its_arguments_before_auto_rebuild() {
    let scratch = Scratch::new();
    let dir = stale_workspace(&scratch);
    let alias = |query: &str| {
        serde_json::json!({
            "command": "query",
            "args": [query],
            "created": "2026-01-01T00:00:00Z",
            "description": null,
        })
    };
    let export = serde_json::json!({
        "version": 1,
        "exported_at": "2026-01-01T00:00:00Z",
        "aliases": {
            "r8-bad-pipeline": alias("bogusfield:x | count"),
            "r8-good-pipeline": alias("kind:function | count"),
        },
    });
    let file = scratch.dir.path().join("r8-aliases.json");
    std::fs::write(&file, export.to_string()).expect("aliases");
    let output = scratch.run(
        dir.path(),
        &["alias", "import", file.to_str().expect("utf-8"), "--global"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let before = index_state(dir.path());

    let output = scratch.run(
        dir.path(),
        &["--validate", "fail", "--auto-rebuild", "@r8-bad-pipeline"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("Unknown field 'bogusfield'"),
        "{}",
        stderr(&output)
    );
    assert_eq!(index_state(dir.path()), before, "nothing was rebuilt");

    let output = scratch.run(
        dir.path(),
        &["--validate", "fail", "--auto-rebuild", "@r8-good-pipeline"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_ne!(
        index_state(dir.path())[0].0,
        before[0].0,
        "the control rebuilds"
    );
}

/// R8 regression guard (passes before and after the repair): a pipeline
/// or a join on an unindexed path parses and validates its query before
/// its executor's own graph load may auto-build an index, so an unknown
/// field builds nothing. A valid pipeline is the control: it auto-indexes.
#[test]
fn a_pipeline_or_join_validates_before_auto_indexing() {
    let scratch = Scratch::new();
    for query in [
        "bogusfield:x | count",
        "(bogusfield:x) CALLS (kind:function)",
    ] {
        let dir = workspace();
        let output = scratch.run(dir.path(), &["query", query, "."]);
        assert_eq!(output.status.code(), Some(2), "{query}");
        assert!(
            stderr(&output).contains("Unknown field 'bogusfield'"),
            "{query}: {}",
            stderr(&output)
        );
        assert!(
            !dir.path().join(".sqry").exists(),
            "{query}: no index was built"
        );
    }
    let dir = workspace();
    let output = scratch.run(dir.path(), &["query", "kind:function | count", "."]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.path().join(".sqry").exists(), "the control indexes");
}
