//! U18 (Phase A C indirect-call precision) MCP integration test.
//!
//! Locks the JSON-metadata marshaller in
//! [`sqry_mcp::execution::tools::relations::collect_call_relation_via_db`]
//! to surface the [`sqry_core::graph::unified::edge::ResolvedVia`]
//! discriminator on every emitted Calls-relation edge.
//!
//! # Routing note (DAG vs IMPLEMENTATION-PLAN reconciliation)
//!
//! The acceptance criterion in
//! `docs/superpowers/plans/2026-05-14-c-semantic-phase-a-icall-precision-dag.toml`
//! names this test
//! `direct_callers_response_includes_resolved_via_field_for_calls_edges`.
//! The literal `direct_callers` MCP tool returns
//! [`sqry_mcp::tool_handlers::execute_direct_callers`]'s
//! `DirectCallersData`, which has no `metadata` field — it carries a
//! flat list of `CallerCalleeData` records. Per Phase A IMPLEMENTATION
//! PLAN §"U18 — MCP edge-marshaller", the JSON `metadata` object that
//! carries `argument_count` / `is_async` / `resolved_via` is emitted
//! only on `RelationEdgeData` values produced by `relation_query`
//! (Callers/Callees routes through `collect_call_relation_via_db` in
//! `relations.rs`). The test therefore drives `execute_relation_query`
//! with `RelationType::Callers`, which is the surface where the U18
//! marshaller actually emits the new field.
//!
//! # Fixture shape
//!
//! Mirrors `sqry-core/tests/pass5b_c_indirect.rs::pass5b_resolves_designated_initializer_binding`
//! — a two-file C workspace where:
//!
//! * `a.c` declares `struct ops { ssize_t (*read)(...); }`, defines
//!   `ssize_t my_read(...)`, and a `static struct ops my_ops = { .read = my_read }`
//!   designated initializer.
//! * `b.c` declares the same `struct ops` shape and defines
//!   `void caller_b(struct ops *f, ...) { f->read(...); }` — a real
//!   indirect callsite resolved by pass5b's binding-plane fallback into
//!   a precise `Calls { resolved_via: BindingPlane }` edge from
//!   `caller_b` to `my_read`.
//! * `b.c` additionally defines `void caller_direct(...) { my_read(...); }`
//!   — a syntactic direct call that the C plugin emits as
//!   `Calls { resolved_via: Direct }`.
//!
//! After indexing, querying `relation_query symbol=my_read relation=callers`
//! must return at least two edges whose `metadata` JSON object carries
//! `"resolved_via": "direct"` and `"resolved_via": "binding_plane"`
//! respectively.

use anyhow::Result;
use serde_json::Value;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{PaginationArgs, RelationQueryArgs, RelationType};
use sqry_mcp::tool_handlers::execute_relation_query;
use std::collections::HashSet;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the path-resolver discovery cache, engine cache, and the
/// trace-path / subgraph telemetry caches exactly once across the whole
/// test binary. The relation handler chains through `build_graph_metadata`
/// which expects the telemetry slots to be initialized.
fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

/// Standard pagination args (no pagination, deterministic).
fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

/// Write a two-file C workspace that yields both a `Direct` and a
/// `BindingPlane` Calls edge into `my_read`. See the module rustdoc for
/// the fixture rationale.
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

/// Index the fixture via the live engine. Auto-indexing is on by default.
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
fn direct_callers_response_includes_resolved_via_field_for_calls_edges() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = RelationQueryArgs {
        symbol: "my_read".to_string(),
        relation: RelationType::Callers,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_relation_query(&args)?;

    assert_eq!(result.data.relation_type, "callers");
    let edges = &result.data.relations;
    assert!(
        !edges.is_empty(),
        "expected at least one Calls edge into `my_read`, got 0"
    );

    // Every emitted Calls-relation edge must carry a `metadata` JSON
    // object that includes the U18 `resolved_via` field. The serde
    // string form is snake_case per ResolvedVia's
    // `#[serde(rename_all = "snake_case")]` — verified against
    // `sqry-core/src/graph/unified/edge/kind.rs:60`.
    let valid_values: HashSet<&str> = ["direct", "type_match", "binding_plane"]
        .into_iter()
        .collect();

    let mut seen_resolutions: HashSet<String> = HashSet::new();
    for edge in edges {
        let metadata = edge
            .metadata
            .as_ref()
            .unwrap_or_else(|| panic!("Calls edge missing metadata JSON: {edge:?}"));
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

    // The fixture deliberately exercises two distinct provenances:
    // `caller_direct` produces a syntactic Direct call; `caller_b`'s
    // `f->read(...)` is rewritten by pass5b into a precise BindingPlane
    // Calls edge anchored on `my_read`. If either is missing the
    // marshaller has regressed (or the upstream resolver has drifted —
    // which is a different bug, but still observable here).
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
