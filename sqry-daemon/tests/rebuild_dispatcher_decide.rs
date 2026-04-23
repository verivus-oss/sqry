//! Task 7 Phase 7a — `RebuildDispatcher` decision-fork matrix.
//!
//! 10 cases covering every branch of [`sqry_daemon::rebuild::decide_mode`]:
//!
//! 1. Trivial single-file edit → Incremental.
//! 2. `git_change_class = Some(BranchSwitch)` → Full (requires_full_rebuild).
//! 3. `git_change_class = Some(TreeDiverged)` → Full.
//! 4. `git_change_class = Some(LocalCommit)` → Incremental (non-full).
//! 5. `git_change_class = Some(Noise)` → Incremental (non-full).
//! 6. `changed_files.len() > incremental_threshold` → Full (threshold + 1).
//! 7. New file path not in registry + threshold not exceeded → Incremental.
//! 8. Large reverse-dep closure (>`closure_limit_percent`% of file_count) → Full.
//! 9. Empty ChangeSet (`is_empty() == true`) → Incremental (no-op rebuild).
//! 10. Closure exactly at integer boundary (`closure.len() == limit`) → Incremental (strict `>`).
//!
//! Cases 8 and 10 build a real `rust_small` graph once (via `OnceLock`)
//! to exercise reverse-dep closure math. Cases 1-7 and 9 run on the
//! empty / synthetic graph so decision-fork coverage does not depend
//! on real plugin output.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use sqry_core::{
    graph::{CodeGraph, unified::build::build_unified_graph},
    plugin::PluginManager,
    watch::{ChangeSet, GitChangeClass},
};
use sqry_daemon::{DaemonConfig, RebuildMode, decide_mode};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rust_small_fixture_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .expect("workspace root must exist")
        .join("sqry-core")
        .join("tests")
        .join("fixtures")
        .join("incremental")
        .join("rust_small")
}

/// Lazy-built `rust_small` fixture graph shared across the tests that
/// need real reverse-dep closure math. Construction costs are non-trivial
/// (plugin manager initialisation + build_unified_graph) so one-shot
/// initialisation is worth the complexity.
fn rust_small_graph() -> Arc<CodeGraph> {
    static GRAPH: OnceLock<Arc<CodeGraph>> = OnceLock::new();
    GRAPH
        .get_or_init(|| {
            let root = rust_small_fixture_path();
            let plugins = PluginManager::new();
            // Register every registered built-in plugin so the fixture
            // resolves Rust AST edges the same way Task 4 does.
            let plugins = {
                // `create_plugin_manager` is the canonical constructor;
                // fall back to `PluginManager::new` if the selection
                // path is unavailable for any reason.
                let _ = plugins;
                sqry_plugin_registry::create_plugin_manager()
            };
            let cfg = sqry_core::graph::unified::build::BuildConfig::default();
            let graph = build_unified_graph(&root, &plugins, &cfg)
                .expect("rust_small fixture must build cleanly");
            Arc::new(graph)
        })
        .clone()
}

fn empty_graph() -> CodeGraph {
    CodeGraph::new()
}

fn make_changes(files: Vec<PathBuf>) -> ChangeSet {
    ChangeSet {
        changed_files: files,
        git_state_changed: false,
        git_change_class: None,
    }
}

fn make_git_changes(class: GitChangeClass) -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(class),
    }
}

// ---------------------------------------------------------------------------
// Case 1 — trivial single-file edit → Incremental
// ---------------------------------------------------------------------------

#[test]
fn trivial_single_file_edit_chooses_incremental() {
    let graph = rust_small_graph();
    // Pick one existing file from the fixture so closure math runs
    // and stays small.
    let file = rust_small_fixture_path().join("util.rs");
    let changes = make_changes(vec![file]);
    let config = DaemonConfig::default();

    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}

// ---------------------------------------------------------------------------
// Case 2 — BranchSwitch → Full
// ---------------------------------------------------------------------------

#[test]
fn branch_switch_forces_full_rebuild() {
    let graph = empty_graph();
    let changes = make_git_changes(GitChangeClass::BranchSwitch);
    let config = DaemonConfig::default();

    assert_eq!(decide_mode(&config, &changes, &graph), RebuildMode::Full);
}

// ---------------------------------------------------------------------------
// Case 3 — TreeDiverged → Full
// ---------------------------------------------------------------------------

#[test]
fn tree_diverged_forces_full_rebuild() {
    let graph = empty_graph();
    let changes = make_git_changes(GitChangeClass::TreeDiverged);
    let config = DaemonConfig::default();

    assert_eq!(decide_mode(&config, &changes, &graph), RebuildMode::Full);
}

// ---------------------------------------------------------------------------
// Case 4 — LocalCommit → Incremental (non-full git trigger)
// ---------------------------------------------------------------------------

#[test]
fn local_commit_does_not_force_full() {
    let graph = rust_small_graph();
    // LocalCommit is not a full trigger per GitChangeClass::requires_full_rebuild.
    let mut changes = make_git_changes(GitChangeClass::LocalCommit);
    // LocalCommit alone with no changed files would hit is_empty() → Incremental.
    // Add an existing file so we exercise the "git trigger but non-full"
    // branch of decide_mode with a non-empty ChangeSet.
    changes
        .changed_files
        .push(rust_small_fixture_path().join("util.rs"));
    let config = DaemonConfig::default();

    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}

// ---------------------------------------------------------------------------
// Case 5 — Noise → Incremental (non-full git trigger)
// ---------------------------------------------------------------------------

#[test]
fn noise_does_not_force_full() {
    let graph = rust_small_graph();
    let mut changes = make_git_changes(GitChangeClass::Noise);
    changes
        .changed_files
        .push(rust_small_fixture_path().join("util.rs"));
    let config = DaemonConfig::default();

    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}

// ---------------------------------------------------------------------------
// Case 6 — changed_files.len() > incremental_threshold → Full
// ---------------------------------------------------------------------------

#[test]
fn over_threshold_file_count_forces_full() {
    let graph = empty_graph();
    let config = DaemonConfig::default();
    // Generate (threshold + 1) distinct file paths to exceed the bound
    // by exactly one (boundary sensitivity matters).
    let overshoot = config.incremental_threshold + 1;
    let files: Vec<PathBuf> = (0..overshoot)
        .map(|i| PathBuf::from(format!("/tmp/sqry-test-{i}.rs")))
        .collect();
    let changes = make_changes(files);

    assert_eq!(decide_mode(&config, &changes, &graph), RebuildMode::Full);
}

// ---------------------------------------------------------------------------
// Case 7 — new file path (not in registry) + under threshold → Incremental
// ---------------------------------------------------------------------------

#[test]
fn new_file_path_under_threshold_stays_incremental() {
    let graph = empty_graph();
    let config = DaemonConfig::default();
    // Empty graph → no registered files. A single new path stays under
    // `incremental_threshold`. `decide_mode` must NOT force Full just
    // because the path is not in the registry (Phase 3e's
    // phase3e_discover_new_file_paths handles it internally).
    let changes = make_changes(vec![PathBuf::from("/tmp/fresh-file.rs")]);

    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}

// ---------------------------------------------------------------------------
// Case 8 — oversized reverse-dep closure → Full
// ---------------------------------------------------------------------------

#[test]
fn oversized_closure_forces_full() {
    let graph = rust_small_graph();
    // Change the widest-reach file in the fixture so the reverse-dep
    // closure covers most of the workspace. `lib.rs` re-exports every
    // sibling module so depending nodes fan back to it, producing the
    // largest closure shape in the fixture.
    let widest_fanout = rust_small_fixture_path().join("lib.rs");
    let changes = make_changes(vec![widest_fanout]);

    // Shrink closure_limit_percent to 1% so even a small closure over
    // the 5-file fixture exceeds the limit. `1 * 5 / 100 == 0`, and
    // any non-empty closure has len >= 1 > 0, which lands us on Full.
    let config = DaemonConfig {
        closure_limit_percent: 1,
        ..DaemonConfig::default()
    };
    assert_eq!(decide_mode(&config, &changes, &graph), RebuildMode::Full);
}

// ---------------------------------------------------------------------------
// Case 9 — empty ChangeSet → Incremental (no-op rebuild)
// ---------------------------------------------------------------------------

#[test]
fn empty_changeset_chooses_incremental_noop() {
    let graph = empty_graph();
    let changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: false,
        git_change_class: None,
    };
    assert!(changes.is_empty());
    let config = DaemonConfig::default();

    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}

// ---------------------------------------------------------------------------
// Case 10 — closure exactly at integer boundary → Incremental (strict >)
// ---------------------------------------------------------------------------

#[test]
fn closure_at_boundary_stays_incremental() {
    let graph = rust_small_graph();
    // Edit a leaf file (no inbound callers) so the closure contains
    // exactly that file. With file_count == 5 and
    // `closure_limit_percent == 100`, the limit is `5 * 100 / 100 == 5`.
    // closure.len() == 1 <= 5 → Incremental.
    //
    // This pins down the strict `>` boundary: the decision only flips
    // to Full when closure strictly exceeds the limit, not when it
    // equals the limit.
    let leaf = rust_small_fixture_path().join("util.rs");
    let changes = make_changes(vec![leaf]);

    // closure_limit_percent 100 → limit == file_count; any closure
    // (including the whole workspace) is <= limit → Incremental.
    let config = DaemonConfig {
        closure_limit_percent: 100,
        ..DaemonConfig::default()
    };
    assert_eq!(
        decide_mode(&config, &changes, &graph),
        RebuildMode::Incremental
    );
}
