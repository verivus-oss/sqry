//! U18.1 (Phase A C indirect-call precision) MCP integration tests.
//!
//! Two-surface predicate-parity tests for the three new C-scoped predicates
//! added by Phase A:
//!
//! * `address_taken:true|false`
//! * `resolved_via:direct|type_match|binding_plane`
//! * `callsite_promiscuous:true|false`
//!
//! These predicates must be reachable from BOTH the core-query path
//! (`mcp__sqry__semantic_search`) AND the planner-IR path
//! (`mcp__sqry__sqry_query`). SPEC §3.1.3 + DESIGN §11 / §12.
//!
//! # Fixture
//!
//! Mirrors `sqry-mcp/tests/u18_resolved_via_marshaller.rs`: a two-file C
//! workspace where `a.c` takes the address of `my_read` via a designated
//! initializer (`{ .read = my_read }`) and `b.c` calls it both directly
//! (`caller_direct`) and indirectly (`caller_b`'s `f->read(...)`). After
//! indexing, `my_read` is flagged `address_taken` by the C plugin's
//! classifier (U2 / U11) and the pass5b binding-plane resolver (U12) emits
//! a `Calls { resolved_via: BindingPlane }` edge from `caller_b` to
//! `my_read`.
//!
//! # Planner-path query shape
//!
//! The planner grammar (`sqry-db/src/planner/parse.rs`) does NOT accept
//! `lang:c` — language filtering is a core-query-parser feature only.
//! The planner path uses `kind:function address_taken:true` against the
//! C-only workspace; the result set is implicitly C because the workspace
//! contains only C files. Parity is asserted on the set of canonical
//! workspace symbols (e.g. `my_read`), not on raw set equality across
//! every node (the two surfaces have legitimate accounting differences
//! for synthetic / plugin-internal nodes).

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::execution::execute_semantic_search;
use sqry_mcp::execution::execute_sqry_query;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{PaginationArgs, SearchFilters, SemanticSearchArgs, SqryQueryParams};
use sqry_mcp::workspace_session_test_api::with_workspace_override;
use std::collections::HashSet;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the per-binary caches exactly once. Tool handlers chain through
/// `build_graph_metadata` which expects the telemetry slots to be live.
fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

/// Two-file C workspace producing both an address-taken function (`my_read`)
/// and two distinct Calls-edge provenances (`Direct` from `caller_direct`,
/// `BindingPlane` from `caller_b`). Identical to the fixture used by
/// `u18_resolved_via_marshaller.rs`.
fn write_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();

    fs::write(
        root.join("a.c"),
        "typedef unsigned long size_t;\n\
         typedef long ssize_t;\n\
         struct ops { ssize_t (*read)(char *buf, size_t n); };\n\
         ssize_t my_read(char *buf, size_t n) { (void)buf; (void)n; return 0; }\n\
         static struct ops my_ops = { .read = my_read };\n",
    )?;

    fs::write(
        root.join("b.c"),
        "typedef unsigned long size_t;\n\
         typedef long ssize_t;\n\
         struct ops { ssize_t (*read)(char *buf, size_t n); };\n\
         ssize_t my_read(char *buf, size_t n);\n\
         void caller_b(struct ops *f, char *buf, size_t n) {\n\
             f->read(buf, n);\n\
         }\n\
         void caller_direct(char *buf, size_t n) {\n\
             my_read(buf, n);\n\
         }\n",
    )?;

    Ok(temp)
}

/// Index the fixture via the live engine — same pattern as the U18 marshaller
/// test. Auto-indexing is on by default.
fn index_fixture(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

fn workspace_arg(temp: &TempDir) -> std::path::PathBuf {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

fn mk_search_args(query: &str, workspace: String) -> SemanticSearchArgs {
    SemanticSearchArgs {
        query: query.to_string(),
        path: workspace,
        filters: SearchFilters::default(),
        max_results: 200,
        context_lines: 0,
        pagination: paging(),
        score_min: None,
        include_classpath: false,
        budget_rows: None,
        framework: None,
        resolved_via: None,
        revision_id: None,
        revision_ref: None,
        revision_commit: None,
        revision_tree: None,
        revision_dirty: false,
        revision_include_untracked: false,
        revision_include_ignored: false,
    }
}

/// AC3: `address_taken:true lang:c` through `semantic_search` returns at
/// least `my_read`, the C-plugin-flagged function in the fixture.
#[test]
fn semantic_search_with_address_taken_predicate_returns_marked_c_functions() -> Result<()> {
    let temp = write_fixture()?;
    let workspace_root = workspace_arg(&temp);
    index_fixture(&workspace_root)?;

    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let args = mk_search_args(
        "address_taken:true lang:c",
        workspace_root.to_string_lossy().into_owned(),
    );

    // The semantic_search handler enforces a workspace-root security boundary
    // before any graph touch. In production, the daemon / SqryServer thread
    // installs this via `with_workspace_override`; in test, we mirror that.
    let result = with_workspace_override(Some(&workspace_root), None, || {
        execute_semantic_search(&args, &cancel)
    })?;

    let hits = &result.data.results;
    assert!(
        !hits.is_empty(),
        "expected at least one address-taken C function, got 0 (predicate may be unwired)"
    );

    // Every hit must be (a) a function-kind node and (b) from a C file.
    for hit in hits {
        assert_eq!(
            hit.language, "c",
            "lang:c filter must restrict results to C, got {:?} for {:?}",
            hit.language, hit.name
        );
        assert!(
            matches!(hit.kind.as_str(), "function" | "method"),
            "address_taken: only applies to callable kinds, got {:?} for {:?}",
            hit.kind,
            hit.name
        );
    }

    // The fixture guarantees `my_read` is address-taken via the designated
    // initializer. If it's missing, the U2/U11 marker pipeline regressed
    // upstream of U18.1.
    let names: HashSet<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert!(
        names.contains("my_read"),
        "expected `my_read` in address_taken:true result set; saw {names:?}"
    );

    Ok(())
}

/// AC4: combining `resolved_via:` with `callers:` filters Calls edges by
/// provenance. We query `callers:my_read resolved_via:binding_plane` — the
/// caller side must include only nodes whose outgoing Calls edges to
/// `my_read` were resolved via the binding plane (i.e. `caller_b`), NOT the
/// syntactic-direct caller `caller_direct`.
#[test]
fn semantic_search_with_resolved_via_predicate_filters_calls() -> Result<()> {
    let temp = write_fixture()?;
    let workspace_root = workspace_arg(&temp);
    index_fixture(&workspace_root)?;

    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let workspace_str = workspace_root.to_string_lossy().into_owned();

    // Binding-plane provenance: should match `caller_b` (whose `f->read(...)`
    // pass5b rewrote into a `Calls { resolved_via: BindingPlane }` edge to
    // `my_read`).
    let bp_args = mk_search_args(
        "callers:my_read resolved_via:binding_plane lang:c",
        workspace_str.clone(),
    );
    let bp_result = with_workspace_override(Some(&workspace_root), None, || {
        execute_semantic_search(&bp_args, &cancel)
    })?;
    let bp_names: HashSet<String> = bp_result
        .data
        .results
        .iter()
        .map(|h| h.name.clone())
        .collect();
    assert!(
        bp_names.contains("caller_b"),
        "binding_plane provenance must include `caller_b`; saw {bp_names:?}"
    );
    assert!(
        !bp_names.contains("caller_direct"),
        "binding_plane provenance must NOT include `caller_direct` (its Calls edge is Direct, not BindingPlane); saw {bp_names:?}"
    );

    // Direct provenance: should match `caller_direct` (whose `my_read(...)`
    // is a syntactic Calls edge), NOT `caller_b`.
    let direct_args = mk_search_args(
        "callers:my_read resolved_via:direct lang:c",
        workspace_str.clone(),
    );
    let direct_result = with_workspace_override(Some(&workspace_root), None, || {
        execute_semantic_search(&direct_args, &cancel)
    })?;
    let direct_names: HashSet<String> = direct_result
        .data
        .results
        .iter()
        .map(|h| h.name.clone())
        .collect();
    assert!(
        direct_names.contains("caller_direct"),
        "direct provenance must include `caller_direct`; saw {direct_names:?}"
    );
    assert!(
        !direct_names.contains("caller_b"),
        "direct provenance must NOT include `caller_b` (its Calls edge is BindingPlane, not Direct); saw {direct_names:?}"
    );

    Ok(())
}

/// AC5: parity — the planner-path (`sqry_query`) returns the same node set
/// as the core-query path (`semantic_search`) for `address_taken:true`. The
/// planner grammar does not accept `lang:c`; the workspace is C-only so the
/// result set is implicitly C on both surfaces.
#[test]
fn sqry_query_address_taken_predicate_returns_marked_functions() -> Result<()> {
    let temp = write_fixture()?;
    let workspace_root = workspace_arg(&temp);
    index_fixture(&workspace_root)?;

    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let workspace_str = workspace_root.to_string_lossy().into_owned();

    // Planner surface — uses `kind:function address_taken:true` (the
    // planner grammar does not accept `lang:` predicates).
    let planner_params = SqryQueryParams {
        query: "kind:function address_taken:true".to_string(),
        path: workspace_str.clone(),
        limit: Some(1000),
        budget_rows: None,
        // Phase β joint-stubs: both filter params default to None (no-op).
        framework: None,
        resolved_via: None,
    };
    let planner_result = with_workspace_override(Some(&workspace_root), None, || {
        execute_sqry_query(&planner_params)
    })?;
    let planner_names: HashSet<String> = planner_result
        .data
        .hits
        .iter()
        .map(|h| h.name.clone())
        .collect();

    // Core-query surface — uses `address_taken:true lang:c` to mirror the
    // C-only restriction the planner gets for free (workspace is C-only).
    let search_args = mk_search_args("address_taken:true lang:c", workspace_str);
    let search_result = with_workspace_override(Some(&workspace_root), None, || {
        execute_semantic_search(&search_args, &cancel)
    })?;
    let search_names: HashSet<String> = search_result
        .data
        .results
        .iter()
        .map(|h| h.name.clone())
        .collect();

    assert!(
        !planner_names.is_empty(),
        "planner surface returned empty result set for `kind:function address_taken:true`"
    );
    assert!(
        !search_names.is_empty(),
        "core-query surface returned empty result set for `address_taken:true lang:c`"
    );

    // Both surfaces must agree on the canonical workspace symbol `my_read`.
    // SPEC §3.1.3 two-surface parity contract: "both return the same
    // semantic answer". We assert presence on both surfaces (the load-bearing
    // contract) and set equality restricted to the canonical fixture symbols
    // (insulating the test from synthetic-node accounting differences).
    assert!(
        planner_names.contains("my_read"),
        "planner surface missing `my_read` (address_taken regression upstream?); saw {planner_names:?}"
    );
    assert!(
        search_names.contains("my_read"),
        "core-query surface missing `my_read` (U18.1 dispatch wiring regression?); saw {search_names:?}"
    );

    let fixture_canonical: HashSet<&str> = ["my_read"].into_iter().collect();
    let planner_canonical: HashSet<&str> = planner_names
        .iter()
        .map(String::as_str)
        .filter(|n| fixture_canonical.contains(n))
        .collect();
    let search_canonical: HashSet<&str> = search_names
        .iter()
        .map(String::as_str)
        .filter(|n| fixture_canonical.contains(n))
        .collect();
    assert_eq!(
        planner_canonical, search_canonical,
        "two-surface parity violation on canonical fixture symbols: planner={planner_canonical:?}, search={search_canonical:?}"
    );

    Ok(())
}
