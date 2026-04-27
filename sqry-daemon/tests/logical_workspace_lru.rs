//! STEP_6 (workspace-aware-cross-repo, 2026-04-26) — LRU eviction +
//! grouping invariants for logically-grouped workspaces.
//!
//! Every assertion here exercises the per-source-root LRU + the
//! `daemon/workspaceStatus`-style aggregate rollup. Eviction is
//! per-source-root, so a logical workspace can be partially evicted
//! while sibling source roots remain `Loaded` (acceptance criteria #4
//! and #5 in the STEP_6 brief).
//!
//! These tests drive `WorkspaceManager` directly — the IPC layer is
//! exercised in the sibling `daemon_workspace_status.rs` integration
//! suite.

use std::{path::PathBuf, sync::Arc};

use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, WorkspaceKey, WorkspaceManager, WorkspaceState, workspace::LoadedWorkspace,
};
use sqry_daemon_protocol::WorkspaceId;

fn make_manager() -> Arc<WorkspaceManager> {
    let config = Arc::new(DaemonConfig::default());
    WorkspaceManager::new_without_reaper(config)
}

fn insert_loaded(
    mgr: &WorkspaceManager,
    key: &WorkspaceKey,
    pinned: bool,
    bytes: usize,
) -> Arc<LoadedWorkspace> {
    mgr.insert_workspace_for_test_with_bytes(key.clone(), WorkspaceState::Loaded, pinned, bytes)
}

#[test]
fn lru_evicts_per_source_root_not_per_workspace_id() {
    // Two source roots share one `workspace_id`. Evicting the
    // least-recently-used one must NOT evict the other — eviction is
    // per-source-root, not per-grouping.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x42; 32]);
    let key_a =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/a"), ProjectRootMode::GitRoot, 0);
    let key_b =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/b"), ProjectRootMode::GitRoot, 0);

    let _ws_a = insert_loaded(&mgr, &key_a, false, 1_000);
    let ws_b = insert_loaded(&mgr, &key_b, false, 1_000);

    // Make `key_a` strictly older than `key_b` so it is the LRU
    // victim. `last_accessed` is initialised to `Instant::now()` on
    // construction; we touch `key_b` after a small delay so the
    // ordering is well-defined.
    std::thread::sleep(std::time::Duration::from_millis(2));
    ws_b.touch();

    let evicted = mgr.evict_lru().expect("at least one candidate");
    assert_eq!(
        evicted, key_a,
        "LRU candidate must be the older source root"
    );

    // key_b remains unevicted — the grouping `workspace_id` does not
    // pull it down with key_a. STEP_6 iter-2 contract: LRU eviction
    // keeps the Evicted tombstone in the map, so the aggregate
    // surfaces BOTH source roots (a Evicted, b Loaded).
    assert_eq!(ws_b.load_state(), WorkspaceState::Loaded);
    let aggregate = mgr
        .workspace_index_status(&id)
        .expect("aggregate still has key_b");
    assert_eq!(
        aggregate.source_roots.len(),
        2,
        "LRU eviction keeps the Evicted tombstone in the aggregate",
    );
    let by_root: std::collections::HashMap<_, _> = aggregate
        .source_roots
        .iter()
        .map(|r| (r.source_root.clone(), r.state))
        .collect();
    assert_eq!(
        by_root.get(&PathBuf::from("/repos/a")),
        Some(&WorkspaceState::Evicted),
        "evicted source root must surface as Evicted, not be removed",
    );
    assert_eq!(
        by_root.get(&PathBuf::from("/repos/b")),
        Some(&WorkspaceState::Loaded),
    );
}

#[test]
fn partial_eviction_aggregate_reports_evicted_for_evicted_source_roots() {
    // STEP_6 iter-2 BLOCK fix: drive REAL per-source-root LRU
    // eviction (NOT a state-only test helper). The aggregate must
    // surface the Evicted source root because production
    // `execute_eviction` keeps the tombstone in the manager map.
    //
    // Test shape:
    //   1. Two source roots share one `workspace_id`.
    //   2. Make `key_a` strictly older than `key_b`.
    //   3. Call `mgr.evict_lru()` — this routes through the real
    //      `execute_eviction`, which now keeps the Evicted tombstone.
    //   4. Aggregate must report BOTH source roots — `key_a` with
    //      state Evicted, `key_b` with state Loaded.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x55; 32]);
    let key_a =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/a"), ProjectRootMode::GitRoot, 0);
    let key_b =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/b"), ProjectRootMode::GitRoot, 0);

    let _ws_a = insert_loaded(&mgr, &key_a, false, 1_000);
    let ws_b = insert_loaded(&mgr, &key_b, false, 1_000);

    // Make `key_a` strictly older than `key_b` so the production
    // LRU picks it as the victim.
    std::thread::sleep(std::time::Duration::from_millis(2));
    ws_b.touch();

    // Drive REAL eviction through the production code path. The
    // STEP_6 iter-2 fix to `execute_eviction` keeps the tombstone
    // in the map with state == Evicted instead of removing it.
    let evicted = mgr.evict_lru().expect("LRU candidate present");
    assert_eq!(evicted, key_a, "LRU victim must be the older source root");

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    assert_eq!(aggregate.workspace_id, id);
    assert_eq!(
        aggregate.source_roots.len(),
        2,
        "BOTH source roots must surface after real LRU eviction — \
         the Evicted tombstone is the contract STEP_6 depends on",
    );

    // Sorted lexically by source_root.
    assert_eq!(
        aggregate.source_roots[0].source_root,
        PathBuf::from("/repos/a")
    );
    assert_eq!(
        aggregate.source_roots[0].state,
        WorkspaceState::Evicted,
        "production LRU eviction must leave the entry as an Evicted tombstone",
    );
    assert_eq!(
        aggregate.source_roots[1].source_root,
        PathBuf::from("/repos/b")
    );
    assert_eq!(aggregate.source_roots[1].state, WorkspaceState::Loaded);

    assert!(
        aggregate.partially_evicted(),
        "aggregate with one Evicted + one Loaded must report partially_evicted == true"
    );

    // Sanity: ws_b is still alive and serving.
    assert_eq!(ws_b.load_state(), WorkspaceState::Loaded);
    // Sanity: the Evicted tombstone has zero resident bytes (the
    // ArcSwap was emptied + memory_bytes zeroed in
    // `execute_eviction`).
    assert_eq!(
        aggregate.source_roots[0].current_bytes, 0,
        "evicted tombstone must report zero resident bytes",
    );
}

#[test]
fn concurrent_daemon_load_with_same_workspace_id_is_idempotent() {
    // Two simulated `daemon/load` callers race for the SAME
    // (workspace_id, source_root, root_mode, config_fingerprint).
    // The manager's HashMap dedups equal WorkspaceKeys: there must be
    // exactly one map entry, no duplicate WorkspaceKeys, no double
    // build (the second insert_workspace_in_state call is a no-op
    // overwrite, which mirrors the get_or_load CAS semantics — only
    // one caller wins the lifecycle gate).
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x77; 32]);
    let key1 = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/shared"),
        ProjectRootMode::GitRoot,
        0,
    );
    let key2 = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/shared"),
        ProjectRootMode::GitRoot,
        0,
    );
    assert_eq!(key1, key2, "two equal keys must compare Eq");
    // Hash equality follows from Eq + the derived `Hash` impl on
    // `WorkspaceKey`; we verify the operational consequence — a
    // `HashMap::insert` of an equal key collapses to a single entry —
    // below.

    let _ws1 = insert_loaded(&mgr, &key1, false, 500);
    // Re-insert under the equal key — production `get_or_load`
    // performs the same idempotent dedup via the HashMap entry API.
    let _ws2 = insert_loaded(&mgr, &key2, false, 500);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    assert_eq!(
        aggregate.source_roots.len(),
        1,
        "concurrent daemon/load with equal keys must produce exactly one entry"
    );
}

#[test]
fn pinned_source_roots_survive_lru_pressure() {
    // A pinned source root in a logical workspace must NEVER be the
    // LRU victim — even when it is the oldest.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x99; 32]);
    let key_pin = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/pinned"),
        ProjectRootMode::GitRoot,
        0,
    );
    let key_evict = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/evictable"),
        ProjectRootMode::GitRoot,
        0,
    );

    let _ws_pin = insert_loaded(&mgr, &key_pin, true, 1_000);
    let _ws_evict = insert_loaded(&mgr, &key_evict, false, 1_000);

    // pinned source root is older but pinned; LRU must skip it.
    std::thread::sleep(std::time::Duration::from_millis(2));
    // No touch on key_evict — the natural newer last_accessed isn't
    // necessary because LRU filters out pinned regardless.

    let evicted = mgr.evict_lru().expect("non-pinned candidate");
    assert_eq!(
        evicted, key_evict,
        "evict_lru must skip pinned source roots even when they are oldest"
    );
}

#[test]
fn unpinned_source_roots_evicted_in_lru_order() {
    // Three unpinned source roots; calling `evict_lru` repeatedly
    // takes them in oldest-first order.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0xaa; 32]);
    let key_a =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/a"), ProjectRootMode::GitRoot, 0);
    let key_b =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/b"), ProjectRootMode::GitRoot, 0);
    let key_c =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/c"), ProjectRootMode::GitRoot, 0);

    let ws_a = insert_loaded(&mgr, &key_a, false, 100);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let ws_b = insert_loaded(&mgr, &key_b, false, 100);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let ws_c = insert_loaded(&mgr, &key_c, false, 100);

    let _ = (&ws_a, &ws_b, &ws_c); // keep Arcs alive

    let evicted_first = mgr.evict_lru().expect("first victim");
    assert_eq!(evicted_first, key_a, "oldest evicted first");
    let evicted_second = mgr.evict_lru().expect("second victim");
    assert_eq!(evicted_second, key_b, "next-oldest evicted second");
    let evicted_third = mgr.evict_lru().expect("third victim");
    assert_eq!(evicted_third, key_c, "last evicted last");
    assert!(
        mgr.evict_lru().is_none(),
        "after every source root is evicted, evict_lru returns None"
    );
}
