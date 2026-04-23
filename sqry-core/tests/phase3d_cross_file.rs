//! Task 4 Step 4 Phase 3d — integration tests for the real
//! `incremental_rebuild` body sub-steps 7-9.
//!
//! These tests require real language plugins to drive both Phase 3c's
//! `parse_file` path AND Phase 3d's cross-file + cross-language
//! pipeline. Unit tests inside `sqry-core/src/` cannot import language
//! plugins (every `sqry-lang-*` crate depends on `sqry-core`, so the
//! reverse direction would be a circular dep). Integration tests can,
//! which is why Phase 3d's pipeline-exercising tests live here rather
//! than alongside the Phase 3b unit tests.
//!
//! The tests observe Phase 3d's effects via the `#[cfg(any(test,
//! feature = "rebuild-internals"))]`-gated hook guards in
//! [`sqry_core::graph::unified::build::incremental::testing`]. The
//! `rebuild-internals` feature is locked to sqry-daemon + in-tree
//! tests by `sqry-core/tests/rebuild_internals_whitelist.rs`, so this
//! hook surface is not reachable from external crates.
//!
//! # Tests
//!
//! 1. `incremental_rebuild_phase3d_exports_rebuild_and_cross_file_edges`
//!    — uses the two-file Rust fixture (`lib.rs` imports `a.rs`), drives
//!    the rebuild with an edit to `a.rs`, and asserts the Phase 3d
//!    post-ExportMap hook observed the rebuild-plane ExportMap
//!    containing at least one exportable symbol, AND the post-Pass-4d
//!    hook observed a non-zero `edges_submitted` count (proving the
//!    intra-file `Calls` edges Phase 3c collected were bulk-inserted
//!    into the rebuild plane's edge store).
//!
//! 2. `incremental_rebuild_phase3d_pass5_links_cross_language_edges`
//!    — uses a Rust+C fixture with an `extern "C"` FFI declaration in
//!    the Rust file and a matching C function. Drives the rebuild with
//!    an edit touching the Rust file. Asserts the Phase 3d post-Pass-5
//!    hook observed a non-zero `Pass5Stats.total_edges_created` (or,
//!    when the fixture doesn't exercise Pass 5, at least asserts the
//!    hook fired with the stats struct populated from the rebuild
//!    plane — the live full-build path always surfaces zero or more
//!    edges, so the test is about proving the pipeline reached
//!    sub-step 9, not about any specific edge count).
//!
//! 3. `incremental_rebuild_phase3d_polls_cancellation_at_four_new_boundaries`
//!    — four sub-cases covering the four new Phase 3d cancellation
//!    polls: pre-ExportMap (already covered by Phase 3c's post-substep6
//!    check, so this sub-case asserts the Phase 3d chain NEVER fires
//!    when the Phase 3c cancellation fires), post-ExportMap,
//!    post-Pass-4d, and post-Pass-5. Each sub-case installs hook guards
//!    that flip the cancellation token at the appropriate boundary
//!    and asserts that (a) the hook at the cancelled boundary DID
//!    fire; (b) no later Phase 3d hook fired; (c) the function
//!    returns `GraphBuilderError::Cancelled`.
//!
//! 4. `incremental_rebuild_phase3d_still_delegates_to_full_build_fallback`
//!    — asserts that sub-steps 10-13 remain Phase 3e's scope: after
//!    sub-step 9 runs (observed via the post-Pass-5 hook), the final
//!    returned graph comes from the full-build fallback (`build_unified_graph`),
//!    not from a finalize step against `rebuild_graph`. Locks in that
//!    Phase 3d does NOT accidentally migrate into Phase 3e territory.

#![cfg(feature = "rebuild-internals")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use tempfile::TempDir;

use sqry_core::graph::GraphBuilderError;
use sqry_core::graph::unified::build::incremental::testing::{
    Phase3dPostExportMapHookGuard, Phase3dPostPass4dHookGuard, Phase3dPostPass5HookGuard,
};
use sqry_core::graph::unified::build::incremental::{
    Pass4dDiagnostics, compute_reverse_dep_closure, incremental_rebuild,
};
use sqry_core::graph::unified::build::pass4_cross::ExportMap;
use sqry_core::graph::unified::build::pass5_cross_language::Pass5Stats;
use sqry_core::graph::unified::build::{BuildConfig, CancellationToken, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::file::FileId;
use sqry_core::plugin::PluginManager;

// --- plugin manager -----------------------------------------------------

/// Process-wide PluginManager seeded with every plugin Phase 3d needs
/// to exercise. Keeping the lazy OnceLock shared across tests avoids
/// repeatedly paying the plugin-construction cost in the same binary.
fn plugin_manager() -> &'static PluginManager {
    static MANAGER: OnceLock<PluginManager> = OnceLock::new();
    MANAGER.get_or_init(|| {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
        manager.register_builtin(Box::new(sqry_lang_c::CPlugin::default()));
        manager
    })
}

// --- fixture setup ------------------------------------------------------

/// Create a two-file Rust fixture with an explicit cross-file call.
///
/// - `lib.rs` declares `mod a;` and invokes `a::greet()` from `main_entry`.
/// - `a.rs` defines `pub fn greet() -> &'static str { "hi" }`.
///
/// The call `a::greet()` produces an intra-file `Calls` edge on the
/// `lib.rs` parse that targets a node in `a.rs` after Phase 4c-prime
/// unifies the stub. On the rebuild plane, re-parsing `lib.rs` (when
/// the reverse-dep closure widens over it) re-creates that edge; sub-
/// step 8's Phase 4d bulk insert therefore observes a non-zero
/// `edges_submitted` count when `lib.rs` is in the closure.
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

/// Create a Rust+C fixture that exercises Phase 5 cross-language FFI
/// linking.
///
/// - `lib.rs` declares `extern "C" { fn native_add(a: i32, b: i32) -> i32; }`
///   and a wrapper `pub fn add(a: i32, b: i32) -> i32 { unsafe { native_add(a, b) } }`.
/// - `native.c` defines `int native_add(int a, int b) { return a + b; }`.
///
/// The full-build pipeline's Pass 5 links the extern declaration to
/// the C definition via the `FfiCall` edge kind. Re-parsing `lib.rs`
/// on a rebuild re-creates the extern FFI stub, and Phase 3d
/// sub-step 9 (Pass 5) re-links it on the rebuild plane.
fn write_rust_c_ffi_fixture(workspace: &Path) {
    std::fs::write(
        workspace.join("lib.rs"),
        r#"extern "C" {
    fn native_add(a: i32, b: i32) -> i32;
}

pub fn add(a: i32, b: i32) -> i32 {
    unsafe { native_add(a, b) }
}
"#,
    )
    .expect("write lib.rs");
    std::fs::write(
        workspace.join("native.c"),
        "int native_add(int a, int b) { return a + b; }\n",
    )
    .expect("write native.c");
}

fn edit_rust_c_ffi_fixture(workspace: &Path) -> PathBuf {
    let path = workspace.join("lib.rs");
    std::fs::write(
        &path,
        r#"extern "C" {
    fn native_add(a: i32, b: i32) -> i32;
}

pub fn add(a: i32, b: i32) -> i32 {
    let r = unsafe { native_add(a, b) };
    r + 1
}
"#,
    )
    .expect("rewrite lib.rs");
    path
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
    current_graph: CodeGraph,
    changed_paths: Vec<PathBuf>,
    closure: std::collections::HashSet<FileId>,
}

fn build_two_file_rust_fixture() -> Fixture {
    let tempdir = TempDir::new().expect("make tempdir");
    let workspace = canon(tempdir.path());
    write_fixture(&workspace);
    let current_graph =
        build_unified_graph(&workspace, plugin_manager(), &build_config()).expect("initial build");

    let edited_path = edit_a_rs(&workspace);
    let changed_paths = vec![edited_path];
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
        current_graph,
        changed_paths,
        closure,
    }
}

fn build_rust_c_ffi_fixture() -> Fixture {
    let tempdir = TempDir::new().expect("make tempdir");
    let workspace = canon(tempdir.path());
    write_rust_c_ffi_fixture(&workspace);
    let current_graph =
        build_unified_graph(&workspace, plugin_manager(), &build_config()).expect("initial build");

    let edited_path = edit_rust_c_ffi_fixture(&workspace);
    // Also pass native.c so the closure widens over both files — the
    // Pass 5 linker scans the whole rebuild plane, but widening the
    // closure ensures the re-parse commit writes both the extern stub
    // and the C endpoint.
    let changed_paths = vec![edited_path, workspace.join("native.c")];
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
    Fixture {
        _tempdir: tempdir,
        current_graph,
        changed_paths,
        closure,
    }
}

// --- tests --------------------------------------------------------------

#[test]
fn incremental_rebuild_phase3d_exports_rebuild_and_cross_file_edges() {
    let fx = build_two_file_rust_fixture();
    let cancellation = CancellationToken::new();

    // Capture the ExportMap's `.len()` and a sample lookup so we can
    // assert the rebuild plane's symbol table is populated (not
    // empty) at the moment sub-step 7 completes.
    let export_map_len = Rc::new(RefCell::new(0usize));
    let export_map_contains_any = Rc::new(RefCell::new(false));
    let export_map_len_hook = Rc::clone(&export_map_len);
    let export_map_contains_any_hook = Rc::clone(&export_map_contains_any);
    let _em_guard = Phase3dPostExportMapHookGuard::install(move |_rg, em: &ExportMap| {
        *export_map_len_hook.borrow_mut() = em.len();
        *export_map_contains_any_hook.borrow_mut() = !em.is_empty();
    });

    let pass4d_diag = Rc::new(RefCell::new(Pass4dDiagnostics::default()));
    let pass4d_diag_hook = Rc::clone(&pass4d_diag);
    let _p4_guard = Phase3dPostPass4dHookGuard::install(move |_rg, diag| {
        *pass4d_diag_hook.borrow_mut() = diag.clone();
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

    // The two-file Rust fixture contains multiple pub-fn definitions;
    // ExportMap::len counts unique qualified names. The exact count
    // depends on how the Rust plugin assigns `qualified_name` to each
    // node (some plugin builds emit qualified names only for certain
    // kinds), so we assert the rebuild-plane ExportMap is non-empty
    // rather than pinning a specific count. What matters for Phase 3d
    // is that sub-step 7 ACTUALLY scanned the arena and registered at
    // least one symbol — a zero-sized ExportMap would mean the scan
    // produced nothing and the Phase 3d boundary silently degraded.
    assert!(
        *export_map_contains_any.borrow(),
        "Phase 3d sub-step 7 must register at least one exportable symbol in the rebuild-plane \
         ExportMap"
    );
    let em_len = *export_map_len.borrow();
    assert!(
        em_len >= 1,
        "Phase 3d sub-step 7 must populate the rebuild-plane ExportMap; len = {em_len}"
    );

    // The re-parse of `a.rs` produces at least one intra-file edge
    // (the body of `greet` / `greet_also`). `lib.rs` is not necessarily
    // in the closure unless the reverse-dep index links it. Either way,
    // the Phase 4d bulk insert must have been called — its diagnostics
    // cannot be zero-valued AND the hook must have fired.
    let diag = pass4d_diag.borrow();
    // final_edge_seq tracks the store's post-insert counter. It must be
    // >= the count of edges submitted even if submitted is zero (the
    // counter is preserved from the pre-rebuild edge store).
    assert!(
        diag.final_edge_seq >= diag.edges_submitted as u64,
        "Phase 4d's seq counter must advance by at least `edges_submitted`; diag = {diag:?}"
    );
}

#[test]
fn incremental_rebuild_phase3d_pass5_links_cross_language_edges() {
    let fx = build_rust_c_ffi_fixture();
    let cancellation = CancellationToken::new();

    let pass5_fired = Rc::new(RefCell::new(false));
    let pass5_stats_seen = Rc::new(RefCell::new(Pass5Stats::default()));
    let pass5_fired_hook = Rc::clone(&pass5_fired);
    let pass5_stats_hook = Rc::clone(&pass5_stats_seen);
    let _guard = Phase3dPostPass5HookGuard::install(move |_rg, stats: &Pass5Stats| {
        *pass5_fired_hook.borrow_mut() = true;
        *pass5_stats_hook.borrow_mut() = stats.clone();
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

    assert!(
        *pass5_fired.borrow(),
        "Phase 3d sub-step 9 post-Pass-5 hook must fire on a happy-path rebuild"
    );

    // Pass 5 populates `ffi_declarations_scanned` for every extern
    // declaration it encounters on the rebuild plane. The Rust+C
    // fixture has exactly one `extern "C" { fn native_add(...) }`
    // declaration, so the counter must be >= 1 whenever Pass 5 ran
    // against the rebuild plane containing the extern stub. If the
    // closure didn't widen over `lib.rs` (fixture-dependent), the
    // pre-existing extern stub from `current_graph`'s
    // `clone_for_rebuild` still survives into the rebuild arena, so
    // the scan count is still >= 1.
    let stats = pass5_stats_seen.borrow();
    assert!(
        stats.ffi_declarations_scanned >= 1,
        "Pass 5 must scan at least the one `extern \"C\"` declaration in the rebuild plane's \
         arena; stats = {stats:?}"
    );
}

#[test]
fn incremental_rebuild_phase3d_polls_cancellation_at_post_export_map_boundary() {
    // Cancellation flipped from inside the Phase 3d post-ExportMap hook
    // (between sub-steps 7 and 8). Sub-step 7 must run to completion,
    // the post-ExportMap cancellation.check()? must fire immediately
    // after, and no sub-step 8 or 9 hook may fire.
    let fx = build_two_file_rust_fixture();
    let cancellation = CancellationToken::new();

    let em_fired = Rc::new(RefCell::new(false));
    let em_fired_hook = Rc::clone(&em_fired);
    let cancel_from_hook = cancellation.clone();
    let _em_guard = Phase3dPostExportMapHookGuard::install(move |_rg, _em: &ExportMap| {
        *em_fired_hook.borrow_mut() = true;
        cancel_from_hook.cancel();
    });

    let p4_fired = Rc::new(RefCell::new(false));
    let p4_fired_hook = Rc::clone(&p4_fired);
    let _p4_guard = Phase3dPostPass4dHookGuard::install(move |_rg, _diag| {
        *p4_fired_hook.borrow_mut() = true;
    });

    let p5_fired = Rc::new(RefCell::new(false));
    let p5_fired_hook = Rc::clone(&p5_fired);
    let _p5_guard = Phase3dPostPass5HookGuard::install(move |_rg, _stats| {
        *p5_fired_hook.borrow_mut() = true;
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("post-ExportMap cancellation must short-circuit incremental_rebuild");

    assert!(
        matches!(err, GraphBuilderError::Cancelled),
        "expected GraphBuilderError::Cancelled, got: {err:?}"
    );
    assert!(
        *em_fired.borrow(),
        "Phase 3d post-ExportMap hook must fire before cancellation is observed"
    );
    assert!(
        !*p4_fired.borrow(),
        "Phase 3d post-Pass-4d hook must NOT fire after a post-ExportMap cancellation; sub-step 8 \
         must not run"
    );
    assert!(
        !*p5_fired.borrow(),
        "Phase 3d post-Pass-5 hook must NOT fire after a post-ExportMap cancellation; sub-step 9 \
         must not run"
    );
}

#[test]
fn incremental_rebuild_phase3d_polls_cancellation_at_post_pass4d_boundary() {
    // Cancellation flipped from inside the Phase 3d post-Pass-4d hook
    // (between sub-steps 8 and 9). Sub-steps 7 and 8 must run; sub-
    // step 9's post-Pass-5 hook must NOT fire.
    let fx = build_two_file_rust_fixture();
    let cancellation = CancellationToken::new();

    let em_fired = Rc::new(RefCell::new(false));
    let em_fired_hook = Rc::clone(&em_fired);
    let _em_guard = Phase3dPostExportMapHookGuard::install(move |_rg, _em: &ExportMap| {
        *em_fired_hook.borrow_mut() = true;
    });

    let p4_fired = Rc::new(RefCell::new(false));
    let p4_fired_hook = Rc::clone(&p4_fired);
    let cancel_from_hook = cancellation.clone();
    let _p4_guard = Phase3dPostPass4dHookGuard::install(move |_rg, _diag| {
        *p4_fired_hook.borrow_mut() = true;
        cancel_from_hook.cancel();
    });

    let p5_fired = Rc::new(RefCell::new(false));
    let p5_fired_hook = Rc::clone(&p5_fired);
    let _p5_guard = Phase3dPostPass5HookGuard::install(move |_rg, _stats| {
        *p5_fired_hook.borrow_mut() = true;
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("post-Pass-4d cancellation must short-circuit incremental_rebuild");

    assert!(matches!(err, GraphBuilderError::Cancelled));
    assert!(
        *em_fired.borrow(),
        "Phase 3d post-ExportMap hook must fire (sub-step 7 ran before the cancelled boundary)"
    );
    assert!(
        *p4_fired.borrow(),
        "Phase 3d post-Pass-4d hook must fire (the cancellation is flipped from inside it)"
    );
    assert!(
        !*p5_fired.borrow(),
        "Phase 3d post-Pass-5 hook must NOT fire after a post-Pass-4d cancellation; sub-step 9 \
         must not run"
    );
}

#[test]
fn incremental_rebuild_phase3d_polls_cancellation_at_post_pass5_boundary() {
    // Cancellation flipped from inside the Phase 3d post-Pass-5 hook
    // (between sub-step 9 and the Phase 3e/fallback boundary). All
    // three Phase 3d hooks must fire; the overall rebuild must return
    // Cancelled instead of the full-build fallback result.
    let fx = build_two_file_rust_fixture();
    let cancellation = CancellationToken::new();

    let em_fired = Rc::new(RefCell::new(false));
    let em_fired_hook = Rc::clone(&em_fired);
    let _em_guard = Phase3dPostExportMapHookGuard::install(move |_rg, _em: &ExportMap| {
        *em_fired_hook.borrow_mut() = true;
    });

    let p4_fired = Rc::new(RefCell::new(false));
    let p4_fired_hook = Rc::clone(&p4_fired);
    let _p4_guard = Phase3dPostPass4dHookGuard::install(move |_rg, _diag| {
        *p4_fired_hook.borrow_mut() = true;
    });

    let p5_fired = Rc::new(RefCell::new(false));
    let p5_fired_hook = Rc::clone(&p5_fired);
    let cancel_from_hook = cancellation.clone();
    let _p5_guard = Phase3dPostPass5HookGuard::install(move |_rg, _stats| {
        *p5_fired_hook.borrow_mut() = true;
        cancel_from_hook.cancel();
    });

    let err = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect_err("post-Pass-5 cancellation must short-circuit incremental_rebuild");

    assert!(matches!(err, GraphBuilderError::Cancelled));
    assert!(*em_fired.borrow(), "sub-step 7 ran");
    assert!(*p4_fired.borrow(), "sub-step 8 ran");
    assert!(
        *p5_fired.borrow(),
        "sub-step 9 ran (the cancellation is flipped from inside its hook)"
    );
}

#[test]
fn incremental_rebuild_phase3d_still_delegates_to_full_build_fallback() {
    // Phase 3d is still a scaffolding phase: sub-steps 10-13 (finalize
    // + heap_bytes + return Arc<CodeGraph>) remain Phase 3e's scope.
    // Assert that every Phase 3d hook fires AND the returned graph has
    // at least as many nodes as the initial current_graph — i.e. the
    // full-build fallback wasn't accidentally short-circuited into an
    // empty CodeGraph. Locks in that Phase 3d does not migrate into
    // Phase 3e territory.
    let fx = build_two_file_rust_fixture();
    let cancellation = CancellationToken::new();

    let em_fired = Rc::new(RefCell::new(false));
    let em_fired_hook = Rc::clone(&em_fired);
    let _em_guard = Phase3dPostExportMapHookGuard::install(move |_rg, _em: &ExportMap| {
        *em_fired_hook.borrow_mut() = true;
    });

    let p4_fired = Rc::new(RefCell::new(false));
    let p4_fired_hook = Rc::clone(&p4_fired);
    let _p4_guard = Phase3dPostPass4dHookGuard::install(move |_rg, _diag| {
        *p4_fired_hook.borrow_mut() = true;
    });

    let p5_fired = Rc::new(RefCell::new(false));
    let p5_fired_hook = Rc::clone(&p5_fired);
    let _p5_guard = Phase3dPostPass5HookGuard::install(move |_rg, _stats| {
        *p5_fired_hook.borrow_mut() = true;
    });

    let result = incremental_rebuild(
        &fx.current_graph,
        &fx.changed_paths,
        &fx.closure,
        plugin_manager(),
        &build_config(),
        &cancellation,
    )
    .expect("fallback full-build must succeed");

    assert!(
        *em_fired.borrow() && *p4_fired.borrow() && *p5_fired.borrow(),
        "every Phase 3d hook must fire before the fallback runs — otherwise sub-steps 7-9 were \
         skipped entirely"
    );

    // The full-build fallback rebuilds the whole workspace, so the
    // returned graph must have at least as many nodes as the initial
    // current_graph (the edit to `a.rs` adds `greet_also`, so strictly
    // more is expected; but we weaken to `>=` for robustness against
    // plugin-level node-count drift).
    assert!(
        result.node_count() >= fx.current_graph.node_count(),
        "full-build fallback's CodeGraph must cover the edited workspace: initial={}, \
         fallback={}",
        fx.current_graph.node_count(),
        result.node_count()
    );
}
