//! U19 — Snapshot-size gate for Phase A C indirect-call precision.
//!
//! SPEC §5.2 commits to a `≤ +10%` snapshot-size budget vs the pre-Phase-A
//! baseline. DESIGN §14.2 enumerates the additive bytes:
//!
//! * `NodeFlags::ADDRESS_TAKEN` / `NodeFlags::CALLSITE_PROMISCUOUS` marker
//!   bits stored under [`NodeMetadataStore::StoredEntry`].
//! * The `resolved_via: ResolvedVia` field on [`EdgeKind::Calls`].
//! * The entire [`CIndirectSideTables`] envelope slot (`fn_signature`,
//!   `struct_field_fnptr`, `local_var_type`, `local_scope_indices`,
//!   `bindings_by_field`, `pending_callsites`).
//!
//! The integration test pinned here makes the +10% budget a falsifiable
//! `cargo test --workspace` gate (so it runs on every CI workspace test,
//! not just the bench-time CI run).
//!
//! # Methodology
//!
//! Build the same C-only workspace once, save its snapshot twice:
//!
//! * **A — Phase-A-free baseline**: strip every Phase-A-introduced piece
//!   of state via [`CodeGraph::clear_phase_a_state_for_test`]. Side
//!   tables = `None`, marker bits cleared on every node, every
//!   `EdgeKind::Calls.resolved_via` collapsed to
//!   [`ResolvedVia::Direct`]. Re-save and measure byte count.
//! * **B — populated**: re-save the original graph as-is and measure
//!   byte count.
//!
//! Assert `B.len() <= (A.len() as f64 * 1.10) as usize`.
//!
//! The fixture is `test-fixtures/c-icall-precision/linux-driver-subset/`
//! — the load-bearing Phase A fixture (DESIGN §13 / U16) that produces
//! every Phase-A-side-table shape (bindings, callsites, marker flags,
//! resolved Calls edges) at non-trivial fan-out.
//!
//! ## Synthetic regression test
//!
//! `phase_a_snapshot_size_gate_detects_regression` proves the gate logic
//! itself catches a `>+10%` regression. It inflates the side tables
//! (duplicates every `pending_callsites` entry 32× and every
//! `bindings_by_field` value vector 32×) and asserts that the resulting
//! delta exceeds `+10%` AND that
//! [`exceeds_snapshot_size_budget`] correctly returns `true`.

use std::fs;
use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::save_to_path;
use sqry_core::graph::unified::{NodeId, StringId};
use sqry_core::plugin::PluginManager;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test-only gate helper — pinned outside the assertion sites so the synthetic
// regression test can exercise the same predicate the +10% gate enforces.
// ---------------------------------------------------------------------------

/// Returns `true` when `populated_bytes` exceeds the +10% snapshot-size
/// budget vs `baseline_bytes`.
///
/// Pinned outside the [`phase_a_snapshot_size_within_10_percent`]
/// assertion so the synthetic regression test (which inflates the side
/// tables and asserts the gate fires) can exercise the same predicate.
#[must_use]
fn exceeds_snapshot_size_budget(baseline_bytes: u64, populated_bytes: u64) -> bool {
    // Budget = ceil(baseline * 1.10). Multiply in f64 to avoid u64
    // wraparound on small baselines; compare against the integer ceiling.
    let budget = (baseline_bytes as f64 * 1.10) as u64;
    populated_bytes > budget
}

// ---------------------------------------------------------------------------
// Fixture path resolution
// ---------------------------------------------------------------------------

/// Resolve the absolute path to the Phase A `linux-driver-subset`
/// fixture. The integration test is rooted under `sqry-core/tests/`, so
/// the workspace root is two parents up from `CARGO_MANIFEST_DIR`.
fn fixture_root() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("sqry-core has a workspace parent")
        .to_path_buf();
    workspace_root.join("test-fixtures/c-icall-precision/linux-driver-subset")
}

/// Build the workspace under `root` with only the C plugin registered.
fn build_c_only_workspace(root: &Path) -> CodeGraph {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build must succeed for U19 fixture")
}

/// Save `graph` to `path` and return the resulting file's byte count.
fn save_and_measure(graph: &CodeGraph, path: &Path) -> u64 {
    save_to_path(graph, path).expect("save_to_path must succeed for U19 baseline");
    fs::metadata(path)
        .expect("snapshot file must exist after save")
        .len()
}

// ---------------------------------------------------------------------------
// Test 1 — load-bearing gate
// ---------------------------------------------------------------------------

/// SPEC §5.2 / DESIGN §14.2: snapshot byte count of a populated Phase A
/// build must be `≤ 1.10×` the byte count of the same build with every
/// Phase-A-introduced piece of state stripped.
#[test]
fn phase_a_snapshot_size_within_10_percent() {
    let _ = env_logger::builder().is_test(true).try_init();

    let fixture = fixture_root();
    assert!(
        fixture.exists(),
        "Phase A fixture must exist at {}",
        fixture.display(),
    );

    // Build once — this is the populated, post-Phase-A graph.
    let populated_graph = build_c_only_workspace(&fixture);

    // Save the populated graph and measure.
    let tmp = TempDir::new().expect("tempdir");
    let populated_path = tmp.path().join("snapshot.populated.sqry");
    let populated_bytes = save_and_measure(&populated_graph, &populated_path);

    // Now strip every Phase-A-introduced piece of state and save again.
    // The same `CodeGraph` is reused (cloned to preserve the populated
    // copy for diagnostics on failure).
    let mut stripped_graph = populated_graph.clone();
    stripped_graph.clear_phase_a_state_for_test();

    let baseline_path = tmp.path().join("snapshot.baseline.sqry");
    let baseline_bytes = save_and_measure(&stripped_graph, &baseline_path);

    let delta_pct = if baseline_bytes == 0 {
        f64::INFINITY
    } else {
        ((populated_bytes as f64 - baseline_bytes as f64) / baseline_bytes as f64) * 100.0
    };

    assert!(
        !exceeds_snapshot_size_budget(baseline_bytes, populated_bytes),
        "Phase A snapshot-size gate breached: baseline = {baseline_bytes} bytes, \
         populated = {populated_bytes} bytes, delta = {delta_pct:+.2}% (budget: +10.00%)",
    );

    // Diagnostic so CI logs surface the actual delta even on success.
    eprintln!(
        "phase_a_snapshot_size_within_10_percent: baseline={baseline_bytes} \
         populated={populated_bytes} delta={delta_pct:+.2}% (budget +10.00%)",
    );
}

// ---------------------------------------------------------------------------
// Test 2 — synthetic regression detection
// ---------------------------------------------------------------------------

/// Prove the gate predicate ([`exceeds_snapshot_size_budget`]) catches a
/// `>+10%` regression. Build the populated graph, inflate the side tables
/// (duplicate every `pending_callsites` entry 32× and every
/// `bindings_by_field` value-vector 32×), re-save, and assert that
/// (a) the actual delta exceeds the budget and (b) the gate predicate
/// returns `true`.
///
/// This makes the gate falsifiable inside `cargo test --workspace`: if a
/// future change accidentally clamps the gate to "always pass", this
/// test fails.
#[test]
fn phase_a_snapshot_size_gate_detects_regression() {
    let _ = env_logger::builder().is_test(true).try_init();

    let fixture = fixture_root();
    assert!(
        fixture.exists(),
        "Phase A fixture must exist at {}",
        fixture.display(),
    );

    let baseline_graph = build_c_only_workspace(&fixture);
    let tmp = TempDir::new().expect("tempdir");

    // Stripped baseline — what a pre-Phase-A build would have looked like.
    let mut stripped = baseline_graph.clone();
    stripped.clear_phase_a_state_for_test();
    let baseline_path = tmp.path().join("snapshot.baseline.sqry");
    let baseline_bytes = save_and_measure(&stripped, &baseline_path);

    // Inflated graph — pathological regression (32× duplication of every
    // side-table entry). Mutates a clone to keep `baseline_graph`
    // untouched for diagnostics on failure.
    let mut inflated = baseline_graph.clone();
    {
        let slot = inflated.c_indirect_tables_mut();
        let tables = slot
            .as_mut()
            .expect("Phase A fixture must produce a Some(c_indirect_tables) on build");

        // Duplicate every callsite 32×.
        let original_callsites = tables.pending_callsites.clone();
        for _ in 0..31 {
            tables.pending_callsites.extend(original_callsites.clone());
        }

        // Duplicate every binding vector's entries 32×.
        let bindings_snapshot = tables.bindings_by_field.clone();
        for (key, value_vec) in bindings_snapshot {
            let slot_vec = tables.bindings_by_field.entry(key).or_default();
            for _ in 0..31 {
                slot_vec.extend(value_vec.clone());
            }
        }

        // Duplicate every fn_signature entry's *value* 32× by injecting
        // synthetic keys. The exact keys don't need to resolve — we are
        // only forcing wire bytes onto the snapshot.
        let mut fn_sig_dupes: Vec<(NodeId, StringId)> = Vec::new();
        for (nid, sig) in &tables.fn_signature {
            for ginc in 1..=31u64 {
                fn_sig_dupes.push((
                    NodeId::new(
                        nid.index().wrapping_add(ginc as u32 * 1_000_000),
                        nid.generation(),
                    ),
                    *sig,
                ));
            }
        }
        for (nid, sig) in fn_sig_dupes {
            tables.fn_signature.insert(nid, sig);
        }
    }

    let inflated_path = tmp.path().join("snapshot.inflated.sqry");
    let inflated_bytes = save_and_measure(&inflated, &inflated_path);

    let delta_pct =
        ((inflated_bytes as f64 - baseline_bytes as f64) / baseline_bytes as f64) * 100.0;

    assert!(
        exceeds_snapshot_size_budget(baseline_bytes, inflated_bytes),
        "synthetic +32× side-table inflation MUST exceed the +10% budget; \
         baseline = {baseline_bytes}, inflated = {inflated_bytes}, delta = {delta_pct:+.2}%",
    );

    // Cross-check: the predicate must agree with a hand-computed
    // comparison against the +10% threshold.
    let hand_check = (inflated_bytes as f64) > (baseline_bytes as f64 * 1.10);
    assert_eq!(
        exceeds_snapshot_size_budget(baseline_bytes, inflated_bytes),
        hand_check,
        "exceeds_snapshot_size_budget must agree with a hand-computed \
         (inflated > baseline * 1.10) comparison",
    );

    eprintln!(
        "phase_a_snapshot_size_gate_detects_regression: baseline={baseline_bytes} \
         inflated={inflated_bytes} delta={delta_pct:+.2}% (gate correctly flagged regression)",
    );
}
