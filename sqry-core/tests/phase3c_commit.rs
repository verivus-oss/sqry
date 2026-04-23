//! Task 4 Step 4 Phase 3c — integration tests for the real
//! `incremental_rebuild` body sub-steps 4-6.
//!
//! These tests require real language plugins to drive the `parse_file`
//! path in sub-step 4. Unit tests inside `sqry-core/src/` cannot import
//! language plugins (every `sqry-lang-*` crate depends on `sqry-core`,
//! so the reverse direction would be a circular dep). Integration
//! tests can, which is why Phase 3c's pipeline-exercising tests live
//! here rather than alongside the Phase 3b unit tests in
//! `src/graph/unified/build/incremental.rs`.
//!
//! The tests observe Phase 3c's effects via the `#[cfg(any(test,
//! feature = "rebuild-internals"))]`-gated `testing::Phase3cHookGuard`
//! in the same module. The `rebuild-internals` feature is locked to
//! sqry-daemon + in-tree tests by
//! `sqry-core/tests/rebuild_internals_whitelist.rs`, so this hook
//! surface is not reachable from external crates even if they somehow
//! enabled the feature.
//!
//! # Tests
//!
//! 1. `incremental_rebuild_phase3c_reparses_closure_files_into_rebuild_graph`
//!    — drives a real two-file Rust fixture + an edit that touches the
//!    changed file, asserts the Phase 3c post-substep6 hook observes
//!    `files_committed > 0` and the rebuild plane's node count > 0.
//!
//! 2. `incremental_rebuild_phase3c_commits_intra_edges_to_rebuild_graph`
//!    — same fixture, but asserts `edges_collected > 0` (the call site
//!    inside the edited function produces an intra-file `Calls` edge).
//!
//! 3. `incremental_rebuild_phase3c_polls_cancellation_between_parse_and_commit`
//!    — cancellation flipped from inside the Phase 3c POST-REPARSE hook
//!    (i.e. AFTER sub-step 4 runs, BEFORE sub-step 5) must short-circuit
//!    at the post-reparse `cancellation.check()?` guard and return
//!    `GraphBuilderError::Cancelled`; the Phase 3c post-substep6 hook
//!    must NOT fire (proving sub-steps 5-6 never ran) but the Phase 3c
//!    post-reparse hook MUST fire (proving sub-step 4 actually ran
//!    before cancellation was observed).
//!
//! 4. `incremental_rebuild_phase3c_does_not_fire_when_phase3b_loop_cancels`
//!    — symmetrical to (3): cancellation flipped from inside the Phase
//!    3b per-iteration hook (i.e. BEFORE sub-step 4 runs) must short-
//!    circuit at the Phase 3b loop-top `cancellation.check()?` and
//!    return `GraphBuilderError::Cancelled`; neither the Phase 3c
//!    post-reparse hook nor the post-substep6 hook may fire (proving
//!    sub-step 4 never ran).
//!
//! 5. `incremental_rebuild_phase3c_polls_cancellation_inside_reparse_loop`
//!    — cancellation flipped from inside the Phase 3c per-iteration
//!    hook on iteration 0 must short-circuit the re-parse loop at
//!    iteration (N+1)'s loop-top `cancellation.check()?` and return
//!    `GraphBuilderError::Cancelled`. Proves the per-iteration
//!    cancellation polling inside `phase3c_reparse_closure` keeps
//!    cancellation responsive even for large closures where
//!    `parse_file` dominates wall time.
//!
//! 6. `incremental_rebuild_phase3c_rebuild_graph_contains_parsed_symbols_before_discard`
//!    — asserts that the rebuild plane's node arena slot count is
//!    non-zero at the moment the Phase 3c hook fires, proving the
//!    sub-step 6 commit landed on the rebuild plane (not the full-build
//!    fallback).
//!
//! 7. `incremental_rebuild_phase3c_still_delegates_to_full_build_fallback`
//!    — asserts the final `incremental_rebuild` return value is Ok(_)
//!    and the returned graph has at least the nodes the full-build
//!    baseline would have. Preserves the §E correctness invariant:
//!    Phase 3c does not yet publish `rebuild_graph`, so the delegate
//!    still runs.

#![cfg(feature = "rebuild-internals")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use tempfile::TempDir;

use sqry_core::graph::GraphBuilderError;
use sqry_core::graph::unified::build::incremental::testing::{
    Phase3bIterHookGuard, Phase3cHookGuard, Phase3cIterHookGuard, Phase3cReparseHookGuard,
};
use sqry_core::graph::unified::build::incremental::{
    PostCommitDiagnostics, compute_reverse_dep_closure, incremental_rebuild,
};
use sqry_core::graph::unified::build::{BuildConfig, CancellationToken, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::file::FileId;
use sqry_core::plugin::PluginManager;

// --- plugin manager -----------------------------------------------------

fn plugin_manager() -> &'static PluginManager {
    static MANAGER: OnceLock<PluginManager> = OnceLock::new();
    MANAGER.get_or_init(|| {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
        manager
    })
}

// --- fixture setup ------------------------------------------------------

/// Create a two-file Rust fixture whose contents are deterministic
/// across runs:
///
/// - `lib.rs` declares `mod a;` and calls `a::greet()`.
/// - `a.rs` defines `pub fn greet() -> &'static str { "hi" }`.
///
/// The call from `lib.rs::main_entry` into `a::greet` ensures the
/// edited closure has at least one intra-file `Calls` edge for the
/// `commits_intra_edges_to_rebuild_graph` test to observe.
fn write_fixture(workspace: &Path) -> PathBuf {
    std::fs::write(
        workspace.join("lib.rs"),
        r#"mod a;

pub fn main_entry() -> &'static str {
    let _g = a::greet();
    helper()
}

fn helper() -> &'static str {
    "helper"
}
"#,
    )
    .expect("write lib.rs");

    std::fs::write(
        workspace.join("a.rs"),
        r#"pub fn greet() -> &'static str {
    "hi"
}
"#,
    )
    .expect("write a.rs");

    workspace.join("lib.rs")
}

/// Modify `a.rs` in-place — a realistic edit that changes an exported
/// symbol's body but keeps the signature intact. The reverse-dep
/// closure widens over `lib.rs` because `lib.rs` imports `a::greet`.
fn edit_a_rs(workspace: &Path) -> PathBuf {
    let a_path = workspace.join("a.rs");
    std::fs::write(
        &a_path,
        r#"pub fn greet() -> &'static str {
    "hi, world"
}

pub fn greet_also() -> &'static str {
    "also"
}
"#,
    )
    .expect("rewrite a.rs");
    a_path
}

fn build_config() -> BuildConfig {
    BuildConfig::default()
}

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// --- test fixtures ------------------------------------------------------

struct Fixture {
    _tempdir: TempDir,
    /// Canonicalised workspace root. Retained so future Phase 3c
    /// follow-ups can assert on filesystem layout without re-running
    /// canonicalisation.
    #[allow(dead_code)]
    workspace: PathBuf,
    /// The initial `current_graph` produced by a full build.
    current_graph: CodeGraph,
    /// The set of changed paths after `edit_a_rs` has run.
    changed_paths: Vec<PathBuf>,
    /// Reverse-dep closure for the changed paths.
    closure: std::collections::HashSet<FileId>,
}

fn build_fixture() -> Fixture {
    let tempdir = TempDir::new().expect("make tempdir");
    let workspace = canon(tempdir.path());
    write_fixture(&workspace);
    let current_graph =
        build_unified_graph(&workspace, plugin_manager(), &build_config()).expect("initial build");

    // Apply the edit and materialise the changed-paths set.
    let edited_path = edit_a_rs(&workspace);
    let changed_paths = vec![edited_path];

    // Resolve the changed-path FileIds via the current graph's registry
    // and compute the reverse-dep closure from them.
    let changed_fids: Vec<FileId> = current_graph
        .indexed_files()
        .filter_map(|(fid, path)| {
            if changed_paths.iter().any(|p| canon(p) == canon(path)) {
                Some(fid)
            } else {
                None
            }
        })
        .collect();
    let closure = compute_reverse_dep_closure(&changed_fids, &current_graph);
    assert!(
        !closure.is_empty(),
        "two-file Rust fixture must produce a non-empty closure when a.rs changes"
    );

    Fixture {
        _tempdir: tempdir,
        workspace,
        current_graph,
        changed_paths,
        closure,
    }
}

/// Like [`build_fixture`] but produces a closure of size >= 2 by
/// seeding THREE standalone Rust files (`x.rs`, `y.rs`, `z.rs`) and
/// passing all three as changed paths. This sidesteps any question of
/// whether `mod` declarations produce reverse-`Imports` edges in the
/// Rust plugin — the `changed_files` themselves are always included in
/// the closure by definition, so with three seed files the closure
/// has at least three elements.
///
/// Used by the Phase 3c per-iteration cancellation test, which needs
/// the re-parse loop to have >= 2 iterations for the "cancel on
/// iteration 0, observe iteration 1's short-circuit" assertion to be
/// meaningful.
fn build_multi_file_fixture() -> Fixture {
    let tempdir = TempDir::new().expect("make tempdir");
    let workspace = canon(tempdir.path());
    // Three standalone modules with no cross-file references. Plain
    // top-level functions per file so each parse produces at least
    // one Function node (useful for any future per-file assertions).
    std::fs::write(
        workspace.join("x.rs"),
        "pub fn x_one() -> u32 { 1 }\npub fn x_two() -> u32 { 2 }\n",
    )
    .expect("write x.rs");
    std::fs::write(
        workspace.join("y.rs"),
        "pub fn y_one() -> u32 { 10 }\npub fn y_two() -> u32 { 20 }\n",
    )
    .expect("write y.rs");
    std::fs::write(
        workspace.join("z.rs"),
        "pub fn z_one() -> u32 { 100 }\npub fn z_two() -> u32 { 200 }\n",
    )
    .expect("write z.rs");
    let current_graph =
        build_unified_graph(&workspace, plugin_manager(), &build_config()).expect("initial build");

    // Rewrite every file in place (simulating a multi-file edit).
    std::fs::write(
        workspace.join("x.rs"),
        "pub fn x_one() -> u32 { 3 }\npub fn x_two() -> u32 { 4 }\n",
    )
    .expect("rewrite x.rs");
    std::fs::write(
        workspace.join("y.rs"),
        "pub fn y_one() -> u32 { 30 }\npub fn y_two() -> u32 { 40 }\n",
    )
    .expect("rewrite y.rs");
    std::fs::write(
        workspace.join("z.rs"),
        "pub fn z_one() -> u32 { 300 }\npub fn z_two() -> u32 { 400 }\n",
    )
    .expect("rewrite z.rs");

    let changed_paths = vec![
        workspace.join("x.rs"),
        workspace.join("y.rs"),
        workspace.join("z.rs"),
    ];
    let changed_fids: Vec<FileId> = current_graph
        .indexed_files()
        .filter_map(|(fid, path)| {
            if changed_paths.iter().any(|p| canon(p) == canon(path)) {
                Some(fid)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        changed_fids.len(),
        3,
        "the three standalone Rust files must all be indexed"
    );
    let closure = compute_reverse_dep_closure(&changed_fids, &current_graph);
    assert!(
        closure.len() >= 2,
        "multi-file fixture must yield a closure of at least 2 files; got {}",
        closure.len()
    );

    Fixture {
        _tempdir: tempdir,
        workspace,
        current_graph,
        changed_paths,
        closure,
    }
}

// --- tests --------------------------------------------------------------

#[test]
fn incremental_rebuild_phase3c_reparses_closure_files_into_rebuild_graph() {
    let fx = build_fixture();
    let cancellation = CancellationToken::new();

    let diagnostics_seen = Rc::new(RefCell::new(PostCommitDiagnostics::default()));
    let diagnostics_hook = Rc::clone(&diagnostics_seen);
    let _guard = Phase3cHookGuard::install(move |_rg, diag| {
        *diagnostics_hook.borrow_mut() = diag.clone();
    });

    // Drive the real incremental_rebuild.
    let _ = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect("incremental_rebuild happy path");

    let diag = diagnostics_seen.borrow();
    assert!(
        diag.files_committed > 0,
        "Phase 3c sub-step 4 must successfully re-parse at least one closure file; \
         diagnostics: {diag:?}"
    );
    assert!(
        diag.nodes_committed > 0,
        "Phase 3c sub-step 6 must commit non-zero nodes into the rebuild plane; \
         diagnostics: {diag:?}"
    );
}

#[test]
fn incremental_rebuild_phase3c_commits_intra_edges_to_rebuild_graph() {
    let fx = build_fixture();
    let cancellation = CancellationToken::new();

    let diagnostics_seen = Rc::new(RefCell::new(PostCommitDiagnostics::default()));
    let diagnostics_hook = Rc::clone(&diagnostics_seen);
    let _guard = Phase3cHookGuard::install(move |_rg, diag| {
        *diagnostics_hook.borrow_mut() = diag.clone();
    });

    let _ = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect("incremental_rebuild happy path");

    let diag = diagnostics_seen.borrow();
    // The fixture's lib.rs calls a::greet() and helper(). Re-parsing
    // lib.rs must collect at least one intra-file Calls edge in the
    // pending-edge vectors returned by phase3_parallel_commit.
    assert!(
        diag.edges_collected > 0,
        "Phase 3c must collect non-zero intra-file edges from the re-parsed closure; \
         the lib.rs fixture has `let _g = a::greet();` plus `helper()`, which parse \
         into two Calls edges minimum. Diagnostics: {diag:?}"
    );
}

#[test]
fn incremental_rebuild_phase3c_polls_cancellation_between_parse_and_commit() {
    // Scenario: cancellation is flipped from inside the Phase 3c
    // post-reparse hook (i.e. AFTER sub-step 4 has finished re-parsing
    // every closure file, but BEFORE the post-reparse
    // `cancellation.check()?` guard that gates sub-step 5). Sub-step 4
    // therefore runs to completion, and cancellation is observed at
    // exactly the boundary under test. This proves the poll boundary
    // between Phase 3c's re-parse and commit steps actually exists
    // and short-circuits the pipeline before sub-steps 5-6 mutate the
    // rebuild plane.
    //
    // Strength of this assertion: the Phase 3c post-reparse hook MUST
    // fire (proving sub-step 4 ran), and the Phase 3c post-substep6
    // hook MUST NOT fire (proving sub-steps 5-6 were skipped). No
    // Phase 3b iter hook is installed — the token is un-cancelled at
    // pipeline entry, so the Phase 3b loop and the pre-flight /
    // post-clone / post-substep3 checks all observe a clean token and
    // fall through into Phase 3c.
    let fx = build_fixture();
    let cancellation = CancellationToken::new();

    let reparse_fired = Rc::new(RefCell::new(0usize));
    let reparse_fired_hook = Rc::clone(&reparse_fired);
    let cancel_from_hook = cancellation.clone();
    let _r_guard = Phase3cReparseHookGuard::install(move |parsed_count| {
        *reparse_fired_hook.borrow_mut() = parsed_count;
        cancel_from_hook.cancel();
    });

    let phase3c_post_substep6_fired = Rc::new(RefCell::new(false));
    let phase3c_post_substep6_fired_hook = Rc::clone(&phase3c_post_substep6_fired);
    let _c_guard = Phase3cHookGuard::install(move |_rg, _diag| {
        *phase3c_post_substep6_fired_hook.borrow_mut() = true;
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("post-reparse cancellation must short-circuit incremental_rebuild");

    assert!(
        matches!(err, GraphBuilderError::Cancelled),
        "expected GraphBuilderError::Cancelled, got: {err:?}"
    );
    let reparsed_count = *reparse_fired.borrow();
    assert!(
        reparsed_count > 0,
        "Phase 3c post-reparse hook MUST fire before cancellation is observed, and \
         it MUST see a non-zero parsed_count (the fixture's closure always contains \
         at least one re-parsable file). parsed_count={reparsed_count}"
    );
    assert!(
        !*phase3c_post_substep6_fired.borrow(),
        "Phase 3c post-substep6 hook must NOT fire when cancellation is flipped \
         between sub-step 4 and sub-step 5 — sub-steps 5-6 must not run"
    );
}

#[test]
fn incremental_rebuild_phase3c_does_not_fire_when_phase3b_loop_cancels() {
    // Scenario: cancellation is flipped from inside the Phase 3b
    // per-iteration hook on iteration 0 (i.e. after the first
    // `remove_file` call, but before any subsequent sub-step). The
    // post-loop and post-substep-3 cancellation checks then fire, so
    // execution does NOT reach Phase 3c sub-step 4 (re-parse). This
    // proves that Phase 3b's cancellation surface is not shadowed by
    // Phase 3c — i.e. the two boundaries are independent and both
    // still gate the pipeline.
    //
    // Strength of this assertion: neither the Phase 3c post-reparse
    // hook nor the post-substep6 hook may fire. Phase 3c must never
    // see this cancellation.
    let fx = build_fixture();
    let cancellation = CancellationToken::new();

    let cancel_from_hook = cancellation.clone();
    let _iter_guard = Phase3bIterHookGuard::install(move |idx, _fid, _rg| {
        if idx == 0 {
            cancel_from_hook.cancel();
        }
    });

    let phase3c_reparse_fired = Rc::new(RefCell::new(false));
    let phase3c_reparse_fired_hook = Rc::clone(&phase3c_reparse_fired);
    let _r_guard = Phase3cReparseHookGuard::install(move |_parsed_count| {
        *phase3c_reparse_fired_hook.borrow_mut() = true;
    });

    let phase3c_post_substep6_fired = Rc::new(RefCell::new(false));
    let phase3c_post_substep6_fired_hook = Rc::clone(&phase3c_post_substep6_fired);
    let _c_guard = Phase3cHookGuard::install(move |_rg, _diag| {
        *phase3c_post_substep6_fired_hook.borrow_mut() = true;
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("mid-loop cancellation must short-circuit incremental_rebuild");

    assert!(
        matches!(err, GraphBuilderError::Cancelled),
        "expected GraphBuilderError::Cancelled, got: {err:?}"
    );
    assert!(
        !*phase3c_reparse_fired.borrow(),
        "Phase 3c post-reparse hook must NOT fire when Phase 3b's iteration-time \
         cancellation short-circuits the pipeline — sub-step 4 must never run"
    );
    assert!(
        !*phase3c_post_substep6_fired.borrow(),
        "Phase 3c post-substep6 hook must NOT fire when Phase 3b's iteration-time \
         cancellation short-circuits the pipeline"
    );
}

#[test]
fn incremental_rebuild_phase3c_polls_cancellation_inside_reparse_loop() {
    // Scenario: cancellation is flipped from inside Phase 3c's PER-
    // ITERATION hook after iteration 0's `parse_file` completes. The
    // next iteration's loop-top `cancellation.check()?` must observe
    // the cancelled token and short-circuit the rest of the re-parse
    // loop. The Phase 3c post-reparse hook must therefore NOT fire
    // (because `phase3c_reparse_closure` returns Err before the outer
    // site fires the post-reparse hook), nor must the post-substep6
    // hook fire.
    //
    // This covers the Gemini peer MINOR: without per-iteration
    // polling, a large closure would block cancellation for the
    // duration of `parse_file` on every remaining file. With the
    // polling in place, cancellation takes effect within one file.
    //
    // Note: the per-iteration cancellation surface only matters for
    // closures with >= 2 files (a 1-file closure trivially completes
    // iteration 0 before any subsequent check could fire). The
    // `build_multi_file_fixture` helper below constructs a closure of
    // 3 standalone Rust files by seeding all three as changed paths,
    // independent of whatever import edges the Rust plugin would
    // otherwise produce.
    let fx = build_multi_file_fixture();
    assert!(
        fx.closure.len() >= 2,
        "build_multi_file_fixture must produce a closure with >= 2 files; got {}",
        fx.closure.len()
    );
    let cancellation = CancellationToken::new();

    let iter_fired_for = Rc::new(RefCell::new(Vec::<usize>::new()));
    let iter_fired_for_hook = Rc::clone(&iter_fired_for);
    let cancel_from_hook = cancellation.clone();
    let _iter_guard = Phase3cIterHookGuard::install(move |idx| {
        iter_fired_for_hook.borrow_mut().push(idx);
        if idx == 0 {
            cancel_from_hook.cancel();
        }
    });

    let phase3c_reparse_fired = Rc::new(RefCell::new(false));
    let phase3c_reparse_fired_hook = Rc::clone(&phase3c_reparse_fired);
    let _r_guard = Phase3cReparseHookGuard::install(move |_parsed_count| {
        *phase3c_reparse_fired_hook.borrow_mut() = true;
    });

    let phase3c_post_substep6_fired = Rc::new(RefCell::new(false));
    let phase3c_post_substep6_fired_hook = Rc::clone(&phase3c_post_substep6_fired);
    let _c_guard = Phase3cHookGuard::install(move |_rg, _diag| {
        *phase3c_post_substep6_fired_hook.borrow_mut() = true;
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("mid-reparse-loop cancellation must short-circuit incremental_rebuild");

    assert!(
        matches!(err, GraphBuilderError::Cancelled),
        "expected GraphBuilderError::Cancelled, got: {err:?}"
    );

    let iter_indices = iter_fired_for.borrow().clone();
    assert_eq!(
        iter_indices,
        vec![0],
        "the per-iteration hook must fire exactly once (iteration 0). iteration 1's \
         loop-top `cancellation.check()?` must short-circuit before `parse_file` — \
         and therefore before the iter hook — runs. observed: {iter_indices:?}"
    );
    assert!(
        !*phase3c_reparse_fired.borrow(),
        "Phase 3c post-reparse hook must NOT fire when `phase3c_reparse_closure` \
         returns Err(Cancelled) — the outer caller site fires the post-reparse hook \
         only after a successful return from sub-step 4"
    );
    assert!(
        !*phase3c_post_substep6_fired.borrow(),
        "Phase 3c post-substep6 hook must NOT fire when cancellation short-circuits \
         inside the sub-step 4 re-parse loop — sub-steps 5-6 must not run"
    );
}

#[test]
fn incremental_rebuild_phase3c_rebuild_graph_contains_parsed_symbols_before_discard() {
    // Assert that at the moment the Phase 3c hook fires, the rebuild
    // plane's arena slot_count is non-zero. The initial
    // clone_for_rebuild preserves the current graph's arena (minus
    // tombstones from closure removals), so slot_count is already > 0
    // before sub-step 4. We therefore assert that it STRICTLY grew
    // between the pre-rebuild value and the post-substep6 value.
    let fx = build_fixture();

    // Capture the pre-rebuild slot_count. clone_for_rebuild
    // deep-copies, so the rebuild plane starts at exactly this value.
    let initial_slot_count = fx.current_graph.nodes().slot_count();

    let cancellation = CancellationToken::new();
    let post_slot = Rc::new(RefCell::new(0usize));
    let post_slot_hook = Rc::clone(&post_slot);
    let _guard = Phase3cHookGuard::install(move |rg, _diag| {
        *post_slot_hook.borrow_mut() = rg.nodes().slot_count();
    });

    let _ = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect("incremental_rebuild happy path");

    let final_slot_count = *post_slot.borrow();
    assert!(
        final_slot_count > initial_slot_count,
        "Phase 3c sub-step 6 must grow the rebuild plane's arena slot_count beyond \
         the clone_for_rebuild starting value. initial={initial_slot_count}, \
         final={final_slot_count}"
    );
}

#[test]
fn incremental_rebuild_phase3c_still_delegates_to_full_build_fallback() {
    // Phase 3c is still a scaffolding phase: sub-steps 7-13 delegate
    // to build_unified_graph. Assert the final return is Ok(_) AND
    // the returned graph has nodes — i.e. the fallback wasn't
    // accidentally replaced by an empty result. (§E proptest locks
    // the semantic-equivalence invariant at a deeper level; this
    // test is a coarser "pipeline still delivers a populated graph"
    // safety net.)
    let fx = build_fixture();
    let cancellation = CancellationToken::new();

    let phase3c_fired = Rc::new(RefCell::new(false));
    let phase3c_fired_hook = Rc::clone(&phase3c_fired);
    let _guard = Phase3cHookGuard::install(move |_rg, _diag| {
        *phase3c_fired_hook.borrow_mut() = true;
    });

    let result = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect("fallback full build must succeed");

    assert!(
        *phase3c_fired.borrow(),
        "Phase 3c hook must fire before the fallback runs — otherwise sub-steps 4-6 \
         were skipped entirely"
    );

    assert!(
        result.node_count() > 0,
        "the fallback full-build path must still produce a populated graph; the Phase \
         3c boundary must not mask the result"
    );
}
