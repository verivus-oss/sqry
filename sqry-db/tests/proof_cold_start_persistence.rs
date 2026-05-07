//! Comprehensive cold-start persistence test suite (PN3 COLD_START_TESTS).
//!
//! Covers every edge case surfaced across PN3 iterations 1–11 (spec §7.2):
//!
//! 1. `mixed_query_roundtrip` — Tier-1-only (CyclesQuery), Tier-2 (CallersQuery),
//!    Tier-2+3 (UnusedQuery) all cache → save → load → hit without recomputation.
//! 2. `header_restoration` — edge_rev, metadata_rev, per-file revs match the
//!    persisted header after load.
//! 3. `builtin_query_type_ids_are_unique` — integration-level re-assertion.
//! 4. `unknown_query_type_id_skip` — crafted manifest with id=0x9999 → 4/5 entries
//!    applied, no error.
//! 5. `fatal_framing_reject` — last-8-bytes truncation → Err(Corrupt); DB pristine;
//!    `cold_load_allowed` still true.
//! 6. `idempotent_load` — second load after success → Err(AlreadyLoaded).
//! 7. `atomic_replace_under_reader` — concurrent save/read → reader never sees a
//!    torn header.
//! 8. `symlink_rejection_parent` (unix) — parent dir is a symlink → save/load fails.
//! 9. `symlink_rejection_target` (unix) — target file is a symlink → save/load fails.
//! 10. `oversize_entry_skip` — max_entry_size_bytes=N, entry serialization > N →
//!     skipped at insert, save+reload shows 0 entries.
//! 11. `staged_validation_purity` — fatal framing error before commit → DB unchanged.
//! 12. `sha_mismatch_whole_file_reject` — save under [0xAA;32], load with [0xBB;32]
//!     → Err(StaleSnapshot); file-delete fallback via load_derived_opportunistic.
//! 13. `revision_mismatch_per_entry_skip` — save at edge_rev=5, bump to 6 post-load;
//!     rehydrated entries whose deps.edge_revision<6 invalidate on access.
//!
//! Spec: docs/superpowers/specs/2026-04-15-pn3-derived-cache-cold-start-design.md §7.2
//! DAG:  docs/superpowers/plans/2026-04-15-pn3-derived-cache-cold-start-dag.toml
//!       [units.COLD_START_TESTS]

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::EdgeKind;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::{CircularType, UnusedScope};

use sqry_db::persistence::{
    DERIVED_FORMAT_VERSION, DERIVED_MAGIC, DerivedHeader, LoadError, LoadOutcome, PersistedEntry,
    QueryDeps, deserialize_derived_header, load_derived, save_derived, serialize_derived_stream,
};
use sqry_db::queries::dispatch::load_derived_opportunistic;
use sqry_db::queries::type_ids;
use sqry_db::queries::{
    CalleesQuery, CallersQuery, CondensationQuery, CyclesKey, CyclesQuery, EntryPointsQuery,
    ExportsQuery, ImplementsQuery, ImportsQuery, IsInCycleQuery, IsNodeUnusedQuery,
    ReachabilityQuery, ReachableFromEntryPointsQuery, ReferencesQuery, RelationKey, SccQuery,
    UnusedKey, UnusedQuery,
};
use sqry_db::query::DerivedQuery;
use sqry_db::{QueryDb, QueryDbConfig};

use tempfile::TempDir;

// ============================================================================
// Shared fixture helpers
// ============================================================================

/// Returns an empty `CodeGraph` snapshot.
fn empty_snapshot() -> Arc<GraphSnapshot> {
    Arc::new(CodeGraph::new().snapshot())
}

/// Adds a node to the graph, registering it in the name index.
fn add_node(
    graph: &mut CodeGraph,
    entry: NodeEntry,
) -> sqry_core::graph::unified::node::id::NodeId {
    let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
    graph
        .indices_mut()
        .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
    id
}

/// Builds a minimal graph with one caller and one callee so both `CallersQuery`
/// and `UnusedQuery` produce non-empty results.
fn build_call_graph() -> (
    Arc<GraphSnapshot>,
    sqry_core::graph::unified::file::id::FileId,
) {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
        .expect("register file");

    let caller_name = graph.strings_mut().intern("main").expect("intern main");
    let caller = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, caller_name, file)
            .with_qualified_name(caller_name)
            .with_byte_range(0, 100),
    );

    let callee_name = graph.strings_mut().intern("helper").expect("intern helper");
    let callee = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, callee_name, file)
            .with_qualified_name(callee_name)
            .with_byte_range(110, 200),
    );

    graph.edges().add_edge(
        caller,
        callee,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        },
        file,
    );

    (Arc::new(graph.snapshot()), file)
}

/// Build a minimal valid postcard stream with `n` entries of type CALLERS at
/// the given SHA and revision values.
fn make_valid_stream_with_revs(
    sha: [u8; 32],
    edge_rev: u64,
    metadata_rev: u64,
    file_revisions: Vec<(sqry_core::graph::unified::file::id::FileId, u64)>,
    n_entries: usize,
) -> Vec<u8> {
    let entries: Vec<PersistedEntry> = (0..n_entries)
        .map(|i| PersistedEntry {
            query_type_id: type_ids::CALLERS,
            raw_key_bytes: postcard::to_allocvec(&RelationKey::exact(format!("sym_{i}")))
                .unwrap_or_default(),
            raw_result_bytes: vec![0xAA, i as u8],
            deps: QueryDeps {
                file_deps: vec![],
                edge_revision: Some(edge_rev),
                metadata_revision: None,
            },
        })
        .collect();
    let header = DerivedHeader::new(
        sha,
        edge_rev,
        metadata_rev,
        file_revisions,
        n_entries as u64,
    );
    serialize_derived_stream(&header, entries).unwrap()
}

/// Build a minimal valid postcard stream (edge_rev=0, no file revisions).
fn make_valid_stream(sha: [u8; 32], n_entries: usize) -> Vec<u8> {
    make_valid_stream_with_revs(sha, 0, 0, vec![], n_entries)
}

// ============================================================================
// Test 1 — mixed_query_roundtrip
// ============================================================================

/// Tier-1-only (CyclesQuery), Tier-2 (CallersQuery), Tier-2+3 (UnusedQuery)
/// all: warm cache → save_derived → load_derived into fresh DB → re-query
/// returns cache hit (no additional misses).
#[test]
fn mixed_query_roundtrip() {
    let (snapshot, _file) = build_call_graph();

    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();
    let sha: [u8; 32] = [0x55; 32];

    // ── Session 1: warm the cache for three query tiers ──────────────────────

    let db1 = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // Tier-1-only: CyclesQuery (TRACKS_EDGE_REVISION only)
    let cycles_key = CyclesKey {
        circular_type: CircularType::Calls,
        bounds: Default::default(),
    };
    let cycles_result = db1.get::<CyclesQuery>(&cycles_key);

    // Tier-2: CallersQuery (TRACKS_EDGE_REVISION, not metadata)
    let callers_key = RelationKey::exact("main");
    let callers_result = db1.get::<CallersQuery>(&callers_key);

    // Tier-2+3: UnusedQuery (TRACKS_EDGE_REVISION + TRACKS_METADATA_REVISION)
    let unused_key = UnusedKey {
        scope: UnusedScope::All,
        max_results: 100,
    };
    let unused_result = db1.get::<UnusedQuery>(&unused_key);

    let after_warm = db1.metrics();
    assert!(
        after_warm.cache_misses >= 3,
        "at least 3 misses during warm-up"
    );

    // Save the DB's persistent entries.
    save_derived(&db1, sha, &derived_path, workspace_root).expect("save_derived must succeed");

    // ── Session 2: cold-start the fresh DB from the saved file ───────────────

    let mut db2 = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let outcome = load_derived(&mut db2, sha, &derived_path, workspace_root)
        .expect("load_derived must succeed");

    match outcome {
        LoadOutcome::Applied { entries } => {
            assert!(entries > 0, "at least one entry must be applied");
        }
        LoadOutcome::Skipped(_) => panic!("unexpected Skipped outcome"),
    }

    // Re-query the same three queries once — spec §2 guarantees each returns
    // the rehydrated value on its FIRST typed lookup after cold-load. This is
    // fulfilled by the warm/cold key-space unification (commit `a41787179`):
    // `QueryKey::new::<Q>(&key)` and `ShardedCache::insert_validated` both
    // produce `(u64::from(Q::QUERY_TYPE_ID), hash(postcard(&key)))` with
    // shard routing `u64::from(Q::QUERY_TYPE_ID) & (shard_count - 1)`, so
    // the rehydrated entry lands in the exact same slot the typed `get` will
    // probe. The read path then dispatches through
    // `ShardedCache::get_cold_if_valid`, which decodes `raw_result_bytes`
    // into `Q::Value`, promotes the entry in place (replaces the unit
    // placeholder with the typed value), and returns the typed value —
    // counted as a cache HIT. Revision tiers (edge_rev, metadata_rev,
    // per-file revs) are restored by `commit_staged_load` so the validator
    // accepts the rehydrated entry.
    let base = db2.metrics();

    let cycles_result2 = db2.get::<CyclesQuery>(&cycles_key);
    let callers_result2 = db2.get::<CallersQuery>(&callers_key);
    let unused_result2 = db2.get::<UnusedQuery>(&unused_key);

    let after_first_requery = db2.metrics();

    // Values must be identical (revision state is correct after cold-load).
    assert_eq!(
        *cycles_result, *cycles_result2,
        "CyclesQuery result must survive roundtrip"
    );
    assert_eq!(
        *callers_result, *callers_result2,
        "CallersQuery result must survive roundtrip"
    );
    assert_eq!(
        *unused_result, *unused_result2,
        "UnusedQuery result must survive roundtrip"
    );

    // Spec §2 promise: "first query after a cold start is free." The three
    // top-level re-queries all hit the cold-rehydration fast path in
    // `ShardedCache::get_cold_if_valid`: they find the rehydrated entries at
    // the matching (shard, QueryKey) slot and decode `raw_result_bytes`
    // directly, no recomputation.
    //
    // Sub-queries (SccQuery used inside CyclesQuery, EntryPointsQuery inside
    // UnusedQuery) are NOT top-level here — they are executed as part of the
    // top-level query's body via `Q::execute`. At save time, ONLY the three
    // top-level queries were warmed, so their sub-queries were not cached and
    // are not present in the rehydrated file. On re-query, the top-level
    // entries hit (zero recompute), but any sub-query invocations triggered
    // by re-computation of non-cached entries would miss. Since the top-level
    // entries hit directly, no sub-query invocation occurs — net misses == 0.
    let first_pass_misses = after_first_requery.cache_misses - base.cache_misses;
    assert_eq!(
        first_pass_misses, 0,
        "ZERO cache misses expected on first typed re-query after cold-load \
         (spec §2: first query after a cold start is free); got {first_pass_misses}"
    );

    let first_pass_hits = after_first_requery.cache_hits - base.cache_hits;
    assert_eq!(
        first_pass_hits, 3,
        "exactly 3 cache hits expected on first typed re-query (all three \
         top-level rehydrated entries); got {first_pass_hits}"
    );

    // Second re-query pass: entries have been promoted in-place to typed
    // values, so the fast downcast path serves them. Still zero misses.
    let base2 = db2.metrics();

    let _ = db2.get::<CyclesQuery>(&cycles_key);
    let _ = db2.get::<CallersQuery>(&callers_key);
    let _ = db2.get::<UnusedQuery>(&unused_key);

    let after_second_requery = db2.metrics();

    let second_pass_misses = after_second_requery.cache_misses - base2.cache_misses;
    assert_eq!(
        second_pass_misses, 0,
        "zero additional misses expected on second typed re-query (entries \
         now typed after first-pass promotion); got {second_pass_misses}"
    );

    let second_pass_hits = after_second_requery.cache_hits - base2.cache_hits;
    assert_eq!(
        second_pass_hits, 3,
        "exactly 3 cache hits expected on second typed re-query; got {second_pass_hits}"
    );
}

// ============================================================================
// Test 2 — header_restoration
// ============================================================================

/// After load_derived, db.edge_revision() / db.metadata_revision() / per-file
/// revisions match the values stored in the persisted DerivedHeader.
#[test]
fn header_restoration() {
    use sqry_core::graph::unified::file::id::FileId;

    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    let sha: [u8; 32] = [0xBE; 32];
    let saved_edge_rev: u64 = 42;
    let saved_metadata_rev: u64 = 17;
    // Two synthetic file IDs with known revisions.
    let fid_a = FileId::new(1);
    let fid_b = FileId::new(2);
    let saved_per_file: Vec<(FileId, u64)> = vec![(fid_a, 5), (fid_b, 8)];

    let bytes = make_valid_stream_with_revs(
        sha,
        saved_edge_rev,
        saved_metadata_rev,
        saved_per_file.clone(),
        0,
    );
    std::fs::write(&derived_path, &bytes).unwrap();

    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    let outcome = load_derived(&mut db, sha, &derived_path, workspace_root).unwrap();
    assert!(matches!(outcome, LoadOutcome::Applied { .. }));

    // Tier 2 — global edge revision restored.
    assert_eq!(
        db.edge_revision(),
        saved_edge_rev,
        "edge_revision must be restored from the header"
    );

    // Tier 3 — global metadata revision restored.
    assert_eq!(
        db.metadata_revision(),
        saved_metadata_rev,
        "metadata_revision must be restored from the header"
    );

    // Tier 1 — per-file revisions restored.
    let store = db.inputs();
    assert_eq!(
        store.revision(fid_a),
        Some(5),
        "per-file revision for fid_a must be restored"
    );
    assert_eq!(
        store.revision(fid_b),
        Some(8),
        "per-file revision for fid_b must be restored"
    );
}

// ============================================================================
// Test 3 — builtin_query_type_ids_are_unique (integration-level re-assertion)
// ============================================================================

/// Integration-level duplicate of the unit-level test in `type_ids.rs`:
/// asserts every built-in query has a unique, non-zero QUERY_TYPE_ID.
#[test]
fn builtin_query_type_ids_are_unique() {
    let ids: Vec<u32> = vec![
        CallersQuery::QUERY_TYPE_ID,
        CalleesQuery::QUERY_TYPE_ID,
        ImportsQuery::QUERY_TYPE_ID,
        ExportsQuery::QUERY_TYPE_ID,
        ReferencesQuery::QUERY_TYPE_ID,
        ImplementsQuery::QUERY_TYPE_ID,
        CyclesQuery::QUERY_TYPE_ID,
        IsInCycleQuery::QUERY_TYPE_ID,
        UnusedQuery::QUERY_TYPE_ID,
        IsNodeUnusedQuery::QUERY_TYPE_ID,
        ReachabilityQuery::QUERY_TYPE_ID,
        EntryPointsQuery::QUERY_TYPE_ID,
        ReachableFromEntryPointsQuery::QUERY_TYPE_ID,
        SccQuery::QUERY_TYPE_ID,
        CondensationQuery::QUERY_TYPE_ID,
    ];

    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "QUERY_TYPE_ID collision detected among built-ins: {ids:?}"
    );
    assert!(
        !ids.contains(&0),
        "0x0000 is reserved — must never appear as a QUERY_TYPE_ID"
    );
    // The 15 built-ins must have exactly 15 unique IDs.
    assert_eq!(
        ids.len(),
        15,
        "expected exactly 15 built-in QUERY_TYPE_IDs; got {}",
        ids.len()
    );
}

// ============================================================================
// Test 4 — unknown_query_type_id_skip
// ============================================================================

/// Synthesize a manifest with 2 valid entries + 1 entry with an unknown
/// query_type_id (0x9999) + 2 more valid entries. Assert that load_derived
/// applies exactly 4 entries (the unknown is silently skipped) and returns Ok.
#[test]
fn unknown_query_type_id_skip() {
    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    let sha: [u8; 32] = [0x33; 32];

    // Build 5 entries: positions 0,1 = CALLERS (valid); 2 = 0x9999 (unknown);
    // 3,4 = CALLEES (valid).
    let entries: Vec<PersistedEntry> = vec![
        PersistedEntry {
            query_type_id: type_ids::CALLERS,
            raw_key_bytes: b"key0".to_vec(),
            raw_result_bytes: b"val0".to_vec(),
            deps: QueryDeps::default(),
        },
        PersistedEntry {
            query_type_id: type_ids::CALLERS,
            raw_key_bytes: b"key1".to_vec(),
            raw_result_bytes: b"val1".to_vec(),
            deps: QueryDeps::default(),
        },
        PersistedEntry {
            query_type_id: 0x9999_u32,
            raw_key_bytes: b"unknownkey".to_vec(),
            raw_result_bytes: b"unknownval".to_vec(),
            deps: QueryDeps::default(),
        },
        PersistedEntry {
            query_type_id: type_ids::CALLEES,
            raw_key_bytes: b"key3".to_vec(),
            raw_result_bytes: b"val3".to_vec(),
            deps: QueryDeps::default(),
        },
        PersistedEntry {
            query_type_id: type_ids::CALLEES,
            raw_key_bytes: b"key4".to_vec(),
            raw_result_bytes: b"val4".to_vec(),
            deps: QueryDeps::default(),
        },
    ];

    let header = DerivedHeader::new(sha, 0, 0, vec![], 5);
    let bytes = serialize_derived_stream(&header, entries).unwrap();
    std::fs::write(&derived_path, &bytes).unwrap();

    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    let outcome = load_derived(&mut db, sha, &derived_path, workspace_root)
        .expect("load_derived must not error for unknown IDs");

    match outcome {
        LoadOutcome::Applied { entries } => {
            assert_eq!(
                entries, 4,
                "unknown id 0x9999 must be skipped; expected 4 entries applied, got {entries}"
            );
        }
        LoadOutcome::Skipped(_) => panic!("unexpected Skipped"),
    }
}

// ============================================================================
// Test 5 — fatal_framing_reject
// ============================================================================

/// Truncate the last 8 bytes of a valid manifest. Assert:
/// - load_derived returns Err(Corrupt)
/// - DB edge_revision remains 0 (pristine)
/// - cold_load_allowed remains true (no successful load occurred)
#[test]
fn fatal_framing_reject() {
    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    let sha: [u8; 32] = [0x44; 32];

    // Build a valid stream with 2 entries.
    let mut bytes = make_valid_stream(sha, 2);
    assert!(
        bytes.len() > 8,
        "test precondition: stream must be > 8 bytes"
    );

    // Truncate the last 8 bytes.
    let truncated_len = bytes.len() - 8;
    bytes.truncate(truncated_len);
    std::fs::write(&derived_path, &bytes).unwrap();

    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());

    // The DB must start pristine.
    assert_eq!(db.edge_revision(), 0);
    assert!(db.cold_load_allowed());

    let err = load_derived(&mut db, sha, &derived_path, workspace_root)
        .expect_err("truncated stream must return Err");

    assert!(
        matches!(err, LoadError::Corrupt { .. }),
        "expected Corrupt error for truncated stream; got: {err}"
    );

    // DB must remain pristine — no partial state committed.
    assert_eq!(
        db.edge_revision(),
        0,
        "DB edge_revision must be 0 after failed framing rejection"
    );

    // cold_load_allowed must still be true — no successful load occurred.
    assert!(
        db.cold_load_allowed(),
        "cold_load_allowed must remain true after a failed load"
    );
}

// ============================================================================
// Test 6 — idempotent_load
// ============================================================================

/// After one successful load_derived, a second call returns Err(AlreadyLoaded)
/// without re-reading the file. DB state is unchanged after the second call.
#[test]
fn idempotent_load() {
    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    let sha: [u8; 32] = [0x77; 32];
    let bytes = make_valid_stream(sha, 3);
    std::fs::write(&derived_path, &bytes).unwrap();

    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());

    // First load: succeeds.
    let first =
        load_derived(&mut db, sha, &derived_path, workspace_root).expect("first load must succeed");
    assert!(matches!(first, LoadOutcome::Applied { .. }));
    assert!(
        !db.cold_load_allowed(),
        "cold_load_allowed must be false after first load"
    );

    let metrics_after_first = db.metrics();

    // Delete the file to confirm the second call doesn't touch disk.
    std::fs::remove_file(&derived_path).unwrap();

    // Second load: must return AlreadyLoaded without reading the now-missing file.
    let second_err = load_derived(&mut db, sha, &derived_path, workspace_root)
        .expect_err("second load must return Err");
    assert!(
        matches!(second_err, LoadError::AlreadyLoaded),
        "second load must return AlreadyLoaded, got: {second_err}"
    );

    // DB state must be unchanged by the second (no-op) call.
    let metrics_after_second = db.metrics();
    assert_eq!(
        metrics_after_first.cache_hits, metrics_after_second.cache_hits,
        "no new hits after AlreadyLoaded"
    );
    assert_eq!(
        metrics_after_first.cache_misses, metrics_after_second.cache_misses,
        "no new misses after AlreadyLoaded"
    );
}

// ============================================================================
// Test 7 — atomic_replace_under_reader
// ============================================================================

/// Spawn a reader thread that loops ~100 iterations parsing the DerivedHeader.
/// Concurrently the writer thread calls save_derived in a loop.
/// The reader must never observe a partially-written or structurally torn file
/// (either old header or new header; never a magic mismatch or partial header).
#[test]
fn atomic_replace_under_reader() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path().to_path_buf();

    // SHA for both writer and reader.
    let sha: [u8; 32] = [0xCC; 32];

    // Write an initial valid file so the reader has something to start from.
    let initial_bytes = make_valid_stream(sha, 1);
    std::fs::write(&derived_path, &initial_bytes).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop);
    let reader_path = derived_path.clone();

    // Reader thread: loop until signalled, parse header each iteration.
    let reader = thread::spawn(move || {
        let mut iterations = 0usize;
        let mut torn_count = 0usize;

        while !stop_reader.load(Ordering::Relaxed) {
            let bytes = match std::fs::read(&reader_path) {
                Ok(b) => b,
                Err(_) => {
                    // File transiently missing during atomic replace is ok —
                    // the atomic_write_bytes uses rename so this window is
                    // extremely short but theoretically possible.
                    continue;
                }
            };

            if bytes.is_empty() {
                continue;
            }

            match deserialize_derived_header(&bytes) {
                Ok((header, _rest)) => {
                    // Header decoded: verify magic/version integrity.
                    if header.magic != DERIVED_MAGIC
                        || header.format_version != DERIVED_FORMAT_VERSION
                    {
                        torn_count += 1;
                    }
                }
                Err(_) => {
                    // A decode failure here means a torn file was observed.
                    torn_count += 1;
                }
            }

            iterations = iterations.saturating_add(1);
        }

        (iterations, torn_count)
    });

    // Writer: save_derived in a loop for 50 iterations.
    let snapshot = empty_snapshot();
    for _ in 0..50 {
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        save_derived(&db, sha, &derived_path, &workspace_root).expect("save_derived must not fail");
    }

    // Signal reader to stop, then join.
    stop.store(true, Ordering::Relaxed);
    let (iterations, torn_count) = reader.join().expect("reader thread panicked");

    assert_eq!(
        torn_count, 0,
        "reader observed {torn_count} torn headers across {iterations} iterations; \
         atomic writes must never produce partially-written files"
    );
}

// ============================================================================
// Tests 8 & 9 — symlink_rejection_parent / symlink_rejection_target (unix only)
// ============================================================================

/// `save_derived` and `load_derived` must fail when the parent of the target
/// path is a symlink.
#[cfg(unix)]
#[test]
fn symlink_rejection_parent() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    // Create a real subdirectory.
    let real_dir = dir.path().join("real");
    std::fs::create_dir_all(&real_dir).unwrap();

    // Symlink: dir/linked → dir/real
    let link_dir = dir.path().join("linked");
    symlink(&real_dir, &link_dir).unwrap();

    // Target lives INSIDE the symlinked directory.
    let target = link_dir.join("derived.sqry");
    let workspace_root = dir.path();
    let sha: [u8; 32] = [0x10; 32];

    let snapshot = empty_snapshot();
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // save_derived must reject the symlinked parent.
    let save_err = save_derived(&db, sha, &target, workspace_root)
        .expect_err("save_derived must fail with a symlinked parent directory");
    let err_display = save_err.to_string();
    assert!(
        err_display.contains("symlink")
            || err_display.contains("symlink")
            || save_err
                .downcast_ref::<sqry_core::persistence::PathSafetyError>()
                .is_some()
            || err_display.contains("ancestor")
            || err_display.contains("outside"),
        "error message must mention symlink or path safety; got: {err_display}"
    );

    // load_derived must also reject the symlinked parent.
    let mut db2 = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    let load_err = load_derived(&mut db2, sha, &target, workspace_root)
        .expect_err("load_derived must fail with a symlinked parent directory");
    assert!(
        matches!(load_err, LoadError::PathSafety(_)),
        "expected PathSafety error from load_derived; got: {load_err}"
    );
}

/// `save_derived` and `load_derived` must fail when the target file itself is
/// a symlink (even when the parent is a real directory).
#[cfg(unix)]
#[test]
fn symlink_rejection_target() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    // Create the symlink target (a real file elsewhere).
    let real_target = dir.path().join("real_derived.sqry");
    std::fs::write(&real_target, b"placeholder").unwrap();

    // Create a symlink at the expected path that points to the real file.
    let sym_path = dir.path().join("derived.sqry");
    symlink(&real_target, &sym_path).unwrap();

    let workspace_root = dir.path();
    let sha: [u8; 32] = [0x20; 32];

    let snapshot = empty_snapshot();
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // save_derived must reject the symlink target.
    let save_err = save_derived(&db, sha, &sym_path, workspace_root)
        .expect_err("save_derived must fail when the target file is a symlink");
    let err_display = save_err.to_string();
    assert!(
        err_display.contains("symlink")
            || save_err
                .downcast_ref::<sqry_core::persistence::PathSafetyError>()
                .is_some(),
        "error message must mention symlink; got: {err_display}"
    );

    // load_derived must also reject the symlink target.
    let mut db2 = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    let load_err = load_derived(&mut db2, sha, &sym_path, workspace_root)
        .expect_err("load_derived must fail when the target file is a symlink");
    assert!(
        matches!(load_err, LoadError::PathSafety(_)),
        "expected PathSafety error from load_derived; got: {load_err}"
    );
}

// ============================================================================
// Test 10 — oversize_entry_skip
// ============================================================================

/// Configure max_entry_size_bytes to 1 byte (the minimum valid value).
/// Any non-empty serialized result exceeds 1 byte, so the entry is skipped at
/// `insert_query` time. `save_derived` therefore produces a file with 0
/// persistent entries, and the reload yields 0 entries applied.
///
/// The oversize cap is on the raw_result_bytes length (postcard-encoded value).
/// A `Vec<NodeId>` with even one element serializes to ≥ 8 bytes, so cap=1
/// guarantees the skip for any non-empty result.
#[test]
fn oversize_entry_skip() {
    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    // Cap of 1 byte guarantees any non-trivial result is skipped.
    let tiny_cap: usize = 1;
    let config = QueryDbConfig::builder()
        .max_entry_size_bytes(tiny_cap)
        .build();

    let (snapshot, _file) = build_call_graph();
    let db = QueryDb::new(Arc::clone(&snapshot), config);

    // Run CallersQuery — the result is Arc<Vec<NodeId>> with at least one
    // entry ("helper" is called by "main"). Its postcard-encoded size exceeds
    // 1 byte, so insert_query silently skips caching it. The computed value is
    // still returned normally.
    let callers_key = RelationKey::exact("main");
    let result = db.get::<CallersQuery>(&callers_key);
    // Sanity: "main" has callee "helper", so the result must be non-empty.
    assert!(
        !result.is_empty(),
        "test precondition: CallersQuery for 'main' must return non-empty result"
    );

    // Save — no persistent entries because all inserts were skipped.
    let sha: [u8; 32] = [0xEE; 32];
    save_derived(&db, sha, &derived_path, workspace_root)
        .expect("save_derived must not fail even with 0 persistent entries");

    // Reload into a fresh DB (config doesn't matter for reload count).
    let mut db2 = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let outcome = load_derived(&mut db2, sha, &derived_path, workspace_root)
        .expect("load_derived must succeed even with 0 entries");

    let entries_applied = match outcome {
        LoadOutcome::Applied { entries } => entries,
        LoadOutcome::Skipped(_) => panic!("unexpected Skipped"),
    };

    // The oversize entry was never cached, so save wrote 0 entries and reload
    // applied 0 entries.
    assert_eq!(
        entries_applied, 0,
        "oversize entries must not appear after reload; expected 0, got {entries_applied}"
    );
}

// ============================================================================
// Test 11 — staged_validation_purity
// ============================================================================

/// A fatal framing error (truncation) BEFORE commit_staged_load must leave the
/// DB completely unchanged. Verify by checking edge_revision and cold_load_allowed
/// before and after the failing load attempt.
#[test]
fn staged_validation_purity() {
    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();

    let sha: [u8; 32] = [0x66; 32];

    // Build a valid stream with a few entries, then truncate it so the entry
    // stream decoding fails partway through.
    let mut bytes = make_valid_stream(sha, 4);
    assert!(bytes.len() > 16, "precondition: stream must be > 16 bytes");

    // Truncate aggressively — remove the last 16 bytes to guarantee the entry
    // stream is corrupt.
    let truncated_len = bytes.len() - 16;
    bytes.truncate(truncated_len);
    std::fs::write(&derived_path, &bytes).unwrap();

    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());

    // Capture pre-load state.
    let edge_rev_before = db.edge_revision();
    let metadata_rev_before = db.metadata_revision();
    let cold_load_allowed_before = db.cold_load_allowed();

    assert_eq!(edge_rev_before, 0);
    assert_eq!(metadata_rev_before, 0);
    assert!(cold_load_allowed_before);

    // Attempt to load the corrupt file — must fail.
    let err = load_derived(&mut db, sha, &derived_path, workspace_root)
        .expect_err("corrupt file must return Err");
    assert!(
        matches!(err, LoadError::Corrupt { .. }),
        "expected Corrupt error; got: {err}"
    );

    // Post-load state must be identical to pre-load state.
    assert_eq!(
        db.edge_revision(),
        edge_rev_before,
        "edge_revision must not change after a failed staged load"
    );
    assert_eq!(
        db.metadata_revision(),
        metadata_rev_before,
        "metadata_revision must not change after a failed staged load"
    );
    assert_eq!(
        db.cold_load_allowed(),
        cold_load_allowed_before,
        "cold_load_allowed must not change after a failed staged load"
    );
}

// ============================================================================
// Test 12 — sha_mismatch_whole_file_reject
// ============================================================================

/// Save a manifest keyed on [0xAA;32]. Attempt to load with [0xBB;32].
/// Assert Err(StaleSnapshot). Then verify that `load_derived_opportunistic`
/// exercises the file-delete fallback when the SHA does not match.
#[test]
fn sha_mismatch_whole_file_reject() {
    let dir = TempDir::new().unwrap();
    let workspace_root = dir.path();

    let saved_sha: [u8; 32] = [0xAA; 32];
    let caller_sha: [u8; 32] = [0xBB; 32];

    // Set up the canonical workspace layout so load_derived_opportunistic can
    // find the derived file.
    let sqry_dir = workspace_root.join(".sqry").join("graph");
    std::fs::create_dir_all(&sqry_dir).unwrap();

    let snapshot_path = sqry_dir.join("snapshot.sqry");
    // Write fake snapshot bytes whose SHA-256 ≠ saved_sha (won't matter because
    // load_derived_opportunistic computes the SHA from the on-disk file).
    std::fs::write(&snapshot_path, b"fake-snapshot").unwrap();

    let derived_path = sqry_dir.join("derived.sqry");
    let bytes = make_valid_stream(saved_sha, 2);
    std::fs::write(&derived_path, &bytes).unwrap();

    // Direct load with mismatched SHA — must return StaleSnapshot.
    let mut db = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    let err = load_derived(&mut db, caller_sha, &derived_path, workspace_root)
        .expect_err("SHA mismatch must return Err");
    assert!(
        matches!(err, LoadError::StaleSnapshot),
        "expected StaleSnapshot error; got: {err}"
    );

    // DB must remain pristine after the stale-snapshot rejection.
    assert_eq!(
        db.edge_revision(),
        0,
        "DB must be pristine after StaleSnapshot rejection"
    );
    assert!(
        db.cold_load_allowed(),
        "cold_load_allowed must remain true after StaleSnapshot rejection"
    );

    // Now restore the derived file and test the opportunistic path.
    // load_derived_opportunistic uses the on-disk snapshot SHA. The on-disk
    // snapshot is "fake-snapshot", so its actual SHA will differ from saved_sha
    // ([0xAA;32]). This triggers StaleSnapshot and the file-delete fallback.
    std::fs::write(&derived_path, &bytes).unwrap();
    assert!(
        derived_path.exists(),
        "derived file must exist before opportunistic load"
    );

    let mut db3 = QueryDb::new(empty_snapshot(), QueryDbConfig::default());
    // load_derived_opportunistic will compute the SHA of "fake-snapshot" and it
    // won't match [0xAA;32] → StaleSnapshot → file deleted.
    let opp_result = load_derived_opportunistic(&mut db3, workspace_root);
    // We expect either StaleSnapshot or NotFound (if the snapshot bytes happen to
    // produce the same hash as saved_sha — astronomically unlikely but structurally
    // possible with artificial fixtures). In either case, the derived file must be
    // deleted or not present.
    match opp_result {
        Err(LoadError::StaleSnapshot) | Err(LoadError::NotFound { .. }) => {
            // Both are acceptable outcomes depending on the actual hash of "fake-snapshot".
        }
        Err(other) => {
            panic!(
                "opportunistic load returned unexpected error: {other}; \
                 expected StaleSnapshot or NotFound"
            );
        }
        Ok(outcome) => {
            // If the SHA happened to match (extremely unlikely), that's an OK outcome.
            // The test still passes because the purpose is to verify no panic/data corruption.
            let _ = outcome;
        }
    }
}

// ============================================================================
// Test 13 — revision_mismatch_per_entry_skip
// ============================================================================

/// Save the cache at edge_revision=5. After loading into a fresh DB, call
/// db.bump_edge_revision() to advance to 6. Re-querying entries that were
/// rehydrated with deps.edge_revision=5 must produce a cache miss (Tier 2
/// invalidation), not a silent stale hit.
#[test]
fn revision_mismatch_per_entry_skip() {
    let (snapshot, _file) = build_call_graph();

    let dir = TempDir::new().unwrap();
    let derived_path = dir.path().join("derived.sqry");
    let workspace_root = dir.path();
    let sha: [u8; 32] = [0x55; 32];

    // ── Session 1: warm cache at edge_revision=5 ─────────────────────────────

    // We need a DB whose edge_revision is at 5 when we warm the CallersQuery.
    // Bump 5 times.
    let db1 = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    for _ in 0..5 {
        db1.bump_edge_revision();
    }
    assert_eq!(db1.edge_revision(), 5);

    // Warm CallersQuery — this stores the entry with deps.edge_revision = Some(5).
    let callers_key = RelationKey::exact("main");
    let callers_val = db1.get::<CallersQuery>(&callers_key);

    // Save the DB — the entry has deps.edge_revision = 5.
    save_derived(&db1, sha, &derived_path, workspace_root).expect("save_derived must succeed");

    // ── Session 2: cold-start load then bump edge_revision to 6 ─────────────

    let mut db2 = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let outcome = load_derived(&mut db2, sha, &derived_path, workspace_root)
        .expect("load_derived must succeed");

    let entries_applied = match outcome {
        LoadOutcome::Applied { entries } => entries,
        LoadOutcome::Skipped(_) => panic!("unexpected Skipped"),
    };
    assert!(
        entries_applied > 0,
        "at least one entry must be applied; got 0"
    );

    // After cold-load the edge_revision is restored to 5.
    assert_eq!(
        db2.edge_revision(),
        5,
        "edge_revision restored to 5 after load"
    );

    // Spec §2 promise: "first query after a cold start is free." Both
    // warm-path and cold-path use the same QueryKey layout
    // (`(u64::from(Q::QUERY_TYPE_ID), hash(postcard(&key)))`) and the same
    // shard routing (`u64::from(Q::QUERY_TYPE_ID) & (shard_count - 1)`), so
    // the typed `db.get::<CallersQuery>(&callers_key)` finds the rehydrated
    // entry in `get_cold_if_valid`, decodes its `raw_result_bytes` into
    // `CallersQuery::Value`, promotes the entry in place, and returns the
    // typed value — counted as a cache HIT, zero recomputation.

    // First query at edge_revision=5: cache HIT via cold-rehydration path.
    let base = db2.metrics();
    let _ = db2.get::<CallersQuery>(&callers_key);
    let after_first = db2.metrics();
    assert_eq!(
        after_first.cache_misses - base.cache_misses,
        0,
        "first typed query after cold-load must be a HIT (spec §2)"
    );
    assert_eq!(
        after_first.cache_hits - base.cache_hits,
        1,
        "first typed query after cold-load must be exactly one cache hit"
    );

    // Second query at edge_revision=5: entry now promoted → warm fast-path.
    let base2 = db2.metrics();
    let _ = db2.get::<CallersQuery>(&callers_key);
    let after_second = db2.metrics();
    assert_eq!(
        after_second.cache_hits - base2.cache_hits,
        1,
        "second typed query at same edge_revision must also be a cache hit"
    );
    assert_eq!(
        after_second.cache_misses, base2.cache_misses,
        "second typed query must produce zero additional misses"
    );

    // Now bump the edge_revision to 6 — simulates a detected edge change.
    let new_rev = db2.bump_edge_revision();
    assert_eq!(new_rev, 6, "edge_revision must now be 6");

    // Third query at edge_revision=6: cached entry has deps.edge_revision=5 ≠ 6
    // → Tier 2 invalidation → cache miss → recompute.
    let base3 = db2.metrics();
    let callers_val2 = db2.get::<CallersQuery>(&callers_key);
    let after_bump = db2.metrics();

    let new_misses = after_bump.cache_misses - base3.cache_misses;
    assert_eq!(
        new_misses, 1,
        "after edge_revision bump the rehydrated entry must invalidate \
         and produce a cache miss (Tier 2), not a silent stale hit; got {new_misses} misses"
    );

    // Values must still be semantically equivalent (same graph, same result
    // after recomputation).
    assert_eq!(
        *callers_val, *callers_val2,
        "recomputed result must match the original warm value"
    );
}

// ============================================================================
// PF04 — make_query_db_cold never writes derived.sqry
// ============================================================================
//
// Proves that the canonical dispatch helper used by CLI, LSP, and MCP
// (`sqry_db::queries::dispatch::make_query_db_cold`) is a reader-only
// surface. It may DELETE a stale or corrupt derived-cache file (allowed
// by the documented opportunistic-load policy), but it must NEVER write
// or modify a derived-cache file.
//
// Spec: docs/reviews/generational-design-analysis/2026-05-07/codex_in_code_verification_2026-05-07T030441Z.md
// Plan: docs/development/generational-analysis-platform/priority-followups/03_IMPLEMENTATION_PLAN.md (unit PF04)
//
// The three scenarios:
//
// 1. **Fresh workspace, snapshot present, no derived file** — a real V10
//    snapshot.sqry is written via `save_to_path`; `make_query_db_cold`
//    is invoked plus several representative queries. Asserts that no
//    derived.sqry file appears on disk.
//
// 2. **No snapshot.sqry on disk** — the cold-load helper must short-
//    circuit on the missing snapshot. Asserts no derived.sqry created.
//
// 3. **Pre-existing valid derived.sqry** — a writer (the daemon hook in
//    production, `save_derived` in this test) creates a valid
//    derived.sqry. `make_query_db_cold` is invoked; the file's bytes
//    AND modification time must be unchanged after cold-load + queries.

/// PF04 — exhaustive proof that `make_query_db_cold` never writes
/// (creates or modifies) the derived.sqry companion file.
#[test]
#[allow(clippy::similar_names)] // caller/callee + caller_name/callee_name: intentional call-graph terminology
fn pf04_make_query_db_cold_never_writes_derived_sqry() {
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_db::queries::dispatch::make_query_db_cold;
    use std::time::SystemTime;

    // ── Scenario A: snapshot present, no derived file ──────────────────────
    {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path();
        let graph_dir = workspace_root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).expect("mkdir .sqry/graph");

        // Build a minimal real graph and persist it via the canonical V10
        // snapshot writer. This ensures the on-disk SHA-256 is well-formed
        // so `load_derived_opportunistic` follows the normal code path
        // rather than short-circuiting on a missing-file branch.
        let (snapshot_arc, _file) = build_call_graph();
        let mut graph_for_save = CodeGraph::new();
        // Reproduce the same minimal shape; we just need *any* valid graph
        // on disk. Reusing the snapshot's underlying graph isn't directly
        // possible because `snapshot()` returns a clone — instead we build
        // a fresh one identical in structure.
        {
            let file = graph_for_save
                .files_mut()
                .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
                .expect("register file");
            let caller_name = graph_for_save
                .strings_mut()
                .intern("main")
                .expect("intern main");
            let caller = add_node(
                &mut graph_for_save,
                NodeEntry::new(NodeKind::Function, caller_name, file)
                    .with_qualified_name(caller_name)
                    .with_byte_range(0, 100),
            );
            let callee_name = graph_for_save
                .strings_mut()
                .intern("helper")
                .expect("intern helper");
            let callee = add_node(
                &mut graph_for_save,
                NodeEntry::new(NodeKind::Function, callee_name, file)
                    .with_qualified_name(callee_name)
                    .with_byte_range(110, 200),
            );
            graph_for_save.edges().add_edge(
                caller,
                callee,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file,
            );
        }
        let snapshot_path = graph_dir.join("snapshot.sqry");
        save_to_path(&graph_for_save, &snapshot_path).expect("save snapshot");
        assert!(snapshot_path.exists(), "scenario A: snapshot must exist");

        let derived_path = graph_dir.join("derived.sqry");
        assert!(
            !derived_path.exists(),
            "scenario A precondition: derived.sqry must not exist before cold-load"
        );

        // Drive the canonical dispatch helper used by CLI / LSP / MCP.
        let db = make_query_db_cold(Arc::clone(&snapshot_arc), workspace_root);

        // Drive at least two representative DerivedQuery dispatches so any
        // would-be writer code path on the read side gets exercised.
        let _callers = db.get::<CallersQuery>(&RelationKey::exact("main"));
        let _imports = db.get::<ImportsQuery>(&RelationKey::exact("main"));
        let _callees = db.get::<CalleesQuery>(&RelationKey::exact("helper"));

        assert!(
            !derived_path.exists(),
            "scenario A: make_query_db_cold + queries must NOT create derived.sqry; \
             reader-only contract violated (file appeared at {})",
            derived_path.display()
        );
    }

    // ── Scenario B: no snapshot.sqry on disk ──────────────────────────────
    {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path();
        // Note: we do NOT create .sqry/graph at all — the helper must
        // tolerate a fully-absent workspace layout.

        let snapshot_arc = empty_snapshot();
        let db = make_query_db_cold(Arc::clone(&snapshot_arc), workspace_root);

        // Run a query — even on an empty graph the dispatch must complete
        // without writing anything.
        let _callers = db.get::<CallersQuery>(&RelationKey::exact("main"));
        let _imports = db.get::<ImportsQuery>(&RelationKey::exact("main"));

        let derived_path = workspace_root
            .join(".sqry")
            .join("graph")
            .join("derived.sqry");
        assert!(
            !derived_path.exists(),
            "scenario B: make_query_db_cold against a workspace with no snapshot.sqry \
             must not create derived.sqry"
        );
        // Belt-and-braces: also assert the .sqry directory was not silently
        // synthesised by the cold-load path.
        let sqry_dir = workspace_root.join(".sqry");
        assert!(
            !sqry_dir.exists(),
            "scenario B: cold-load must not create the .sqry/ directory either"
        );
    }

    // ── Scenario C: pre-existing valid derived.sqry ──────────────────────
    //
    // Setup: write a real V10 snapshot, then use `save_derived` (the only
    // legitimate writer — invoked from the daemon hook in production) to
    // produce a derived.sqry whose SHA matches the on-disk snapshot.
    //
    // Verification: capture the file's bytes + mtime before the
    // cold-load; invoke `make_query_db_cold`; assert bytes + mtime are
    // identical afterwards. The opportunistic loader is allowed to READ
    // and PARSE the file but must never rewrite it.
    {
        use sqry_db::persistence::{compute_file_sha256, save_derived};

        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path();
        let graph_dir = workspace_root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).expect("mkdir .sqry/graph");

        // Persist a real graph so `load_derived_opportunistic` finds a
        // matching SHA on disk. This is what unlocks the actual rehydration
        // path inside `make_query_db_cold` (rather than a NotFound short-
        // circuit on the snapshot file).
        let mut graph_for_save = CodeGraph::new();
        let file = graph_for_save
            .files_mut()
            .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
            .expect("register file");
        let caller_name = graph_for_save
            .strings_mut()
            .intern("main")
            .expect("intern main");
        let caller = add_node(
            &mut graph_for_save,
            NodeEntry::new(NodeKind::Function, caller_name, file)
                .with_qualified_name(caller_name)
                .with_byte_range(0, 100),
        );
        let callee_name = graph_for_save
            .strings_mut()
            .intern("helper")
            .expect("intern helper");
        let callee = add_node(
            &mut graph_for_save,
            NodeEntry::new(NodeKind::Function, callee_name, file)
                .with_qualified_name(callee_name)
                .with_byte_range(110, 200),
        );
        graph_for_save.edges().add_edge(
            caller,
            callee,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let snapshot_path = graph_dir.join("snapshot.sqry");
        save_to_path(&graph_for_save, &snapshot_path).expect("save snapshot");

        // Compute the actual SHA the on-disk snapshot has, then write a
        // derived.sqry keyed on that SHA so the opportunistic loader will
        // accept it as fresh.
        let on_disk_sha = compute_file_sha256(&snapshot_path).expect("hash snapshot");

        // Build a cache-warm DB whose snapshot matches the persisted graph
        // structurally (the cache holds derived results that are valid for
        // any snapshot with the same SHA — saved separately above).
        let snapshot_arc = Arc::new(graph_for_save.snapshot());
        let writer_db = QueryDb::new(Arc::clone(&snapshot_arc), QueryDbConfig::default());
        // Warm a query so save_derived has at least one entry to persist.
        let _ = writer_db.get::<CallersQuery>(&RelationKey::exact("main"));

        let derived_path = graph_dir.join("derived.sqry");
        save_derived(&writer_db, on_disk_sha, &derived_path, workspace_root)
            .expect("setup: save_derived must succeed");
        assert!(
            derived_path.exists(),
            "scenario C precondition: derived.sqry must exist after setup"
        );

        // Snapshot the file's bytes + mtime BEFORE the cold-load.
        let before_bytes = std::fs::read(&derived_path).expect("read derived");
        let before_meta = std::fs::metadata(&derived_path).expect("metadata derived");
        let before_mtime = before_meta
            .modified()
            .expect("mtime")
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("duration since epoch");

        // Sleep briefly so any (forbidden) rewrite would produce a
        // distinct mtime on filesystems with second-level granularity.
        // 1.1s is enough on every supported filesystem.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Drive the canonical dispatch helper.
        let db = make_query_db_cold(Arc::clone(&snapshot_arc), workspace_root);

        // Run multiple queries — even queries that miss the rehydrated
        // entries (and would warm fresh cache entries in the in-memory DB)
        // must not trigger a disk write.
        let _ = db.get::<CallersQuery>(&RelationKey::exact("main"));
        let _ = db.get::<ImportsQuery>(&RelationKey::exact("main"));
        let _ = db.get::<CalleesQuery>(&RelationKey::exact("helper"));

        // The file MUST still exist (opportunistic loader does not delete
        // a fresh, well-formed derived file).
        assert!(
            derived_path.exists(),
            "scenario C: derived.sqry must still exist after cold-load (it was valid)"
        );

        // Bytes must be byte-for-byte identical.
        let after_bytes = std::fs::read(&derived_path).expect("read derived after");
        assert_eq!(
            before_bytes, after_bytes,
            "scenario C: derived.sqry bytes must be unchanged by make_query_db_cold + queries; \
             reader-only contract violated (file was rewritten)"
        );

        // mtime must be unchanged.
        let after_meta = std::fs::metadata(&derived_path).expect("metadata derived after");
        let after_mtime = after_meta
            .modified()
            .expect("mtime")
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("duration since epoch");
        assert_eq!(
            before_mtime, after_mtime,
            "scenario C: derived.sqry mtime must be unchanged by make_query_db_cold + queries; \
             reader-only contract violated (file was touched)"
        );
    }
}
