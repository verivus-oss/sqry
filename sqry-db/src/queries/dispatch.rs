//! Shared query-layer dispatch helpers for name-keyed predicate handlers.
//!
//! # Why this module lives in `sqry-db`
//!
//! `QueryDb` construction and the graph_eval-style relation inversion
//! wrappers are used by **multiple** transport layers (MCP and CLI, and
//! potentially LSP in the future). Prior to DB18 these helpers lived in
//! `sqry-mcp/src/execution/relation_dispatch.rs` as `pub(crate)` — which
//! meant the CLI had no way to route through them and was reimplementing
//! the same `snapshot.edges().reverse()` / `edges_from()` walks inline.
//! DB18 lifts them into `sqry-db` (the canonical home for query-layer
//! concerns) so every transport shares the same dispatch table.
//!
//! # Why the inversion wrappers exist
//!
//! sqry-db's planner-side relation queries follow the planner convention
//! "filter rows whose `<relation>` set contains the key". That means
//! [`CallersQuery`] keyed on `X` returns nodes whose **`callers` set**
//! includes `X`, i.e., nodes that `X` calls (callees of `X` under the
//! colloquial reading). MCP / CLI consumers, mirroring `graph_eval` and
//! standard LSP semantics, expect the inverted graph_eval-style convention:
//! `direct_callers(X)` returns nodes that call `X`.
//!
//! Codex's DB14 review (`docs/reviews/phase3-derived-db/2026-04-14/db14_review_post_codex.md`)
//! explicitly recommended *not* flipping sqry-db's queries to match the
//! graph_eval naming ("flipping DB14 to match `graph_eval::match_*` would
//! make the names more aesthetically uniform while breaking the actual
//! set-membership contract you are caching"). DB15 implemented the second
//! option Codex left open: route through inverted wrappers in one place.
//! DB18 shares that dispatch table across transports.
//!
//! # Direction crib sheet
//!
//! | Transport-facing call | sqry-db query | Returns |
//! |---|---|---|
//! | `mcp_callers_query(db, X)` | `db.get::<CalleesQuery>(X)` | nodes that call `X` (callers of `X`) |
//! | `mcp_callees_query(db, X)` | `db.get::<CallersQuery>(X)` | nodes that `X` calls (callees of `X`) |
//! | `mcp_imports_query(db, X)` | `db.get::<ImportsQuery>(X)` | nodes whose outgoing `Imports` target matches `X` |
//! | `mcp_exports_query(db, X)` | `db.get::<ExportsQuery>(X)` | nodes participating in an `Exports` edge whose other endpoint matches `X` |
//! | `mcp_references_query(db, X)` | `db.get::<ReferencesQuery>(X)` | nodes whose incoming reference edges have a source matching `X` |
//!
//! Don't bypass these wrappers from transport handlers — the inversion is
//! easy to get wrong. Golden tests in
//! `sqry-mcp/tests/migration_golden_relations_test.rs` and
//! `sqry-cli/tests/migration_golden_cli_test.rs` lock the expected direction.
//!
//! # Which handlers route through this module?
//!
//! `direct_callers` and `direct_callees` (both MCP and CLI) use
//! `mcp_callers_query` and `mcp_callees_query` — these tools take a
//! user-supplied symbol name as the predicate value, which is exactly what
//! sqry-db's name-keyed relation queries are designed for.
//!
//! The multi-hop `relation_query` handler does NOT route through this
//! module. Stripped-name dispatch broadens the BFS frontier with unrelated
//! same-named chains (post-DB15 Codex review finding); the structural fix
//! is to enumerate Calls edges directly from the
//! `find_nodes_by_name`-resolved start nodes (a NodeId-anchored operation,
//! not a name-keyed predicate). See
//! `sqry-mcp/src/execution/tools/relations.rs::collect_call_relation_via_db`
//! for the rationale.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;

use crate::persistence::{LoadError, LoadOutcome, compute_file_sha256, load_derived};
use crate::queries::{
    CalleesQuery, CallersQuery, ExportsQuery, ImportsQuery, ReferencesQuery, RelationKey,
};
use crate::{QueryDb, QueryDbConfig};

/// Build a per-call `QueryDb` over the supplied snapshot.
///
/// Using a fresh `QueryDb` per transport call keeps per-snapshot caches
/// isolated and avoids cross-request poisoning. The cost is one shard-array
/// allocation; relation queries themselves are O(N) graph scans the first
/// time and O(1) cache hits thereafter, so the construction is amortized.
///
/// This variant does NOT attempt opportunistic cold-load. Use
/// [`make_query_db_cold`] when you have the `workspace_root` available and
/// want cached derived facts restored from the companion `derived.sqry` file.
#[must_use]
pub fn make_query_db(snapshot: Arc<GraphSnapshot>) -> QueryDb {
    QueryDb::new(snapshot, QueryDbConfig::default())
}

/// Build a per-call `QueryDb` over the supplied snapshot, then opportunistically
/// cold-load from the workspace's `derived.sqry` companion file.
///
/// # Cold-load semantics
///
/// - `Ok(Applied)` → cache is warm; subsequent `db.get::<Q>()` calls will be
///   O(1) cache hits for loaded entries.
/// - `NotFound` → silent (expected on fresh workspaces).
/// - `StaleSnapshot` / `Corrupt` → file is deleted, DB stays pristine.
/// - Other errors → logged as `warn`, DB stays pristine.
///
/// The returned `QueryDb` is always usable. On any cold-load error the DB is
/// the same pristine state as `make_query_db(snapshot)` would have returned.
///
/// # Panics
///
/// Does not panic. All cold-load errors are handled gracefully.
#[must_use]
pub fn make_query_db_cold(snapshot: Arc<GraphSnapshot>, workspace_root: &Path) -> QueryDb {
    let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let _ = load_derived_opportunistic(&mut db, workspace_root);
    db
}

/// Compute the canonical path to a workspace's derived-cache file.
///
/// Convention: `<workspace_root>/.sqry/graph/<config.derived_persistence_filename>`.
#[must_use]
pub fn derived_path(workspace_root: &Path, config: &QueryDbConfig) -> PathBuf {
    workspace_root
        .join(".sqry")
        .join("graph")
        .join(&config.derived_persistence_filename)
}

/// Opportunistic cold-load wrapper for non-sqryd callers.
///
/// Reads the snapshot SHA-256 from the on-disk file
/// `<workspace_root>/.sqry/graph/snapshot.sqry`, then calls
/// [`load_derived`] against the companion `derived.sqry` file.
///
/// Error policy (CLIENT_LOAD spec §5.9):
/// - `Ok(Applied)` / `Ok(Skipped)` → logged at `debug` level.
/// - `Err(NotFound)` → silent; expected on fresh workspaces.
/// - `Err(StaleSnapshot)` / `Err(Corrupt)` → derived file deleted +
///   logged at `info` level.
/// - `Err(AlreadyLoaded)` → logged at `debug` level.
/// - Other `Err` → logged at `warn` level.
///
/// # Errors
///
/// Returns `Err` propagated from [`load_derived`] for all non-`NotFound`
/// cases, but callers can safely ignore the `Err` — the `db` is always
/// pristine (fully usable) when `Err` is returned.
pub fn load_derived_opportunistic(
    db: &mut QueryDb,
    workspace_root: &Path,
) -> Result<LoadOutcome, LoadError> {
    let config = db.config().clone();
    let snapshot_path = workspace_root
        .join(".sqry")
        .join("graph")
        .join("snapshot.sqry");
    let derived = derived_path(workspace_root, &config);

    // Compute the snapshot SHA-256 from the on-disk file.  If the file does
    // not exist, treat it as a soft miss — no cold load is possible.
    let snapshot_sha256 = match compute_file_sha256(&snapshot_path) {
        Ok(sha) => sha,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No snapshot on disk yet — not an error condition.
            return Err(LoadError::NotFound {
                path: snapshot_path,
            });
        }
        Err(e) => {
            log::warn!("PN3 cold load: failed to hash snapshot file: {e}");
            return Err(LoadError::Io(e));
        }
    };

    match load_derived(db, snapshot_sha256, &derived, workspace_root) {
        Ok(outcome) => {
            match &outcome {
                LoadOutcome::Applied { entries } => {
                    log::debug!(
                        "PN3 cold load: {entries} entries applied from {}",
                        derived.display()
                    );
                }
                LoadOutcome::Skipped(reason) => {
                    log::debug!("PN3 cold load skipped: {reason:?}");
                }
            }
            Ok(outcome)
        }
        Err(LoadError::NotFound { .. }) => {
            // Expected on fresh workspaces — silent.
            Err(LoadError::NotFound { path: derived })
        }
        Err(err @ (LoadError::StaleSnapshot | LoadError::Corrupt { .. })) => {
            let _ = std::fs::remove_file(&derived);
            log::info!("PN3 discarded stale/corrupt derived-cache file: {err}");
            Err(err)
        }
        Err(LoadError::AlreadyLoaded) => {
            log::debug!("PN3 cold load: AlreadyLoaded (unexpected for fresh DB)");
            Err(LoadError::AlreadyLoaded)
        }
        Err(err) => {
            log::warn!("PN3 cold load failed: {err}");
            Err(err)
        }
    }
}

/// Transport-facing `direct_callers(X)` — return nodes that call `X`.
///
/// **Inverts** the planner naming: dispatches to sqry-db's
/// [`CalleesQuery`] keyed on `X`, which under the planner contract returns
/// nodes whose `callees` set includes `X` — i.e., callers of `X` under the
/// `graph_eval` convention MCP/CLI expose.
#[must_use]
pub fn mcp_callers_query(db: &QueryDb, key: &RelationKey) -> Arc<Vec<NodeId>> {
    db.get::<CalleesQuery>(key)
}

/// Transport-facing `direct_callees(X)` — return nodes that `X` calls.
///
/// **Inverts** the planner naming: dispatches to sqry-db's
/// [`CallersQuery`] keyed on `X`, which under the planner contract returns
/// nodes whose `callers` set includes `X` — i.e., callees of `X` under the
/// `graph_eval` convention MCP/CLI expose.
#[must_use]
pub fn mcp_callees_query(db: &QueryDb, key: &RelationKey) -> Arc<Vec<NodeId>> {
    db.get::<CallersQuery>(key)
}

/// Transport-facing `relation_query relation:imports symbol:X` — return
/// nodes whose outgoing `Imports` edge target/alias/wildcard matches `X`.
///
/// Aligned per-node semantic across sqry-db and `graph_eval` (post-DB15).
///
/// Not currently called from `execute_relation_query` because that handler
/// keeps the legacy NodeId-anchored "what does X import" semantic rather
/// than the planner-canonical "which nodes import X" semantic; see the
/// module docstring on [`sqry_mcp::execution::tools::relations`]. Kept
/// here so the dispatch table is complete and future tools
/// (e.g. a `things_importing` surface) can route through one shared
/// inversion table.
#[must_use]
pub fn mcp_imports_query(db: &QueryDb, key: &RelationKey) -> Arc<Vec<NodeId>> {
    db.get::<ImportsQuery>(key)
}

/// Transport-facing `relation_query relation:exports symbol:X` — return
/// nodes participating in an `Exports` edge whose other endpoint matches `X`.
///
/// See [`mcp_imports_query`] for the explanation of why this wrapper is
/// not currently called from `execute_relation_query`.
#[must_use]
pub fn mcp_exports_query(db: &QueryDb, key: &RelationKey) -> Arc<Vec<NodeId>> {
    db.get::<ExportsQuery>(key)
}

/// Transport-facing `relation_query relation:references symbol:X` — return
/// nodes whose incoming reference edges (`Calls`, `References`, `Imports`,
/// `FfiCall`) have a source matching `X`.
///
/// Reserved for future transport exposure; not currently routed because the
/// public `RelationType` enum in sqry-mcp does not yet expose `References`
/// (only Callers/Callees/Imports/Exports/Returns). Keeping the wrapper here
/// so the dispatch table is complete and auditable in one place.
#[must_use]
pub fn mcp_references_query(db: &QueryDb, key: &RelationKey) -> Arc<Vec<NodeId>> {
    db.get::<ReferencesQuery>(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;

    /// Builds: `main --Calls--> helper`, `isolated` has no edges. Returns
    /// `(snapshot, main_id, helper_id, isolated_id)`.
    fn build_caller_graph() -> (Arc<GraphSnapshot>, NodeId, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.rs")).unwrap();
        let main_name = graph.strings_mut().intern("main").unwrap();
        let helper_name = graph.strings_mut().intern("helper").unwrap();
        let isolated_name = graph.strings_mut().intern("isolated").unwrap();

        let main_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file_id)
                    .with_qualified_name(main_name),
            )
            .unwrap();
        let helper_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, helper_name, file_id)
                    .with_qualified_name(helper_name),
            )
            .unwrap();
        let isolated_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, isolated_name, file_id)
                    .with_qualified_name(isolated_name),
            )
            .unwrap();

        graph.edges_mut().add_edge(
            main_id,
            helper_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        (Arc::new(graph.snapshot()), main_id, helper_id, isolated_id)
    }

    #[test]
    fn mcp_callers_query_returns_callers_of_symbol_under_graph_eval_convention() {
        // direct_callers("helper") must return main (main calls helper).
        let (snapshot, main_id, helper_id, isolated_id) = build_caller_graph();
        let db = make_query_db(snapshot);

        let key = RelationKey::exact("helper");
        let callers_of_helper = mcp_callers_query(&db, &key);

        assert!(
            callers_of_helper.contains(&main_id),
            "main is a caller of helper"
        );
        assert!(
            !callers_of_helper.contains(&helper_id),
            "helper does not call itself"
        );
        assert!(
            !callers_of_helper.contains(&isolated_id),
            "isolated has no calls"
        );
    }

    #[test]
    fn mcp_callees_query_returns_callees_of_symbol_under_graph_eval_convention() {
        // direct_callees("main") must return helper (main calls helper).
        let (snapshot, main_id, helper_id, isolated_id) = build_caller_graph();
        let db = make_query_db(snapshot);

        let key = RelationKey::exact("main");
        let callees_of_main = mcp_callees_query(&db, &key);

        assert!(
            callees_of_main.contains(&helper_id),
            "helper is a callee of main"
        );
        assert!(
            !callees_of_main.contains(&main_id),
            "main does not call itself"
        );
        assert!(
            !callees_of_main.contains(&isolated_id),
            "isolated is not called by main"
        );
    }

    // -----------------------------------------------------------------------
    // CLIENT_LOAD acceptance tests
    // -----------------------------------------------------------------------

    use tempfile::TempDir;

    /// Set up a minimal temp workspace with the `.sqry/graph/` directory tree
    /// and a fake `snapshot.sqry` file so `load_derived_opportunistic` can
    /// compute a SHA.  Returns `(TempDir, workspace_root_path, snapshot_sha)`.
    fn setup_workspace() -> (TempDir, std::path::PathBuf, [u8; 32]) {
        let dir = TempDir::new().unwrap();
        let workspace_root = dir.path().to_path_buf();
        let graph_dir = workspace_root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).unwrap();

        // Write a fake snapshot file — the SHA is the SHA-256 of these bytes.
        let snapshot_bytes = b"fake_snapshot_content_for_test";
        let snapshot_path = graph_dir.join("snapshot.sqry");
        std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

        let sha = crate::persistence::compute_file_sha256(&snapshot_path).unwrap();
        (dir, workspace_root, sha)
    }

    /// Write a valid derived stream to `<workspace>/.sqry/graph/derived.sqry`
    /// using the given snapshot SHA.
    fn write_derived_file(workspace_root: &std::path::Path, sha: [u8; 32], n_entries: usize) {
        use crate::persistence::{
            DerivedHeader, PersistedEntry, QueryDeps, serialize_derived_stream,
        };
        use crate::queries::type_ids;

        let entries: Vec<PersistedEntry> = (0..n_entries)
            .map(|i| PersistedEntry {
                query_type_id: type_ids::CALLERS,
                raw_key_bytes: vec![i as u8],
                raw_result_bytes: vec![0xAA, i as u8],
                deps: QueryDeps::default(),
            })
            .collect();
        let header = DerivedHeader::new(sha, 0, 0, vec![], entries.len() as u64);
        let bytes = serialize_derived_stream(&header, entries).unwrap();
        let derived_path = workspace_root
            .join(".sqry")
            .join("graph")
            .join("derived.sqry");
        std::fs::write(&derived_path, bytes).unwrap();
    }

    // -----------------------------------------------------------------------
    // AC CLIENT_LOAD-1: empty workspace does not create derived file
    // -----------------------------------------------------------------------

    /// `make_query_db_cold` on an empty workspace (no `derived.sqry`) returns a
    /// pristine DB and does NOT create the file.
    #[test]
    fn make_query_db_cold_on_empty_workspace_does_not_create_derived_file() {
        let (dir, workspace_root, _sha) = setup_workspace();

        let derived_path = workspace_root
            .join(".sqry")
            .join("graph")
            .join("derived.sqry");
        assert!(
            !derived_path.exists(),
            "pre-condition: derived.sqry must not exist"
        );

        let (snapshot, _, _, _) = build_caller_graph();
        let db = make_query_db_cold(snapshot, &workspace_root);

        // DB is usable (pristine).
        assert!(
            db.cold_load_allowed(),
            "cold_load_allowed must remain true: no load succeeded"
        );
        // No derived file created.
        assert!(
            !derived_path.exists(),
            "make_query_db_cold must NOT create derived.sqry"
        );

        // Silence "unused variable" warning for `dir`.
        drop(dir);
    }

    // -----------------------------------------------------------------------
    // AC CLIENT_LOAD-2: stale SHA deletes file and returns pristine DB
    // -----------------------------------------------------------------------

    /// `make_query_db_cold` with a `derived.sqry` written for a *different*
    /// snapshot SHA deletes the file and returns a pristine DB.
    #[test]
    fn make_query_db_cold_with_stale_sha_deletes_file_and_returns_pristine_db() {
        let (dir, workspace_root, sha) = setup_workspace();

        // Write a derived.sqry with the CORRECT sha, then overwrite snapshot.sqry
        // with different bytes so the SHA computed at load time no longer matches.
        write_derived_file(&workspace_root, sha, 2);

        let derived_path = workspace_root
            .join(".sqry")
            .join("graph")
            .join("derived.sqry");
        assert!(
            derived_path.exists(),
            "pre-condition: derived.sqry must exist"
        );

        // Overwrite the snapshot file so its SHA no longer matches the derived header.
        let snapshot_path = workspace_root
            .join(".sqry")
            .join("graph")
            .join("snapshot.sqry");
        std::fs::write(&snapshot_path, b"different_snapshot_bytes").unwrap();

        let (snapshot, _, _, _) = build_caller_graph();
        let db = make_query_db_cold(snapshot, &workspace_root);

        // DB is pristine (cold-load was rejected).
        assert!(
            db.cold_load_allowed(),
            "cold_load_allowed must remain true: stale load was rejected"
        );
        // Stale file must have been deleted.
        assert!(
            !derived_path.exists(),
            "make_query_db_cold must delete stale derived.sqry"
        );

        drop(dir);
    }

    // -----------------------------------------------------------------------
    // AC CLIENT_LOAD-3: matching SHA cold-loads entries
    // -----------------------------------------------------------------------

    /// `make_query_db_cold` with a valid `derived.sqry` whose SHA matches
    /// the snapshot file warms the DB's cold-load window.
    #[test]
    fn make_query_db_cold_with_matching_sha_cold_loads() {
        let (dir, workspace_root, sha) = setup_workspace();
        write_derived_file(&workspace_root, sha, 3);

        let (snapshot, _, _, _) = build_caller_graph();
        let db = make_query_db_cold(snapshot, &workspace_root);

        // After a successful cold-load, `cold_load_allowed()` returns false
        // (the cold-load window is closed).
        assert!(
            !db.cold_load_allowed(),
            "cold_load_allowed must be false after a successful load"
        );

        drop(dir);
    }
}
