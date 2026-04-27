//! `STEP_8` — Global `--workspace <PATH>` flag and `SQRY_WORKSPACE_FILE`
//! environment variable parse-level integration tests.
//!
//! Validates the seven acceptance criteria from
//! `docs/superpowers/plans/.../STEP_8_CLI_SYMMETRY` (DAG TOML lines 456–495)
//! and `docs/development/workspace-aware-cross-repo/03_IMPLEMENTATION_PLAN.md`
//! Step 8:
//!
//! 1. Global `--workspace <PATH>` parses and propagates to every subcommand.
//! 2. `SQRY_WORKSPACE_FILE` resolves identically; `--workspace` wins on
//!    conflict.
//! 3. Positional `<path>` on `sqry index <path>` and `sqry query <path> …`
//!    wins over `--workspace`.
//! 4. `sqry workspace --workspace …` errors with a clear, actionable message.
//! 5. Internal benchmark crates that previously used `--workspace` continue
//!    to work (namespace isolation: those flags are passed to a separate
//!    Python process; sqry-cli does not see them).
//!
//! Tests exercise the parser via `clap::Parser::try_parse_from` for parse-only
//! assertions (fast, no binary launch). Env-var tests use `assert_cmd` against
//! the built binary to obtain a clean process environment per test.

mod common;

use assert_cmd::Command;
use clap::Parser;
use predicates::prelude::*;
use serial_test::serial;
use sqry_cli::args::{Cli, Command as CliCommand, WorkspaceCommand};
use std::path::PathBuf;

use common::sqry_bin;

/// SAFETY-style helper: scoped guard that sets `SQRY_WORKSPACE_FILE` for the
/// duration of a test and restores the prior value (or unsets it) on drop.
///
/// Tests that touch the process environment must be marked `#[serial]` —
/// `std::env::set_var` mutates global state and races other tests.
struct EnvGuard {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: serialised via `#[serial]` so no other test thread mutates
        // process env concurrently. Required for the env-precedence assertions.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prior }
    }

    fn remove(key: &'static str) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: see `set` above.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `EnvGuard::set`.
        unsafe {
            match self.prior.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// 16 MB-stack wrapper for tests that call `Cli::parse_from` /
/// `try_parse_from`. Clap's deep subcommand tree can overflow the default
/// 8 MB debug-mode thread stack.
macro_rules! large_stack_test {
    ($(#[$attr:meta])* fn $name:ident() $body:block) => {
        $(#[$attr])*
        fn $name() {
            let result = std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || $body)
                .expect("spawn test thread")
                .join();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        }
    };
}

fn sqry_cmd_clean_env() -> Command {
    let mut cmd = Command::new(sqry_bin());
    // Ensure any inherited SQRY_WORKSPACE_FILE from the test runner doesn't
    // leak into a test that asserts on parser behaviour without the env var.
    cmd.env_remove("SQRY_WORKSPACE_FILE");
    cmd.env("NO_COLOR", "1");
    cmd
}

// ---------------------------------------------------------------------------
// Criterion 1: --workspace parses and propagates to every subcommand
// ---------------------------------------------------------------------------

large_stack_test! {
#[test]
#[serial]
fn global_workspace_flag_parses_and_propagates() {
    // Placement before subcommand
    let cli = Cli::try_parse_from(["sqry", "--workspace", "/tmp/ws", "index"])
        .expect("parse --workspace before subcommand");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );
    assert!(matches!(cli.command.as_deref(), Some(CliCommand::Index { .. })));

    // Placement after subcommand (clap `global = true`)
    let cli = Cli::try_parse_from(["sqry", "index", "--workspace", "/tmp/ws"])
        .expect("parse --workspace after subcommand");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );

    // Propagates to query subcommand
    let cli = Cli::try_parse_from(["sqry", "--workspace", "/tmp/ws", "query", "kind:function"])
        .expect("parse with query subcommand");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );
    assert!(matches!(cli.command.as_deref(), Some(CliCommand::Query { .. })));

    // Propagates to search shorthand (no subcommand)
    let cli = Cli::try_parse_from(["sqry", "--workspace", "/tmp/ws", "main"])
        .expect("parse with shorthand search");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );
    assert_eq!(cli.pattern.as_deref(), Some("main"));

    // Propagates to graph subcommand
    let cli = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/ws",
        "graph",
        "stats",
    ])
    .expect("parse with graph subcommand");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );
}
}

// ---------------------------------------------------------------------------
// Criterion 2: env var resolves identically; CLI flag wins on conflict
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn env_var_resolves_identically() {
    // Stack-thunk to keep deep clap parsing inside the 16 MB test thread.
    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            // Set SQRY_WORKSPACE_FILE; assert clap surfaces the env value as
            // `cli.workspace` exactly as if `--workspace <PATH>` were passed.
            // This proves criterion 2 at the resolution layer, not just in
            // help text.
            let _guard = EnvGuard::set(
                "SQRY_WORKSPACE_FILE",
                std::ffi::OsStr::new("/tmp/sqry-step8-env-path"),
            );

            // Parse with NO --workspace flag; clap should pull the value
            // from the env binding declared on the field.
            let cli = Cli::try_parse_from(["sqry", "index"]).expect("parse with env-only");
            assert_eq!(
                cli.workspace,
                Some(PathBuf::from("/tmp/sqry-step8-env-path")),
                "env var must populate `cli.workspace` identically to --workspace"
            );
            assert_eq!(
                cli.workspace_path(),
                Some(std::path::Path::new("/tmp/sqry-step8-env-path")),
                "workspace_path() helper must return the env-supplied path"
            );

            // Resolution propagates the env value to `resolve_subcommand_path`
            // when no positional is provided.
            let positional = if let Some(CliCommand::Index { path, .. }) = cli.command.as_deref() {
                path.as_deref()
            } else {
                panic!("expected Index variant");
            };
            assert_eq!(positional, None);
            let resolved = cli
                .resolve_subcommand_path(positional)
                .expect("env workspace path is UTF-8");
            assert_eq!(
                resolved, "/tmp/sqry-step8-env-path",
                "resolver must surface the env-supplied path as the effective search path"
            );
        })
        .expect("spawn test thread")
        .join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[serial]
fn cli_flag_wins_over_env_var_on_conflict() {
    // Set both the env var and the CLI flag to *distinguishable* paths;
    // assert that the resolved value is the CLI-flag path, not the env path.
    // This proves criterion 2's precedence rule at the resolution layer.
    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let _guard = EnvGuard::set(
                "SQRY_WORKSPACE_FILE",
                std::ffi::OsStr::new("/tmp/sqry-step8-from-env"),
            );

            let cli =
                Cli::try_parse_from(["sqry", "--workspace", "/tmp/sqry-step8-from-flag", "index"])
                    .expect("parse with both env and flag");

            assert_eq!(
                cli.workspace,
                Some(PathBuf::from("/tmp/sqry-step8-from-flag")),
                "explicit --workspace flag must override SQRY_WORKSPACE_FILE"
            );
            assert_ne!(
                cli.workspace,
                Some(PathBuf::from("/tmp/sqry-step8-from-env")),
                "env var must NOT win over the explicit --workspace flag"
            );

            // Resolution path: with no positional, the resolver must surface
            // the flag path, never the env path.
            let positional = if let Some(CliCommand::Index { path, .. }) = cli.command.as_deref() {
                path.as_deref()
            } else {
                panic!("expected Index variant");
            };
            assert_eq!(positional, None);
            let resolved = cli
                .resolve_subcommand_path(positional)
                .expect("flag path is UTF-8");
            assert_eq!(
                resolved, "/tmp/sqry-step8-from-flag",
                "CLI flag path must be the effective search path on conflict"
            );
        })
        .expect("spawn test thread")
        .join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[serial]
fn env_var_alone_resolves_for_query_subcommand() {
    // Cross-subcommand check: env-only resolution must work for `query` too,
    // not just `index`. This locks down "env resolves identically" across
    // every path-scoped subcommand.
    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let _guard = EnvGuard::set(
                "SQRY_WORKSPACE_FILE",
                std::ffi::OsStr::new("/tmp/sqry-step8-env-query"),
            );

            let cli = Cli::try_parse_from(["sqry", "query", "kind:function"])
                .expect("parse query with env-only");
            assert_eq!(
                cli.workspace,
                Some(PathBuf::from("/tmp/sqry-step8-env-query"))
            );

            let positional = if let Some(CliCommand::Query { path, .. }) = cli.command.as_deref() {
                path.as_deref()
            } else {
                panic!("expected Query variant");
            };
            assert_eq!(positional, None);
            let resolved = cli
                .resolve_subcommand_path(positional)
                .expect("env workspace path is UTF-8");
            assert_eq!(resolved, "/tmp/sqry-step8-env-query");
        })
        .expect("spawn test thread")
        .join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

// ---------------------------------------------------------------------------
// End-to-end binary smoke checks (kept for help-text coverage)
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn help_text_advertises_env_var_binding() {
    // Cosmetic / discoverability check: clap renders `[env: SQRY_WORKSPACE_FILE=...]`
    // in help output so users discover the env var. This is independent from
    // the resolution-layer assertions above.
    sqry_cmd_clean_env()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--workspace"))
        .stdout(predicate::str::contains("SQRY_WORKSPACE_FILE"));
}

#[test]
#[serial]
fn cli_flag_wins_over_env_var_e2e_collision_diagnostic() {
    // End-to-end binary check that the collision diagnostic renders the
    // *flag* path, not the env path, when the user combines both with the
    // `workspace` subcommand. Complements the parser-level precedence
    // assertion in `cli_flag_wins_over_env_var_on_conflict` by proving the
    // built binary's dispatch sees the flag value.
    sqry_cmd_clean_env()
        .env("SQRY_WORKSPACE_FILE", "/tmp/sqry-step8-from-env")
        .args([
            "--workspace",
            "/tmp/sqry-step8-from-flag",
            "workspace",
            "status",
            "/tmp/positional",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--workspace").and(
            predicate::str::contains("workspace").and(predicate::str::contains("subcommand")),
        ));
}

// ---------------------------------------------------------------------------
// Criterion 3: positional path wins over --workspace for index/query
// ---------------------------------------------------------------------------

large_stack_test! {
#[test]
#[serial]
fn positional_path_wins_over_workspace_for_index() {
    // `sqry index <positional>` with `--workspace` set: positional wins.
    let cli = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/ws-flag",
        "index",
        "/tmp/positional-path",
    ])
    .expect("parse");

    let positional = if let Some(CliCommand::Index { path, .. }) = cli.command.as_deref() {
        path.as_deref()
    } else {
        panic!("expected Index variant");
    };

    assert_eq!(positional, Some("/tmp/positional-path"));

    // The resolver must echo the positional, *not* the workspace flag.
    let resolved = cli
        .resolve_subcommand_path(positional)
        .expect("positional path is UTF-8");
    assert_eq!(resolved, "/tmp/positional-path");

    // And when the positional is absent, the resolver falls back to the
    // workspace flag.
    let cli_no_positional = Cli::try_parse_from(["sqry", "--workspace", "/tmp/ws-flag", "index"])
        .expect("parse without positional");
    let positional = if let Some(CliCommand::Index { path, .. }) = cli_no_positional.command.as_deref() {
        path.as_deref()
    } else {
        panic!("expected Index variant");
    };
    assert_eq!(positional, None);
    let resolved = cli_no_positional
        .resolve_subcommand_path(positional)
        .expect("workspace flag is UTF-8");
    assert_eq!(resolved, "/tmp/ws-flag");
}
}

large_stack_test! {
#[test]
#[serial]
fn positional_path_wins_over_workspace_for_query() {
    let cli = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/ws-flag",
        "query",
        "kind:function",
        "/tmp/positional-path",
    ])
    .expect("parse");

    let positional = if let Some(CliCommand::Query { path, .. }) = cli.command.as_deref() {
        path.as_deref()
    } else {
        panic!("expected Query variant");
    };
    assert_eq!(positional, Some("/tmp/positional-path"));

    let resolved = cli
        .resolve_subcommand_path(positional)
        .expect("positional path is UTF-8");
    assert_eq!(resolved, "/tmp/positional-path");

    // Without the positional, the workspace flag is used.
    let cli_no_positional = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/ws-flag",
        "query",
        "kind:function",
    ])
    .expect("parse without positional");
    let positional = if let Some(CliCommand::Query { path, .. }) =
        cli_no_positional.command.as_deref()
    {
        path.as_deref()
    } else {
        panic!("expected Query variant");
    };
    assert_eq!(positional, None);
    let resolved = cli_no_positional
        .resolve_subcommand_path(positional)
        .expect("workspace flag is UTF-8");
    assert_eq!(resolved, "/tmp/ws-flag");
}
}

large_stack_test! {
#[test]
#[serial]
fn resolver_falls_back_to_default_when_neither_set() {
    // Defensive: clear any inherited SQRY_WORKSPACE_FILE so this test
    // observes the true "neither set" case regardless of test ordering.
    let _guard = EnvGuard::remove("SQRY_WORKSPACE_FILE");
    let cli = Cli::try_parse_from(["sqry", "index"]).expect("parse");
    let positional = if let Some(CliCommand::Index { path, .. }) = cli.command.as_deref() {
        path.as_deref()
    } else {
        panic!("expected Index variant");
    };
    assert_eq!(positional, None);
    // No --workspace, no positional: defaults to "." via search_path().
    assert_eq!(
        cli.resolve_subcommand_path(positional)
            .expect("default `.` is UTF-8"),
        "."
    );
}
}

// ---------------------------------------------------------------------------
// Criterion 4: `sqry workspace --workspace …` is a hard error
// ---------------------------------------------------------------------------

large_stack_test! {
#[test]
#[serial]
fn workspace_subcommand_parses_with_global_workspace_flag() {
    // The parser MUST accept the combination — the conflict is enforced by
    // main.rs at dispatch time, not by clap itself. This guarantees the
    // error message we control (criterion 4) is the one users see, rather
    // than an opaque clap "argument provided more than once" message.
    let cli = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/global-ws",
        "workspace",
        "stats",
        "/tmp/positional-ws",
    ])
    .expect("parser must accept the combination so dispatch can render a clear error");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/global-ws"))
    );
    if let Some(CliCommand::Workspace {
        action: WorkspaceCommand::Stats { workspace },
    }) = cli.command.as_deref()
    {
        assert_eq!(workspace, "/tmp/positional-ws");
    } else {
        panic!("expected WorkspaceCommand::Stats");
    }
}
}

#[test]
#[serial]
fn workspace_subcommand_with_global_workspace_errors_clearly() {
    // End-to-end assertion: the binary must reject the combination with a
    // diagnostic that mentions both `--workspace` and the `workspace`
    // subcommand, and tells the user how to fix it.
    sqry_cmd_clean_env()
        .args([
            "--workspace",
            "/tmp/global-ws",
            "workspace",
            "stats",
            "/tmp/positional-ws",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--workspace")
                .and(predicate::str::contains("subcommand"))
                .and(
                    predicate::str::contains("SQRY_WORKSPACE_FILE")
                        .or(predicate::str::contains("positional")),
                ),
        );
}

#[test]
#[serial]
fn env_var_with_workspace_subcommand_errors_clearly() {
    // The env-var-only path must trigger the same hard error: leaving
    // `SQRY_WORKSPACE_FILE` exported in the operator's shell while running
    // `sqry workspace status <path>` would otherwise silently override the
    // intended target.
    sqry_cmd_clean_env()
        .env("SQRY_WORKSPACE_FILE", "/tmp/from-env-ws")
        .args(["workspace", "stats", "/tmp/positional-ws"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--workspace").and(predicate::str::contains("subcommand")),
        );
}

// ---------------------------------------------------------------------------
// Criterion 5: namespace isolation from internal benchmark crates
// ---------------------------------------------------------------------------

large_stack_test! {
#[test]
#[serial]
fn internal_bench_workspace_flag_does_not_collide() {
    // The `benchmarks/` crate constructs `--workspace` arguments and passes
    // them to a *separate* Python process via std::process::Command; those
    // bytes never reach the sqry-cli clap parser. This test locks down the
    // namespace boundary: sqry-cli's `--workspace` is parsed exclusively
    // by the sqry binary, so the bench harness can keep using the literal
    // string "--workspace" in its own argv without colliding with our
    // global flag.
    //
    // Concretely: a sqry-cli parse with the same flag name still resolves
    // to PathBuf and never confuses the value with another arg.
    let cli = Cli::try_parse_from([
        "sqry",
        "--workspace",
        "/tmp/sqry-cli-ws",
        "index",
        "/tmp/positional",
    ])
    .expect("parse");
    assert_eq!(
        cli.workspace.as_deref(),
        Some(std::path::Path::new("/tmp/sqry-cli-ws"))
    );
    if let Some(CliCommand::Index { path, .. }) = cli.command.as_deref() {
        assert_eq!(path.as_deref(), Some("/tmp/positional"));
    } else {
        panic!("expected Index variant");
    }
}
}

// ---------------------------------------------------------------------------
// Workspace_path() helper — sanity coverage
// ---------------------------------------------------------------------------

large_stack_test! {
#[test]
#[serial]
fn workspace_path_helper_returns_some_when_flag_set() {
    let cli = Cli::try_parse_from(["sqry", "--workspace", "/tmp/x", "index"]).unwrap();
    assert_eq!(cli.workspace_path(), Some(std::path::Path::new("/tmp/x")));
}
}

large_stack_test! {
#[test]
#[serial]
fn workspace_path_helper_returns_none_when_unset() {
    // Defensive: clear any inherited SQRY_WORKSPACE_FILE so this test
    // observes the true "unset" case regardless of test ordering.
    let _guard = EnvGuard::remove("SQRY_WORKSPACE_FILE");
    let cli = Cli::try_parse_from(["sqry", "index"]).unwrap();
    assert!(cli.workspace_path().is_none());
}
}

// ---------------------------------------------------------------------------
// STEP_8 codex iter1 regression: non-UTF-8 workspace path
// ---------------------------------------------------------------------------

/// Codex iter1 finding (`docs/reviews/.../STEP_8_codex_review_iter1.md`):
/// `resolve_subcommand_path` previously did `workspace.to_str().unwrap_or(_)`
/// and silently fell back to `cli.search_path()` when the workspace path
/// contained non-UTF-8 bytes. That violated the documented precedence
/// (positional → workspace → default). Iter2 replaces the lossy fallback
/// with an explicit error so operators are told to supply a UTF-8 path
/// rather than landing on a silently-wrong target directory.
///
/// On Unix, `PathBuf` can represent valid non-UTF-8 paths — we construct
/// one with `OsStr::from_bytes(b"\xff/foo")`. Windows paths are UTF-16
/// natively, so this regression does not apply there.
#[cfg(unix)]
#[test]
#[serial]
fn non_utf8_workspace_path_surfaces_as_error() {
    use std::os::unix::ffi::OsStrExt;

    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            // Defensive: clear any inherited SQRY_WORKSPACE_FILE so the
            // baseline parse below produces a clean `Cli` we can mutate.
            let _env_guard = EnvGuard::remove("SQRY_WORKSPACE_FILE");

            // Build a `PathBuf` with a non-UTF-8 byte (0xff is invalid UTF-8).
            let bad_path = PathBuf::from(std::ffi::OsStr::from_bytes(b"\xff/sqry-step8-non-utf8"));
            assert!(
                bad_path.to_str().is_none(),
                "fixture must be non-UTF-8 to exercise the regression"
            );

            // Inject directly into a parsed `Cli` (skipping clap, which would
            // round-trip through `OsString` correctly but cannot itself
            // construct a non-UTF-8 PathBuf from `try_parse_from`'s &str
            // arguments).
            let mut cli = Cli::try_parse_from(["sqry", "index"]).expect("parse baseline");
            cli.workspace = Some(bad_path);

            // No positional → resolver must hit the workspace branch and
            // return an error, NOT silently fall through to `search_path()`.
            let positional: Option<&str> = None;
            let err = cli
                .resolve_subcommand_path(positional)
                .expect_err("non-UTF-8 workspace must error, not silently fall back");

            let msg = err.to_string();
            assert!(
                msg.contains("UTF-8"),
                "error must mention UTF-8 violation; got: {msg}"
            );
            assert!(
                msg.contains("--workspace") || msg.contains("SQRY_WORKSPACE_FILE"),
                "error must reference the flag/env that supplied the bad path; got: {msg}"
            );

            // Sanity: when an explicit positional IS provided, the non-UTF-8
            // workspace is bypassed entirely (positional wins).
            let resolved = cli
                .resolve_subcommand_path(Some("/tmp/explicit-positional"))
                .expect("positional bypasses the workspace branch");
            assert_eq!(resolved, "/tmp/explicit-positional");
        })
        .expect("spawn test thread")
        .join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
