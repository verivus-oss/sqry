//! Phase 3C / PN3 Proof Point 3 — Hot-cache persistence + cold-start restore.
//!
//! From the spec (`docs/superpowers/specs/2026-04-12-derived-analysis-db-query-
//! planner-design.md`, "Proof 3: Hot-cache persistence"):
//!
//! > Build, query `dependency_impact` for 5 symbols (populates cache).
//! > Save snapshot + companion `derived.sqry`. Reload. Query the same 5
//! > symbols. Assert zero recomputation (cache warm from persisted
//! > derived facts). Assert file-level revision validation passes.
//! > Assert graph identity (snapshot SHA-256) matches.
//!
//! # Scope
//!
//! DB21 delivered the manifest-level primitives (snapshot SHA-256 identity,
//! manifest save/load, freshness checks).  DB22 wired full cache-entry
//! serialisation via `save_derived` / `load_derived`.  PN3 (cold-start
//! persistence) extended the cross-session proof to include:
//!
//! - Full `save_derived` → `load_derived` round-trip across two separate
//!   `QueryDb` sessions.
//! - `LoadOutcome::Applied { entries }` assertion on the freshly-loaded cache.
//! - `cold_load_allowed() == false` signal after a successful load.
//! - Two-pass warm-path verification (priming + zero-recomputation) using the
//!   revision tiers restored by `commit_staged_load`.
//! - Selective Tier 1 invalidation: only the entries whose nodes live on the
//!   mutated file miss; entries on unrelated files stay warm.
//!
//! This file therefore asserts:
//!
//! - Within a single `QueryDb` session, repeated queries hit the cache and
//!   never recompute (the "zero recomputation" property on the warm path).
//! - The manifest round-trips through `save_manifest` / `load_manifest` and
//!   `matches_snapshot` correctly authorises reloads when the snapshot hash
//!   matches and rejects reloads when it does not.
//! - `compute_file_sha256` produces a stable identity for the same bytes and a
//!   different identity for differing bytes.
//! - A full cold-start restore (`save_derived` + `load_derived`) correctly
//!   rebuilds the three revision tiers and enables two-pass warm-path
//!   verification and selective file-level Tier 1 invalidation.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::file::id::FileId;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::persistence::{
    DerivedManifest, compute_file_sha256, derived_path_for_snapshot, load_manifest, save_manifest,
};
use sqry_db::queries::{IsInCycleQuery, RelationKey, mcp_callers_query};
use sqry_db::{LoadOutcome, QueryDb, QueryDbConfig};

use tempfile::TempDir;

/// Adds a node into the arena and registers it with the name index.
fn add_node(graph: &mut CodeGraph, entry: NodeEntry) -> NodeId {
    let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
    graph
        .indices_mut()
        .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
    id
}

/// Builds a 3-file fixture for cold-start persistence tests using
/// `IsInCycleQuery` entries.
///
/// Distribution (chosen so file_a has exactly 1 node — the "mutation target"):
/// - `file_a` (`src/file_a.rs`): node_a only  → 1 `IsInCycleQuery` entry
/// - `file_b` (`src/file_b.rs`): node_b, node_c, node_d → 3 entries
/// - `file_c` (`src/file_c.rs`): node_e  → 1 entry
///
/// Every node carries a **self-loop** `Calls` edge so that
/// `IsInCycleQuery { should_include_self_loops: true }` returns `true`
/// without reaching `db.get::<SccQuery>()`.  This has two benefits:
///
/// 1. SccQuery is never warmed during fixture use → `save_derived` stores
///    exactly 5 entries (one per node), satisfying
///    `LoadOutcome::Applied { entries: 5 }`.
/// 2. `IsInCycleQuery::execute` records `record_file_dep(entry.file)` for
///    each node and returns immediately, so the Tier 1 dep is exactly the
///    owning file (no SccQuery nested call to clear the thread-local).
///
/// `IsInCycleQuery` records deps via `snapshot.nodes().get(key.node_id).file`
/// — the node's stored `FileId` — which is set correctly in hand-built
/// fixtures (unlike `snapshot.file_segments()`, which is only populated by
/// the full build pipeline and is EMPTY in hand-built graphs).
///
/// Returns: `(Arc<GraphSnapshot>, Vec<IsInCycleKey>, FileId, FileId, FileId)`
/// where `IsInCycleKey` carries the NodeId + self-loop bounds.
fn build_3_file_fixture() -> (
    Arc<GraphSnapshot>,
    Vec<sqry_db::queries::IsInCycleKey>,
    FileId,
    FileId,
    FileId,
) {
    use sqry_core::query::CircularType;
    use sqry_db::queries::{CycleBounds, IsInCycleKey};

    let mut graph = CodeGraph::new();

    // Self-loop Calls edge: node → node (same source and target).
    let add_self_loop = |graph: &mut CodeGraph, node: NodeId, file: FileId| {
        graph.edges().add_edge(
            node,
            node,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
    };

    // ── file_a: node_a (1 node — the mutation target) ───────────────────────
    let file_a = graph
        .files_mut()
        .register_with_language(Path::new("src/file_a.rs"), Some(Language::Rust))
        .expect("register file_a");
    let sym_a = graph.strings_mut().intern("sym_a").expect("intern sym_a");
    let node_a = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, sym_a, file_a)
            .with_qualified_name(sym_a)
            .with_byte_range(0, 80),
    );
    add_self_loop(&mut graph, node_a, file_a);

    // ── file_b: node_b, node_c, node_d (3 nodes — stay warm on mutation) ────
    let file_b = graph
        .files_mut()
        .register_with_language(Path::new("src/file_b.rs"), Some(Language::Rust))
        .expect("register file_b");
    let sym_b = graph.strings_mut().intern("sym_b").expect("intern sym_b");
    let node_b = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, sym_b, file_b)
            .with_qualified_name(sym_b)
            .with_byte_range(0, 80),
    );
    add_self_loop(&mut graph, node_b, file_b);

    let sym_c = graph.strings_mut().intern("sym_c").expect("intern sym_c");
    let node_c = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, sym_c, file_b)
            .with_qualified_name(sym_c)
            .with_byte_range(100, 180),
    );
    add_self_loop(&mut graph, node_c, file_b);

    let sym_d = graph.strings_mut().intern("sym_d").expect("intern sym_d");
    let node_d = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, sym_d, file_b)
            .with_qualified_name(sym_d)
            .with_byte_range(200, 280),
    );
    add_self_loop(&mut graph, node_d, file_b);

    // ── file_c: node_e (1 node — stays warm on mutation) ────────────────────
    let file_c = graph
        .files_mut()
        .register_with_language(Path::new("src/file_c.rs"), Some(Language::Rust))
        .expect("register file_c");
    let sym_e = graph.strings_mut().intern("sym_e").expect("intern sym_e");
    let node_e = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, sym_e, file_c)
            .with_qualified_name(sym_e)
            .with_byte_range(0, 80),
    );
    add_self_loop(&mut graph, node_e, file_c);

    // Build keys: `should_include_self_loops = true` so execute() short-circuits
    // at the self-loop check and never calls SccQuery.
    let bounds = CycleBounds {
        min_depth: 2,
        max_depth: None,
        max_results: 100,
        should_include_self_loops: true,
    };
    let keys = vec![
        IsInCycleKey {
            node_id: node_a,
            circular_type: CircularType::Calls,
            bounds,
        },
        IsInCycleKey {
            node_id: node_b,
            circular_type: CircularType::Calls,
            bounds,
        },
        IsInCycleKey {
            node_id: node_c,
            circular_type: CircularType::Calls,
            bounds,
        },
        IsInCycleKey {
            node_id: node_d,
            circular_type: CircularType::Calls,
            bounds,
        },
        IsInCycleKey {
            node_id: node_e,
            circular_type: CircularType::Calls,
            bounds,
        },
    ];

    (Arc::new(graph.snapshot()), keys, file_a, file_b, file_c)
}

/// Builds a 5-symbol fixture. Each symbol is a Function with at least one
/// caller so `dependency_impact` (modeled here via `callers_of`) returns a
/// non-empty result.
fn build_fixture() -> (Arc<GraphSnapshot>, Vec<String>) {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
        .expect("register file");

    let caller_name = graph.strings_mut().intern("driver").expect("intern driver");
    let caller = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, caller_name, file)
            .with_qualified_name(caller_name)
            .with_byte_range(0, 60),
    );

    let symbols = ["sym_a", "sym_b", "sym_c", "sym_d", "sym_e"];
    for (i, s) in symbols.iter().enumerate() {
        let name = graph.strings_mut().intern(s).expect("intern symbol");
        let start = 100 + (i as u32) * 200;
        let node = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, name, file)
                .with_qualified_name(name)
                .with_byte_range(start, start + 80),
        );
        graph.edges().add_edge(
            caller,
            node,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
    }

    (
        Arc::new(graph.snapshot()),
        symbols.iter().map(|s| (*s).to_string()).collect(),
    )
}

#[test]
fn proof3_warm_cache_has_zero_recomputation_for_repeated_queries() {
    // The "zero recomputation" property on the warm path. After an
    // initial populate pass, repeating the same 5 queries must be
    // served entirely from cache — no `execute` invocations.
    let (snapshot, symbols) = build_fixture();
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    // Cold populate: 5 queries × 1 miss each.
    for s in &symbols {
        let _ = mcp_callers_query(&db, &RelationKey::exact(s));
    }
    let cold = db.metrics();
    assert_eq!(cold.cache_misses, 5, "5 cold queries => 5 misses");
    assert_eq!(cold.cache_hits, 0);

    // Warm repeat: 5 queries × 1 hit each, 0 misses.
    for s in &symbols {
        let _ = mcp_callers_query(&db, &RelationKey::exact(s));
    }
    let warm = db.metrics();
    assert_eq!(
        warm.cache_misses, cold.cache_misses,
        "warm repeat must NOT produce additional misses (zero recomputation)"
    );
    assert_eq!(
        warm.cache_hits - cold.cache_hits,
        5,
        "warm repeat must produce exactly 5 hits"
    );
}

#[test]
fn proof3_snapshot_sha256_matches_across_identical_payloads() {
    // Graph-identity assertion: the SHA-256 of a fixed byte sequence is
    // stable. This is the integrity anchor the manifest uses to decide
    // whether the on-disk derived facts are safe to consume.
    let tmp = TempDir::new().expect("tempdir");
    let snap1 = tmp.path().join("snap1.sqry");
    let snap2 = tmp.path().join("snap2.sqry");
    std::fs::write(&snap1, b"snapshot-payload-v1").expect("write snap1");
    std::fs::write(&snap2, b"snapshot-payload-v1").expect("write snap2");

    let h1 = compute_file_sha256(&snap1).expect("sha snap1");
    let h2 = compute_file_sha256(&snap2).expect("sha snap2");
    assert_eq!(h1, h2, "identical bytes must hash to the same SHA-256");

    // Differing bytes → differing hash.
    let snap3 = tmp.path().join("snap3.sqry");
    std::fs::write(&snap3, b"snapshot-payload-v2").expect("write snap3");
    let h3 = compute_file_sha256(&snap3).expect("sha snap3");
    assert_ne!(h1, h3, "different bytes must hash differently");
}

#[test]
fn proof3_manifest_round_trip_and_graph_identity_gating() {
    // Manifest life-cycle: write, read, check the SHA-256 gate.
    let tmp = TempDir::new().expect("tempdir");
    let snapshot_path = tmp.path().join(".sqry").join("graph").join("snapshot.sqry");
    std::fs::create_dir_all(snapshot_path.parent().unwrap()).expect("mkdir");
    std::fs::write(&snapshot_path, b"snapshot-bytes").expect("write snapshot");

    let snap_hash = compute_file_sha256(&snapshot_path).expect("hash snapshot");

    // derived_path_for_snapshot: "derived.sqry" alongside "snapshot.sqry".
    let derived_path = derived_path_for_snapshot(&snapshot_path, "derived.sqry");
    assert_eq!(
        derived_path,
        snapshot_path.parent().unwrap().join("derived.sqry")
    );

    // Write the manifest keyed on the true hash.
    // DerivedManifest is now an alias for DerivedHeader v02; supply the
    // three new revision fields as zeros (warm-path proof only cares about
    // snapshot identity, not the revision tier baselines).
    let manifest = DerivedManifest::new(snap_hash, 0, 0, vec![], 5);
    save_manifest(&derived_path, &manifest).expect("save manifest");

    // Reload the manifest and verify identity.
    let loaded = load_manifest(&derived_path).expect("load manifest");
    assert_eq!(loaded.snapshot_sha256, snap_hash);
    assert_eq!(loaded.entry_count, 5);
    assert!(
        loaded.matches_snapshot(&snap_hash),
        "manifest must authorise reload when the snapshot hash matches"
    );

    // Mutate the snapshot — the manifest must now reject.
    std::fs::write(&snapshot_path, b"snapshot-bytes-changed").expect("rewrite snapshot");
    let new_hash = compute_file_sha256(&snapshot_path).expect("rehash snapshot");
    assert_ne!(new_hash, snap_hash);
    assert!(
        !loaded.matches_snapshot(&new_hash),
        "manifest must reject reload when the snapshot hash has changed"
    );
}

#[test]
fn proof3_file_level_revision_validation_is_monotonic() {
    // File-level revision validation gate: a cached result with recorded
    // `(FileId, revision)` pairs validates only while every file's current
    // revision still matches. Once any file's revision bumps, the Tier 1
    // validation fails.
    use smallvec::SmallVec;
    use sqry_core::graph::unified::file::id::FileId;
    use sqry_db::cache::CachedResult;
    use sqry_db::dependency::FileDep;
    use sqry_db::input::{FileInput, FileInputStore};

    let mut store = FileInputStore::new();
    store.insert(FileId::new(1), FileInput::new(Default::default()));
    store.insert(FileId::new(2), FileInput::new(Default::default()));

    let mut deps: SmallVec<[FileDep; 8]> = SmallVec::new();
    deps.push((FileId::new(1), 1));
    deps.push((FileId::new(2), 1));
    let cached = CachedResult::new(vec![1u32, 2, 3], deps, None, None);

    assert!(
        cached.validate_file_deps(&store),
        "baseline revisions (1,1) must validate"
    );

    // Bump file 1 → validation fails.
    store
        .get_mut(FileId::new(1))
        .unwrap()
        .update(Default::default());
    assert!(
        !cached.validate_file_deps(&store),
        "after file 1's revision bump, validation must fail"
    );
}

#[test]
fn proof3_load_manifest_returns_none_for_missing_file() {
    // Negative case: no manifest on disk means reload skips and the
    // cache stays cold. This is the failure mode the persistence layer
    // degrades into cleanly — cache misses on cold start, not a crash.
    let tmp = TempDir::new().expect("tempdir");
    let derived_path = tmp.path().join("nonexistent-derived.sqry");
    assert!(load_manifest(&derived_path).is_none());
}

#[test]
fn proof3_dependency_impact_queries_are_independent_cache_entries() {
    // Each of the 5 symbols occupies its own cache entry — bumping one
    // symbol's inputs (via a revision bump on that symbol's file, which
    // in this fixture is shared across all 5) invalidates ALL because
    // they share the single file. But re-querying them still produces
    // exactly 5 fresh cache entries (no silent deduplication across
    // distinct RelationKeys).
    let (snapshot, symbols) = build_fixture();
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    let before = db.metrics();
    for s in &symbols {
        let _ = mcp_callers_query(&db, &RelationKey::exact(s));
    }
    let after_first = db.metrics();
    assert_eq!(after_first.cache_misses - before.cache_misses, 5);

    // Second pass — all 5 are hits.
    for s in &symbols {
        let _ = mcp_callers_query(&db, &RelationKey::exact(s));
    }
    let after_second = db.metrics();
    assert_eq!(after_second.cache_hits - after_first.cache_hits, 5);
    assert_eq!(after_second.cache_misses, after_first.cache_misses);
}

// ============================================================================
// PN3 PROOF3_EXTEND — cold-start full-restore (spec §7.1)
// ============================================================================

/// Proof 3 extension: cold-start full-restore across two separate `QueryDb`
/// sessions, per spec §7.1.
///
/// Contract under test:
/// 1. Session A: Build 3-file fixture, warm 5 `IsInCycleQuery` entries (one
///    per node — each node carries a self-loop), call `save_derived`.
/// 2. Drop Session A.
/// 3. Session B (fresh `QueryDb`): call `load_derived`.
///    Assert `LoadOutcome::Applied { entries: 5 }`.
///    Assert `cold_load_allowed() == false` (cold-load window closed).
/// 4. Primary recomputation probe — `cold_load_allowed == false`:
///    `QueryDbMetrics` does not expose a `recomputations` counter; the
///    `cold_load_allowed == false` flag is the canonical signal that the
///    cold-load pipeline ran successfully and closed the load window.
///    Secondary: re-issue the 5 queries in Session B once (priming pass)
///    and then re-issue a second time (warm pass).  The second pass must
///    produce ZERO new misses, proving the revision tiers restored by
///    `commit_staged_load` are live guards that keep the freshly-computed
///    entries cache-valid.
///
///    Note on `insert_validated` semantics: cold-loaded entries are stored
///    under a raw-byte-hash-keyed `QueryKey`, while typed `db.get::<Q>()`
///    lookups compute the key via `TypeId + key_hash`.  These keys differ,
///    so the first typed re-issue after cold-load is a cache miss (priming
///    pass).  The value of cold-load is that the three revision tiers are
///    correctly restored before any query runs, so the primed entries are
///    immediately valid.  The second re-issue (warm pass) then hits the
///    freshly-primed entries with ZERO misses.
///
/// 5. Simulate a file-content change for `file_a` (revision bump only, no
///    edge-revision change).  `IsInCycleQuery::execute` records
///    `record_file_dep(entry.file)` using the node's stored `FileId`, so only
///    the 1 entry whose node lives on `file_a` records `file_a` as a Tier 1
///    dep.  The 4 entries on `file_b` / `file_c` record different files.
///    Assert: exactly 1 entry misses (node_a on file_a), 4 entries hit (nodes
///    on file_b and file_c stay warm) — the Tier 1 file-revision check is
///    narrow and precise, proving cold-loaded revisions are live guards.
///
/// # Architecture note on `IsInCycleQuery` dep granularity
///
/// `IsInCycleQuery::execute` records `record_file_dep(entry.file)` for the
/// target node's owning file and then short-circuits with `return true` when
/// the node has a self-loop and `should_include_self_loops` is true.  This
/// means:
///
/// - `SccQuery` is **never called** — only 5 `IsInCycleQuery` entries are
///   warmed and saved (entry count stays at exactly 5).
/// - The Tier 1 dep is the exact owning file of the node, not every file in
///   the snapshot.  Mutation isolation is therefore per-file, not global.
/// - The node's `FileId` is set correctly in hand-built fixtures (unlike
///   `snapshot.file_segments()`, which is only populated by the full build
///   pipeline).
#[test]
fn proof3_cold_start_full_restore() {
    // ── Fixture + workspace setup ───────────────────────────────────────────
    let (snapshot, keys, file_a, _file_b, _file_c) = build_3_file_fixture();

    // Set up a tempdir acting as the workspace root.  We need a real on-disk
    // `.sqry/graph/` hierarchy because `save_derived` / `load_derived` call
    // `validate_path_in_workspace` which canonicalises the path and verifies
    // no symlink escapes.  The snapshot file doesn't need to contain real
    // sqry-serialized graph data — any stable byte sequence anchors the
    // SHA-256 identity.
    let workspace = TempDir::new().expect("create workspace tempdir");
    let graph_dir = workspace.path().join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).expect("create .sqry/graph/");

    let snapshot_path = graph_dir.join("snapshot.sqry");
    std::fs::write(&snapshot_path, b"proof3-cold-start-anchor-bytes").expect("write snapshot stub");
    let snapshot_sha256 = compute_file_sha256(&snapshot_path).expect("sha256 snapshot stub");

    let config = QueryDbConfig::default();
    let derived = sqry_db::queries::derived_path(workspace.path(), &config);

    // ── Session A: warm 5 IsInCycleQuery entries, persist ──────────────────
    {
        let db = QueryDb::new(Arc::clone(&snapshot), config.clone());

        // Cold-populate: 5 IsInCycleQuery entries × 1 miss each.
        // Each self-loop node short-circuits in execute() → returns true
        // without calling SccQuery, so exactly 5 entries are warmed.
        for key in &keys {
            let result = db.get::<IsInCycleQuery>(key);
            assert!(
                result,
                "every node has a self-loop — IsInCycleQuery must return true"
            );
        }
        let after_warm = db.metrics();
        assert_eq!(
            after_warm.cache_misses, 5,
            "session A: 5 IsInCycleQuery queries must produce 5 cold misses"
        );

        // Persist the warm cache.
        sqry_db::persistence::save_derived(&db, snapshot_sha256, &derived, workspace.path())
            .expect("save_derived must succeed");
    }
    // Session A dropped here — all in-process cache state is gone.

    // ── Session B: fresh QueryDb, cold-load ────────────────────────────────
    let mut db_b = QueryDb::new(Arc::clone(&snapshot), config.clone());

    // cold_load_allowed must be true at construction.
    assert!(
        db_b.cold_load_allowed(),
        "cold_load_allowed must be true before the first load"
    );

    let outcome =
        sqry_db::persistence::load_derived(&mut db_b, snapshot_sha256, &derived, workspace.path())
            .expect("load_derived must succeed");

    // ── Assertion 1: Applied { entries: 5 } ────────────────────────────────
    match outcome {
        LoadOutcome::Applied { entries } => {
            assert_eq!(entries, 5, "cold-load must restore exactly 5 cache entries");
        }
        other => panic!("expected LoadOutcome::Applied {{ entries: 5 }}, got {other:?}"),
    }

    // ── Assertion 2: cold_load_allowed == false after a successful load ─────
    assert!(
        !db_b.cold_load_allowed(),
        "cold_load_allowed must be false after a successful load_derived"
    );

    // ── Assertion 3: recomputation probe ───────────────────────────────────
    //
    // Primary signal: `cold_load_allowed() == false` (already asserted above).
    // This is the canonical proof that `load_derived` ran and closed the
    // cold-load window — `QueryDbMetrics` has no `recomputations` counter.
    //
    // Spec §2 promise — "first query after a cold start is free":
    //
    // `commit_staged_load` places rehydrated entries into the same shard +
    // `QueryKey` slot that warm-path `QueryDb::get::<IsInCycleQuery>` probes
    // (`(u64::from(Q::QUERY_TYPE_ID), hash(postcard(&key)))`). The typed
    // `get::<Q>` path tries the warm downcast first; on placeholder entries
    // from cold-load, it falls into `ShardedCache::get_cold_if_valid`, which
    // decodes `raw_result_bytes` into `Q::Value`, validates the restored
    // three-tier revision baseline, promotes the entry in place, and returns
    // the typed value — all counted as a cache HIT, zero recomputation.
    //
    // The single post-load pass therefore MUST produce 5 cache hits and
    // 0 misses.
    assert_eq!(
        db_b.metrics().cache_misses,
        0,
        "no misses should have accumulated before the post-load pass in session B"
    );

    // Post-load pass — spec §2 "first query after cold start is free".
    for key in &keys {
        let _ = db_b.get::<IsInCycleQuery>(key);
    }
    let after_first_pass = db_b.metrics();
    assert_eq!(
        after_first_pass.cache_misses, 0,
        "first typed query pass after cold-load MUST be ZERO misses (spec §2 \
         'first query after a cold start is free')"
    );
    assert_eq!(
        after_first_pass.cache_hits, 5,
        "first typed query pass after cold-load MUST be exactly 5 cache hits \
         (rehydrated entries landed in the same (shard, QueryKey) slot that \
         `get::<Q>` probes)"
    );

    // Second pass stays at 5 additional hits, 0 new misses — entries have
    // been promoted in-place to typed values, so the fast downcast path
    // serves them.
    for key in &keys {
        let _ = db_b.get::<IsInCycleQuery>(key);
    }
    let after_second_pass = db_b.metrics();
    assert_eq!(
        after_second_pass.cache_misses - after_first_pass.cache_misses,
        0,
        "second pass must also produce zero misses"
    );
    assert_eq!(
        after_second_pass.cache_hits - after_first_pass.cache_hits,
        5,
        "second pass must produce exactly 5 more cache hits"
    );

    // ── Assertion 4 (mutation + selective invalidation) ────────────────────
    //
    // Simulate a file-content change for `file_a` by bumping its revision
    // counter without changing the global edge revision.
    //
    // `IsInCycleQuery::execute` records `record_file_dep(entry.file)` for the
    // node's owning file (narrow dep footprint).  Only the 1 cached entry for
    // node_a (which lives on file_a) records file_a as a Tier 1 dep.  The 4
    // entries for nodes on file_b / file_c record their respective files.
    //
    // After bumping file_a's revision:
    //   - node_a's entry: Tier 1 check fails → 1 cache miss (recomputes)
    //   - node_b, node_c, node_d, node_e entries: Tier 1 still passes → 4 hits
    //
    // This proves that the revision guards restored by `commit_staged_load`
    // are live, active, and correctly scoped — not stale no-ops.
    db_b.inputs_mut()
        .get_mut(file_a)
        .expect("file_a must be present in the input store after cold-load")
        .update(Default::default()); // bumps revision: 1 → 2

    let pre_mutation_reissue = db_b.metrics();
    for key in &keys {
        let _ = db_b.get::<IsInCycleQuery>(key);
    }
    let post_mutation_reissue = db_b.metrics();

    // Exactly 1 miss: node_a on file_a.  4 hits: node_b/c/d on file_b, node_e
    // on file_c.  Narrow Tier 1 dep granularity from IsInCycleQuery.
    assert_eq!(
        post_mutation_reissue.cache_misses - pre_mutation_reissue.cache_misses,
        1,
        "after file_a revision bump, exactly 1 IsInCycleQuery entry must invalidate \
         (only node_a records file_a as its Tier 1 dep)"
    );
    assert_eq!(
        post_mutation_reissue.cache_hits - pre_mutation_reissue.cache_hits,
        4,
        "after file_a revision bump, the 4 entries on file_b and file_c must stay warm"
    );

    // After the recomputation pass, a final repeat must be all hits again.
    let pre_final = db_b.metrics();
    for key in &keys {
        let _ = db_b.get::<IsInCycleQuery>(key);
    }
    let post_final = db_b.metrics();
    assert_eq!(
        post_final.cache_hits - pre_final.cache_hits,
        5,
        "after recomputation, a second repeat must be 5 cache hits"
    );
    assert_eq!(
        post_final.cache_misses - pre_final.cache_misses,
        0,
        "after recomputation, a second repeat must have 0 new misses"
    );
}
