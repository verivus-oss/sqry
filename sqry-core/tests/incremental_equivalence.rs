//! §E property-based equivalence harness for Task 4 Gate 0a.
//!
//! For every proptest case, the harness:
//!
//! 1. Picks a fixture from [`incremental_edit_ops::FIXTURES`] and a
//!    sequence of 1..=20 [`EditOp`]s valid for that fixture.
//! 2. Copies the fixture into a **baseline** tempdir, applies every edit
//!    in one go, and builds the graph with [`build_unified_graph`].
//! 3. Copies the fixture into an **incremental** tempdir, builds the
//!    initial graph, then applies each edit one at a time, calling
//!    [`incremental_rebuild`] after each edit to carry state forward.
//! 4. Compares the two final graphs via
//!    [`assert_graph_semantically_equivalent`] and
//!    [`assert_build_errors_equivalent`].
//!
//! Because the Gate 0a stub of [`incremental_rebuild`] delegates to a
//! full rebuild, divergence between the two paths would prove a defect
//! in the *harness itself* (keying, canonicalisation, copy logic,
//! assertion set arithmetic). The [planted-bug verification
//! artefact][pb] locks in that contract.
//!
//! When Gates 0b–0d and Steps 1–6 land, the stub is swapped out for the
//! real engine — the harness then becomes the primary CI gate that
//! proves the incremental engine is observationally indistinguishable
//! from a full rebuild.
//!
//! [pb]: ../../../docs/reviews/sqryd-daemon/2026-04-16/task-4-gate-0a-planted-bug_verification.md
//! [`build_unified_graph`]: sqry_core::graph::unified::build::build_unified_graph
//! [`incremental_rebuild`]: sqry_core::graph::unified::build::incremental::incremental_rebuild
//! [`EditOp`]: crate::support::incremental_edit_ops::EditOp
//! [`assert_graph_semantically_equivalent`]: crate::support::incremental_equivalence::assert_graph_semantically_equivalent
//! [`assert_build_errors_equivalent`]: crate::support::incremental_equivalence::assert_build_errors_equivalent

#![allow(clippy::cast_possible_truncation)] // Benchmark scaling only.

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use proptest::prelude::*;
use proptest::test_runner::TestRunner;
use tempfile::TempDir;

use sqry_core::graph::unified::build::incremental::{
    compute_reverse_dep_closure, incremental_rebuild,
};
use sqry_core::graph::unified::build::{BuildConfig, CancellationToken, build_unified_graph};
use sqry_core::graph::unified::compaction::{Direction, build_compacted_csr, snapshot_edges};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::publish::assert_publish_bijection;
use sqry_core::plugin::PluginManager;

use support::incremental_edit_ops::{
    EditOp, FIXTURES, FixtureSpec, any_edit_sequence, copy_fixture,
};
use support::incremental_equivalence::{
    assert_build_errors_equivalent, assert_graph_semantically_equivalent, build_sem_graph,
    canonicalize_workspace_root, sem_graph_from_result,
};

// ----------------------------------------------------------------------
// Plugin registration
// ----------------------------------------------------------------------

/// Build a [`PluginManager`] with every plugin the Gate 0a fixtures need.
/// Registered once per process via [`OnceLock`] so proptest cases do not
/// pay registration cost repeatedly; the manager itself is cheap to clone
/// but plugins can carry tree-sitter parsers that warm up on first use.
///
/// We intentionally register plugins in deterministic order so the graph
/// build produces the same canonical plugin set every time.
fn plugin_manager() -> &'static PluginManager {
    static MANAGER: OnceLock<PluginManager> = OnceLock::new();
    MANAGER.get_or_init(|| {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
        manager.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
        manager.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::new()));
        manager.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::new()));
        manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::new()));
        manager.register_builtin(Box::new(sqry_lang_java::JavaPlugin::new()));
        manager
    })
}

/// Absolute path to the source-tree `tests/fixtures/incremental` directory.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("incremental")
}

/// Build a graph for `workspace`. Thin wrapper around
/// [`build_unified_graph`] that pins the harness to a deterministic
/// build configuration, then CSR-compacts edges to match the
/// post-publish representation `incremental_rebuild` produces.
///
/// # Why compact here
///
/// `build_unified_graph` leaves edges in the delta tier (no CSR). Users
/// never consume that raw state — every production surface wraps the
/// call in compaction:
///
/// * `sqry index` / CLI invocations route through
///   `persist_and_analyze_graph`, which runs [`snapshot_edges`] +
///   [`build_compacted_csr`] + `swap_csrs_and_clear_deltas` before
///   returning.
/// * The daemon's `WorkspaceManager` publishes graphs produced by
///   `incremental_rebuild`, which walks the finalize contract
///   (`RebuildGraph::finalize` step 9 runs the identical compaction).
///
/// The §E harness compares observed edge-set behaviour between the
/// full-rebuild baseline and the incremental-rebuild candidate. Before
/// compaction the two sides expose different `edges_from` semantics:
///
/// * Delta-only: `build_delta_lww_from_source` picks ONE
///   `DeltaEdge` per `(source, target, kind)` key (the highest seq) and
///   drops every earlier DeltaEdge's `spans` vector. Multi-call patterns
///   (e.g. `distance_squared_from_origin` calling `square` twice) lose
///   all but one call-site span.
/// * CSR-compacted: `merge_delta_edges` groups same-key Add edges and
///   concatenates their span vectors into the single surviving
///   `MergedEdge`. Every call-site span survives.
///
/// That asymmetry is a pre-existing property of the delta-vs-CSR
/// representation split (not a Phase 3e regression), but it breaks the
/// harness's `EdgeSemKey::span_discriminator` comparison because the
/// baseline reports one span and the candidate reports many. Matching
/// the production path — compact both sides — lets the harness observe
/// the canonical representation and catches actual incremental-rebuild
/// divergences without false positives on span aggregation.
fn build_and_capture(workspace: &Path) -> Result<CodeGraph> {
    let config = BuildConfig::default();
    let graph = build_unified_graph(workspace, plugin_manager(), &config)?;
    compact_graph_to_csr(&graph);
    Ok(graph)
}

/// Run the production CSR-compaction sequence in-place on `graph`.
///
/// Mirrors the sequence in
/// `sqry-core/src/graph/unified/build/entrypoint.rs::persist_and_analyze_graph`
/// (Step 3, "Compact edge stores into CSR before persistence"). Both
/// directions are snapshotted, compacted in parallel via `rayon::join`,
/// and atomically swapped into the store while the deltas are cleared.
///
/// Panics on CSR build failure — the production call site uses
/// `?`-propagation, but inside a test harness a compaction failure on a
/// well-formed `CodeGraph` indicates a storage-layer regression that
/// should surface as a hard panic rather than silently returning a
/// partially-compacted graph.
fn compact_graph_to_csr(graph: &CodeGraph) {
    let node_count = graph.node_count();
    let forward_snapshot = {
        let forward_store = graph.edges().forward();
        snapshot_edges(&forward_store, node_count)
    };
    let reverse_snapshot = {
        let reverse_store = graph.edges().reverse();
        snapshot_edges(&reverse_store, node_count)
    };
    let (forward_result, reverse_result) = rayon::join(
        || build_compacted_csr(&forward_snapshot, Direction::Forward),
        || build_compacted_csr(&reverse_snapshot, Direction::Reverse),
    );
    let (forward_csr, _) = forward_result
        .expect("harness compaction: forward CSR build should succeed on well-formed graph");
    let (reverse_csr, _) = reverse_result
        .expect("harness compaction: reverse CSR build should succeed on well-formed graph");
    graph
        .edges()
        .swap_csrs_and_clear_deltas(forward_csr, reverse_csr);
}

// ----------------------------------------------------------------------
// Harness implementation
// ----------------------------------------------------------------------

/// Apply every operator in `edits` once against `workspace`. Skipped
/// operators still count as applied — they're part of the sequence that
/// gets replayed in the incremental path, so the shape of the mutation
/// must match.
fn apply_all_edits(workspace: &Path, edits: &[EditOp]) -> Result<Vec<EditOutcome>> {
    edits
        .iter()
        .map(|op| EditOutcome::from(op, workspace))
        .collect()
}

/// Record of what an applied edit actually changed on disk. The
/// [`changed_paths`] list is what the harness feeds into the reverse-dep
/// closure computation and passes to `incremental_rebuild`.
struct EditOutcome {
    changed_paths: Vec<PathBuf>,
}

impl EditOutcome {
    fn from(op: &EditOp, workspace: &Path) -> Result<Self> {
        let before_paths = changed_paths_for(op, workspace);
        let _applied = op
            .apply(workspace)
            .map_err(|err| anyhow::anyhow!("apply({op:?}) failed: {err}"))?;
        // RenameFile may have introduced new paths; compute again for a
        // conservative reporting surface.
        let after_paths = changed_paths_for(op, workspace);
        let mut merged: Vec<PathBuf> = before_paths;
        for p in after_paths {
            if !merged.contains(&p) {
                merged.push(p);
            }
        }
        Ok(EditOutcome {
            changed_paths: merged,
        })
    }
}

fn changed_paths_for(op: &EditOp, workspace: &Path) -> Vec<PathBuf> {
    let resolve = |rel: &str| workspace.join(rel);
    match op {
        EditOp::AddFunction { rel_path, .. }
        | EditOp::RemoveFunction { rel_path, .. }
        | EditOp::RenameSymbol { rel_path, .. }
        | EditOp::AddImport { rel_path, .. }
        | EditOp::RemoveImport { rel_path, .. }
        | EditOp::AddExternBlock { rel_path, .. }
        | EditOp::AddHttpRoute { rel_path, .. }
        | EditOp::AddFile { rel_path, .. }
        | EditOp::RemoveFile { rel_path }
        | EditOp::WhitespaceEdit { rel_path }
        | EditOp::InvalidSyntaxEdit { rel_path } => vec![resolve(rel_path)],
        EditOp::RenameFile {
            old_rel_path,
            new_rel_path,
        } => vec![resolve(old_rel_path), resolve(new_rel_path)],
    }
}

/// Run one proptest case: build baseline, then replay edits through
/// `incremental_rebuild`, then compare the final sem-graphs.
fn run_case(spec: &'static FixtureSpec, edits: &[EditOp]) -> Result<()> {
    let source_root = fixtures_root();
    let context = format!(
        "fixture={} edit_count={} ops={}",
        spec.name,
        edits.len(),
        edits
            .iter()
            .map(|op| format!("{:?}", op.kind()))
            .collect::<Vec<_>>()
            .join(","),
    );

    // -------- Baseline path: apply everything then build --------
    let baseline_dir = TempDir::new()?;
    copy_fixture(spec.name, &source_root, baseline_dir.path())?;
    apply_all_edits(baseline_dir.path(), edits)?;
    let baseline_workspace = canonicalize_workspace_root(baseline_dir.path());
    let baseline_result = build_and_capture(&baseline_workspace);

    // Publish-boundary bijection on the baseline — plan §F.3 call site 4.
    // The full-rebuild end inside `build_unified_graph_inner` already
    // asserts this, but re-running it here closes the §E harness
    // contract literally: every graph the harness compares must pass
    // the bijection independently of where it was produced. The §F.2
    // residue check has EXACTLY ONE call site (finalize step 14) per
    // plan §H — the §E harness must therefore only call
    // `assert_publish_bijection` here, never `assert_publish_invariants`.
    if let Ok(baseline_graph) = baseline_result.as_ref() {
        assert_publish_bijection(baseline_graph);
    }

    // -------- Incremental path: per-edit, through incremental_rebuild --------
    let incremental_dir = TempDir::new()?;
    copy_fixture(spec.name, &source_root, incremental_dir.path())?;
    let incremental_workspace = canonicalize_workspace_root(incremental_dir.path());

    let config = BuildConfig::default();
    let mut current_graph: CodeGraph =
        build_unified_graph(&incremental_workspace, plugin_manager(), &config)?;
    // Match `build_and_capture`: run the production CSR-compaction
    // sequence on the initial full-rebuild output so the first
    // `incremental_rebuild` call observes the same canonical starting
    // state the daemon's WorkspaceManager would publish after a
    // warmed-up `sqry index`. Without this, `clone_for_rebuild` would
    // inherit an uncompacted delta whose span semantics differ from the
    // post-finalize output, and the first edit's candidate graph would
    // diverge from the baseline for non-bug reasons (see
    // `build_and_capture` docs for the delta/CSR aggregation asymmetry).
    compact_graph_to_csr(&current_graph);
    // Initial incremental-path build is a full rebuild; apply the §F.3
    // publish-boundary bijection here too so the harness catches any
    // regression before the first edit even lands. Residue is checked
    // at the single finalize step-14 site, not here.
    assert_publish_bijection(&current_graph);

    let mut incremental_result: Result<CodeGraph, String> = Ok(current_graph.clone());

    for op in edits {
        let outcome = EditOutcome::from(op, incremental_dir.path())?;
        // Compute the reverse-dep closure over FileIds that exist in the
        // current graph. Newly-created files have no FileId yet; the real
        // engine discovers them via FS walk. For the stub this is moot.
        let closure = compute_closure_for_paths(&current_graph, &outcome.changed_paths);

        // The §E harness does not test cancellation semantics (that is
        // exercised by dedicated unit tests in `build/cancellation.rs` +
        // `build/incremental.rs`). We pass a fresh default token so every
        // property-run sees an inert, never-cancelled signal — exactly the
        // happy path the harness's invariants depend on.
        let cancellation = CancellationToken::new();
        match incremental_rebuild(
            &current_graph,
            &outcome.changed_paths,
            &closure,
            plugin_manager(),
            &config,
            &cancellation,
        ) {
            Ok(next) => {
                // Every successful incremental rebuild is a publish
                // boundary per plan §F.3 call site 4 — the harness must
                // reject any graph that fails the bijection, not just
                // graphs that diverge semantically from the baseline.
                // The §F.2 residue check already fired at
                // `RebuildGraph::finalize` step 14 against that rebuild's
                // drained tombstone set (the single authoritative
                // residue call site per plan §H); re-running it here
                // against an empty `HashSet` would violate the
                // exactly-one-site contract (Gate 0d iter-1 Major 1).
                assert_publish_bijection(&next);
                current_graph = next;
                // Keep the result cell Ok so a later iteration can still
                // fail the harness when something regresses.
                incremental_result = Ok(current_graph.clone());
            }
            Err(err) => {
                let msg = format!("incremental_rebuild failed at op {op:?}: {err}");
                incremental_result = Err(msg);
                break;
            }
        }
    }

    // -------- Compare outcomes --------
    // Normalise both results to `Result<CodeGraph, String>` so the shared
    // assertion helper can compare the Ok/Err discriminants uniformly.
    let baseline_for_cmp: Result<CodeGraph, String> = baseline_result
        .as_ref()
        .map(CodeGraph::clone)
        .map_err(|e| format!("{e:#}"));
    let incremental_for_cmp: Result<CodeGraph, String> = incremental_result.clone();
    assert_build_errors_equivalent(&baseline_for_cmp, &incremental_for_cmp, &context);

    if let (Some(baseline_sg), Some(candidate_sg)) = (
        sem_graph_from_result(&baseline_result, &baseline_workspace),
        incremental_result
            .as_ref()
            .ok()
            .map(|g| build_sem_graph(g, &incremental_workspace)),
    ) {
        assert_graph_semantically_equivalent(&baseline_sg, &candidate_sg, &context);
    }

    Ok(())
}

fn compute_closure_for_paths(graph: &CodeGraph, paths: &[PathBuf]) -> HashSet<FileId> {
    let mut file_ids = Vec::new();
    for path in paths {
        if let Some(fid) = graph.files().get(path) {
            file_ids.push(fid);
        } else if let Ok(canonical) = std::fs::canonicalize(path)
            && let Some(fid) = graph.files().get(&canonical)
        {
            file_ids.push(fid);
        }
    }
    compute_reverse_dep_closure(&file_ids, graph)
}

// ----------------------------------------------------------------------
// Test budget
// ----------------------------------------------------------------------

/// Resolve the proptest case budget from `PROPTEST_CASES` with a default
/// of 256 — the Gate 0a budget called out in the sqryd daemon design.
///
/// Nightly-scale runs (4,096 cases, seq 1..=50) can override the default
/// via the environment variable.
fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// Resolve the maximum edit-sequence length from `PROPTEST_SEQ_MAX`,
/// defaulting to 20.
fn max_sequence_len() -> usize {
    std::env::var("PROPTEST_SEQ_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

// ----------------------------------------------------------------------
// Proptest suite
// ----------------------------------------------------------------------

fn run_fixture(spec: &'static FixtureSpec) {
    let config = ProptestConfig {
        cases: proptest_cases(),
        // Suppress the "Saving this and other seeds to..." message — it
        // interferes with CI log parsing and the same-seed replay path
        // is still wired up (PROPTEST_REGRESSION_DIR).
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let strategy = any_edit_sequence(spec, max_sequence_len());

    TestRunner::new(config)
        .run(&strategy, |edits: Vec<EditOp>| {
            run_case(spec, &edits).map_err(|err| {
                TestCaseError::fail(format!("run_case errored before semantic check: {err:#}"))
            })
        })
        .unwrap_or_else(|err| panic!("proptest harness for {} failed: {err}", spec.name));
}

#[test]
fn prop_incremental_matches_full_rust_small() {
    run_fixture(fixture_by_name("rust_small"));
}

#[test]
fn prop_incremental_matches_full_multi_lang_ffi() {
    run_fixture(fixture_by_name("multi_lang_ffi"));
}

#[test]
fn prop_incremental_matches_full_ts_http_routes() {
    run_fixture(fixture_by_name("ts_http_routes"));
}

#[test]
fn prop_incremental_matches_full_java_enterprise() {
    run_fixture(fixture_by_name("java_enterprise"));
}

#[test]
fn prop_incremental_matches_full_monorepo_mixed() {
    run_fixture(fixture_by_name("monorepo_mixed"));
}

fn fixture_by_name(name: &str) -> &'static FixtureSpec {
    FIXTURES
        .iter()
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("unknown fixture: {name}"))
}

// ----------------------------------------------------------------------
// Deterministic Phase 3e closure-widening regression tests
// ----------------------------------------------------------------------
//
// The four tests below lock in specific shrinks the proptest suite
// discovered while Phase 3e removed the `build_unified_graph` fallback
// and the reverse-dep closure was still `Imports`-only. Each test
// replays ONE specific edit sequence against ONE specific fixture and
// asserts the incremental path produces a semantically equivalent graph
// to the full rebuild. Unlike the proptest suite, these are cheap to
// run and give a sharply targeted failure message if any of the
// underlying fixes (closure widening, Pass 5 CSR+delta scan,
// Phase 4c-prime path-based tie-break, Phase 4c-prime committed-edge
// retarget) regress. They do NOT replace the proptest suite — they
// supplement it by pinning the known failure shapes.

fn run_deterministic_case(fixture_name: &str, edits: &[EditOp]) {
    run_case(fixture_by_name(fixture_name), edits)
        .unwrap_or_else(|err| panic!("deterministic regression ({fixture_name}) failed: {err:#}"));
}

#[test]
fn regression_rust_small_whitespace_edit_preserves_cross_file_calls() {
    // Pre-fix shape: `main.rs::main → shapes.rs::Point::new` cross-file
    // Calls edge disappeared in the incremental candidate after a
    // WhitespaceEdit on `shapes.rs`. Root cause was `compute_reverse_dep_closure`
    // using `reverse_import_index` (Imports-only): `main.rs` imports
    // `Point` via `rust_small::shapes::Point` which the Rust plugin
    // routes through `lib.rs`, so no direct `Imports` edge existed from
    // `main.rs` into `shapes.rs`. Closure therefore excluded main.rs,
    // `remove_file(shapes.rs)` tombstoned the old `Point::new`, the
    // Calls edge from main was tombstoned with it, and nothing
    // re-registered it. Phase 3e's switch to `reverse_dependency_index`
    // widens the closure to all inter-file edge kinds and fixes this.
    run_deterministic_case(
        "rust_small",
        &[EditOp::WhitespaceEdit {
            rel_path: "shapes.rs".to_string(),
        }],
    );
}

#[test]
fn regression_rust_small_rename_file_preserves_cross_file_calls() {
    // Same mechanism as the whitespace test above but triggered via
    // `RenameFile`. Verifies the closure widening is orthogonal to the
    // specific edit operator — any re-parse of `shapes.rs` needs
    // `main.rs` in the closure, regardless of whether the re-parse was
    // triggered by content, rename, or removal.
    run_deterministic_case(
        "rust_small",
        &[EditOp::RenameFile {
            old_rel_path: "shapes.rs".to_string(),
            new_rel_path: "shapes_renamed.rs".to_string(),
        }],
    );
}

#[test]
fn regression_ts_http_routes_remove_file_rebuilds_webhook_status_target() {
    // Pre-fix shape: `webhook.ts`'s `res::status` stub node survived the
    // incremental rebuild with an empty qualified_name after
    // `RemoveFile{server.ts}` deleted the canonical definition. The bug
    // had two layers:
    //   (a) `reverse_dependency_index` didn't yet walk HttpRequest
    //       cross-file edges, so `webhook.ts` never entered the closure.
    //   (b) Even after closure widening, Phase 4c-prime's tie-break by
    //       `FileId` was non-deterministic across representations.
    // Both are fixed — this test locks the combined behaviour in.
    run_deterministic_case(
        "ts_http_routes",
        &[EditOp::RemoveFile {
            rel_path: "server.ts".to_string(),
        }],
    );
}

#[test]
fn regression_ts_http_routes_add_http_route_preserves_existing_http_links() {
    // Pre-fix shape: `collect_http_requests` in Pass 5 scanned only the
    // edge delta buffer, not CSR. Once the §E harness CSR-compacts
    // graphs before comparison, any HttpRequest edge that landed in CSR
    // was invisible to Pass 5's incremental run. Adding a new HTTP route
    // to `server.ts` re-parses `server.ts` (and its dependents via the
    // widened closure), but the existing HttpRequest edges from
    // `client.ts` into the prior endpoints were stranded because Pass 5
    // couldn't see them. The CSR+delta scan fixes this.
    run_deterministic_case(
        "ts_http_routes",
        &[EditOp::AddHttpRoute {
            rel_path: "server.ts".to_string(),
            method: "GET".to_string(),
            path: "/api/regression_harness".to_string(),
            handler: "harnessRegressionHandler".to_string(),
        }],
    );
}

#[test]
fn regression_ts_http_routes_newly_resolvable_endpoint_links_unchanged_requester() {
    // Scenario Codex called out as NOT covered by closure widening
    // alone: "unchanged requester + newly-resolvable endpoint".
    //
    // Step 1: `AddFile` a new client that issues `fetch("/api/brand_new")`.
    //         The plugin creates a stub `http::/api/brand_new` Module
    //         node owned by the new client file, plus an `HttpRequest`
    //         edge from the client's calling scope into that stub. No
    //         matching Endpoint exists, so Pass 5 cannot resolve the
    //         request to a real route yet.
    //
    // Step 2: `AddHttpRoute` registers `/api/brand_new` on `server.ts`.
    //         `server.ts` is in the rebuild closure (the edit points at
    //         it directly). The new endpoint node lands in the arena.
    //         Pass 5 runs over the full rebuild plane, sees the
    //         unresolved `HttpRequest` edge from the new client, sees
    //         the matching Endpoint in `server.ts`, and installs the
    //         resolving cross-file `HttpRequest` edge.
    //
    // The new-client file is NOT in the closure of step 2 (nothing that
    // already existed in the pre-step-2 graph points into `server.ts`
    // from the new client — its request edge targets the STUB module,
    // not `server.ts`), so the only way for the incremental build to
    // agree with the full rebuild is for Pass 5 to walk the
    // CSR+delta-merged edge view (our Step 4 fix), not the delta buffer
    // alone.
    //
    // This case is the lower-bound justification for keeping the Pass 5
    // CSR+delta scan even after the closure widening — closure widening
    // covers existing cross-file edges; the Pass 5 scan covers newly
    // resolvable edges.
    run_deterministic_case(
        "ts_http_routes",
        &[
            EditOp::AddFile {
                rel_path: "new_client.ts".to_string(),
                content:
                    r#"// Deterministic regression: unchanged-requester + newly-resolvable endpoint.
export async function fetchBrandNew(): Promise<unknown> {
    const response = await fetch("/api/brand_new");
    return response.json();
}
"#
                    .to_string(),
            },
            EditOp::AddHttpRoute {
                rel_path: "server.ts".to_string(),
                method: "GET".to_string(),
                path: "/api/brand_new".to_string(),
                handler: "brandNewHandler".to_string(),
            },
        ],
    );
}
