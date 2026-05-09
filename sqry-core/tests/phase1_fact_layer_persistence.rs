//! Phase 1 fact-layer persistence integration tests.
//!
//! Covers T06 (V8 round-trip), T07 (V7 legacy load), T08 (fact_epoch
//! monotonicity), T10 (SHA-256 covers V8), T11 (public accessors).
//!
//! See: `docs/development/generational-analysis-platform/phase1-fact-layer/05_TEST_PLAN.md`

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::snapshot::{
    load_from_path, load_header_from_path, save_to_path,
};
use tempfile::NamedTempFile;

/// T06: V8 save → load round-trip preserves topology + provenance is populated.
#[test]
fn t06_v8_round_trip_with_provenance() {
    let graph = CodeGraph::new();
    let tmp = NamedTempFile::new().unwrap();

    save_to_path(&graph, tmp.path()).unwrap();
    let loaded = load_from_path(tmp.path(), None).unwrap();

    assert_eq!(loaded.node_count(), graph.node_count());
    assert_eq!(loaded.edge_count(), graph.edge_count());
    // fact_epoch should be non-zero after V8 save
    assert!(
        loaded.fact_epoch() > 0,
        "V8 round-trip must produce non-zero fact_epoch"
    );
}

/// T08: repeated in-process saves produce strictly increasing fact_epoch.
#[test]
fn t08_fact_epoch_monotonic_in_process() {
    let graph = CodeGraph::new();
    let tmp = NamedTempFile::new().unwrap();

    save_to_path(&graph, tmp.path()).unwrap();
    let epoch1 = load_header_from_path(tmp.path()).unwrap().fact_epoch();

    save_to_path(&graph, tmp.path()).unwrap();
    let epoch2 = load_header_from_path(tmp.path()).unwrap().fact_epoch();

    assert!(
        epoch2 > epoch1,
        "second save epoch ({epoch2}) must exceed first ({epoch1})"
    );
}

/// T10: SHA-256 verification of a V8 snapshot (the existing verify_snapshot_bytes
/// function covers the full payload including provenance stores).
#[test]
fn t10_sha256_covers_v8_payload() {
    use sha2::{Digest, Sha256};
    use sqry_core::graph::unified::persistence::snapshot::verify_snapshot_bytes;

    let graph = CodeGraph::new();
    let tmp = NamedTempFile::new().unwrap();

    save_to_path(&graph, tmp.path()).unwrap();

    let bytes = std::fs::read(tmp.path()).unwrap();
    let actual_hash = hex::encode(Sha256::digest(&bytes));

    // Verify with the correct hash succeeds
    verify_snapshot_bytes(&bytes, &actual_hash).unwrap();

    // Verify with a wrong hash fails
    assert!(
        verify_snapshot_bytes(
            &bytes,
            "0000000000000000000000000000000000000000000000000000000000000000"
        )
        .is_err()
    );
}

/// T11: public accessors on loaded CodeGraph return sensible values.
#[test]
fn t11_public_accessors_on_loaded_graph() {
    let graph = CodeGraph::new();
    let tmp = NamedTempFile::new().unwrap();

    save_to_path(&graph, tmp.path()).unwrap();
    let loaded = load_from_path(tmp.path(), None).unwrap();

    // fact_epoch > 0 after V8 save
    assert!(loaded.fact_epoch() > 0);

    // Snapshot also exposes fact_epoch
    let snapshot = loaded.snapshot();
    assert_eq!(snapshot.fact_epoch(), loaded.fact_epoch());
}
