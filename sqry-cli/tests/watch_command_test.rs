// Windows debug builds overflow the default 1MB stack with Cli::parse_from
// (deep clap subcommand tree causes STATUS_STACK_OVERFLOW).
#![cfg(not(target_os = "windows"))]

//! Watch command CLI integration tests
//!
//! Tests the `sqry watch` command's CLI interface end-to-end.
//!
//! Note: Watch mode runs indefinitely, so tests use short timeouts to verify
//! the command starts correctly rather than testing the full watch loop.
//! The watch mode functionality itself is tested in sqry-core/src/symbols/watch_mode.rs

mod common;
use clap::{CommandFactory, Parser};
use sqry_cli::args::{Cli, Command};
use sqry_cli::commands::watch;
use sqry_core::test_support::verbosity;
use std::fs;
use std::sync::Once;
use tempfile::TempDir;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Helper: Create test project with files
fn create_test_project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        let file_path = dir.path().join(path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, content).unwrap();
    }
    dir
}

// ============================================================================
// Basic Functionality Tests
// ============================================================================

#[test]
fn test_watch_help() {
    init_logging();
    log::info!("Testing 'sqry watch --help' shows usage information");

    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    cmd.find_subcommand_mut("watch")
        .expect("watch subcommand exists")
        .write_long_help(&mut buf)
        .expect("render help");
    let help = String::from_utf8(buf).expect("utf8 help");

    assert!(help.contains("Watch directory"));
    assert!(help.contains("--debounce"));
    assert!(help.contains("--stats"));
    assert!(help.contains("--build"));

    log::info!("✓ Watch help displays correctly");
}

#[test]
fn test_watch_requires_index() {
    init_logging();
    log::info!("Testing watch command fails without existing index");

    let project = create_test_project(&[("test.rs", "fn main() {}")]);
    let path_str = project.path().to_string_lossy().to_string();
    let cli = Cli::parse_from(["sqry", "watch", &path_str]);

    // Watch without index should fail
    let err = watch::execute(&cli, Some(path_str), None, false, false)
        .expect_err("watch should fail when index is missing");
    let msg = err.to_string();
    assert!(
        msg.contains("No index found")
            || msg.contains("Index load failed")
            || msg.contains("Error"),
        "unexpected error: {msg}"
    );

    log::info!("✓ Watch correctly requires existing index");
}

#[test]
fn test_watch_nonexistent_directory() {
    init_logging();
    log::info!("Testing watch command with nonexistent directory");

    let cli = Cli::parse_from(["sqry", "watch", "/nonexistent/path/to/nowhere"]);
    let err = watch::execute(
        &cli,
        Some("/nonexistent/path/to/nowhere".to_string()),
        None,
        false,
        false,
    )
    .expect_err("watch should fail for nonexistent directory");
    let msg = err.to_string();
    assert!(
        msg.contains("does not exist") || msg.contains("No such"),
        "unexpected error: {msg}"
    );

    log::info!("✓ Watch handles nonexistent directory correctly");
}

#[test]
fn test_watch_zero_debounce() {
    init_logging();
    log::info!("Testing watch command with zero debounce value");

    let cli = Cli::parse_from(["sqry", "watch", "--debounce", "0"]);

    match cli.command.expect("watch command parsed") {
        Command::Watch {
            debounce,
            build,
            stats,
            path,
        } => {
            assert_eq!(debounce, Some(0));
            assert!(!build);
            assert!(!stats);
            assert!(path.is_none());
        }
        other => panic!("expected watch command, got {other:?}"),
    }

    log::info!("✓ Watch handles zero debounce value");
}

// ============================================================================
// Path Argument Tests
// ============================================================================

#[test]
fn test_watch_with_explicit_path() {
    init_logging();
    log::info!("Testing watch command with explicit path argument");

    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    let path_str = project.path().to_str().unwrap();
    let cli = Cli::parse_from(["sqry", "watch", path_str]);

    match cli.command.expect("watch command parsed") {
        Command::Watch { path, .. } => {
            assert_eq!(path.as_deref(), Some(path_str));
        }
        other => panic!("expected watch command, got {other:?}"),
    }

    log::info!("✓ Watch accepts explicit path argument");
}

#[test]
fn test_watch_current_directory_default() {
    init_logging();
    log::info!("Testing watch uses current directory by default");

    let cli = Cli::parse_from(["sqry", "watch"]);
    match cli.command.expect("watch command parsed") {
        Command::Watch { path, .. } => {
            assert!(path.is_none(), "path should default to None for watch");
        }
        other => panic!("expected watch command, got {other:?}"),
    }

    log::info!("✓ Watch defaults to current directory");
}

// ============================================================================
// Flag Validation Tests
// ============================================================================

#[test]
fn test_watch_large_debounce_value() {
    init_logging();
    log::info!("Testing watch command with large debounce value");

    let cli = Cli::parse_from(["sqry", "watch", "--debounce", "10000"]);
    match cli.command.expect("watch command parsed") {
        Command::Watch {
            debounce,
            build,
            stats,
            path,
        } => {
            assert_eq!(debounce, Some(10_000));
            assert!(!build);
            assert!(!stats);
            assert!(path.is_none());
        }
        other => panic!("expected watch command, got {other:?}"),
    }

    log::info!("✓ Watch handles large debounce values");
}

#[test]
fn test_watch_build_and_stats_together() {
    init_logging();
    log::info!("Testing watch with --build and --stats flags together");

    let cli = Cli::parse_from(["sqry", "watch", "--build", "--stats", "--debounce", "250"]);
    match cli.command.expect("watch command parsed") {
        Command::Watch {
            build,
            stats,
            debounce,
            path,
        } => {
            assert!(build);
            assert!(stats);
            assert_eq!(debounce, Some(250));
            assert!(path.is_none());
        }
        other => panic!("expected watch command, got {other:?}"),
    }

    log::info!("✓ Watch accepts --build and --stats together");
}
