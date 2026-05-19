//! U18 iter-2 (Phase A C indirect-call precision) MCP integration test.
//!
//! Locks the JSON-metadata marshaller in
//! [`sqry_mcp::execution::tools::graph::call_edge_metadata`] to surface
//! the [`sqry_core::graph::unified::edge::ResolvedVia`] discriminator on
//! every emitted Calls-relation edge.
//!
//! # Why a separate test from `u18_resolved_via_marshaller.rs`
//!
//! Codex iter-1 review of U18 noted that the original U18 fix only
//! patched `sqry-mcp/src/execution/tools/relations.rs::collect_call_relation_via_db`,
//! while a second independent JSON-metadata emission site —
//! `call_edge_metadata` in `sqry-mcp/src/execution/tools/graph.rs` — was
//! left destructuring `EdgeKind::Calls` with `..` and dropped
//! `resolved_via` on every payload. That helper backs the `metadata`
//! field on `RelationEdgeData` for two MCP tools:
//!
//! * `show_dependencies` (via
//!   `process_unified_call_edges` →
//!   `insert_relation_edge(..., call_edge_metadata(&edge.kind))`)
//! * `subgraph` (via
//!   `add_call_edge(..., metadata: call_edge_metadata(edge_kind))`)
//!
//! This test drives `show_dependencies` end-to-end (the simpler of the
//! two surfaces — `subgraph` requires symbol resolution to a single
//! start node, while `show_dependencies` accepts a `symbol_name` and
//! walks both incoming and outgoing Calls edges). The codex finding is
//! about the helper, not about a specific tool; locking the helper via
//! one of its consumers is sufficient because both consumers funnel
//! through the same emission point.
//!
//! # Fixture shape
//!
//! Re-uses the two-file C workspace from
//! `u18_resolved_via_marshaller.rs` — `a.c` defines `my_read` plus a
//! `static struct ops my_ops = { .read = my_read }` designated
//! initializer; `b.c` declares the same `struct ops` shape and defines
//! both a `caller_direct` (syntactic direct call) and `caller_b`
//! (`f->read(...)` rewritten by pass5b's binding-plane fallback).
//! After indexing, querying `show_dependencies symbol=my_read` returns
//! at least two Calls-relation edges whose `metadata` JSON object
//! carries `"resolved_via": "direct"` and `"resolved_via": "binding_plane"`
//! respectively. The fixture is copied (not shared via a helper module)
//! to keep the test file self-contained per the IMPL-PLAN's "additive
//! marshalling only" scope guard.

use anyhow::Result;
use serde_json::Value;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{PaginationArgs, ShowDependenciesArgs};
use sqry_mcp::tool_handlers::execute_get_dependencies;
use std::collections::HashSet;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the path-resolver discovery cache, engine cache, and the
/// trace-path / subgraph telemetry caches exactly once across the whole
/// test binary. `execute_get_dependencies` chains through
/// `build_graph_metadata` which expects the telemetry slots to be
/// initialized.
fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

/// Write the two-file C workspace that yields both `Direct` and
/// `BindingPlane` Calls edges into `my_read`. See module rustdoc.
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

fn index_fixture(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[test]
fn show_dependencies_response_includes_resolved_via_field_for_calls_edges() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = ShowDependenciesArgs {
        file_path: None,
        symbol_name: Some("my_read".to_string()),
        path: workspace_arg(&temp),
        max_depth: 2,
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_get_dependencies(&args)?;

    let edges = &result.data.edges;
    assert!(
        !edges.is_empty(),
        "expected at least one Calls edge in the dependency graph anchored on `my_read`, got 0"
    );

    // Every emitted Calls-relation edge must carry a `metadata` JSON
    // object that includes the U18 `resolved_via` field. The serde
    // string form is snake_case per `ResolvedVia`'s
    // `#[serde(rename_all = "snake_case")]` — verified against
    // `sqry-core/src/graph/unified/edge/kind.rs:60`.
    let valid_values: HashSet<&str> = ["direct", "type_match", "binding_plane"]
        .into_iter()
        .collect();

    let mut seen_resolutions: HashSet<String> = HashSet::new();
    let mut calls_edge_count = 0usize;
    for edge in edges {
        // `show_dependencies` only emits Calls-derived edges via
        // `process_unified_call_edges`, which filters on
        // `EdgeKind::Calls { .. }`. Both `callers` and `callees`
        // relation_type values route through `call_edge_metadata`.
        if edge.relation_type != "callers" && edge.relation_type != "callees" {
            continue;
        }
        calls_edge_count += 1;

        let metadata = edge.metadata.as_ref().unwrap_or_else(|| {
            panic!("Calls edge missing metadata JSON: {edge:?}");
        });
        let object = metadata
            .as_object()
            .unwrap_or_else(|| panic!("metadata must be a JSON object, got {metadata:?}"));
        let resolved_via = object
            .get("resolved_via")
            .unwrap_or_else(|| panic!("metadata JSON missing `resolved_via` field: {metadata:?}"));
        let resolved_str = match resolved_via {
            Value::String(s) => s.clone(),
            other => panic!("`resolved_via` must be a JSON string, got {other:?}"),
        };
        assert!(
            valid_values.contains(resolved_str.as_str()),
            "`resolved_via` value {resolved_str:?} not in expected serde-snake_case set {valid_values:?}"
        );
        seen_resolutions.insert(resolved_str);
    }

    assert!(
        calls_edge_count > 0,
        "expected at least one Calls-relation edge (relation_type callers/callees) in payload, \
         saw types: {:?}",
        edges.iter().map(|e| &e.relation_type).collect::<Vec<_>>()
    );

    // The fixture deliberately exercises two distinct provenances:
    // `caller_direct` produces a syntactic Direct call; `caller_b`'s
    // `f->read(...)` is rewritten by pass5b into a precise
    // BindingPlane Calls edge anchored on `my_read`. If either is
    // missing the marshaller has regressed (or the upstream resolver
    // has drifted — distinct bug, still observable here).
    assert!(
        seen_resolutions.contains("direct"),
        "expected at least one `resolved_via=\"direct\"` Calls edge \
         (caller_direct → my_read); saw: {seen_resolutions:?}"
    );
    assert!(
        seen_resolutions.contains("binding_plane"),
        "expected at least one `resolved_via=\"binding_plane\"` Calls \
         edge (caller_b's f->read(...) rewritten by pass5b → my_read); \
         saw: {seen_resolutions:?}"
    );

    Ok(())
}
