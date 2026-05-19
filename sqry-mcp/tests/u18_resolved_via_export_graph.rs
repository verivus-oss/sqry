//! U18 iter-2 (codex MEDIUM follow-up) MCP integration test for
//! `export_graph`.
//!
//! Locks the JSON-metadata marshaller in the `export_graph` arm of
//! [`sqry_mcp::execution::tools::graph::classify_edge_for_export`] to
//! surface the [`sqry_core::graph::unified::edge::ResolvedVia`]
//! discriminator on every emitted Calls-relation edge.
//!
//! # Why a separate test from `u18_resolved_via_graph_metadata.rs`
//!
//! The iter-2 codex review of U18 noted that `export_graph` is a third
//! independent JSON-metadata emission site (alongside
//! `call_edge_metadata` for `show_dependencies` / `subgraph` and
//! `collect_call_relation_via_db` for `relations.rs`). It destructures
//! `EdgeKind::Calls` directly inside `classify_edge_for_export` and
//! emits its own `json!({...})` payload that flows into
//! `RelationEdgeData.metadata` via `process_bfs_node_for_export`.
//!
//! Unlike `show_dependencies` (which walks both `Direct` and
//! `BindingPlane` resolutions), `export_graph` restricts its Calls arm
//! to `resolved_via: ResolvedVia::Direct` via pattern guard. So this
//! test only asserts the `"direct"` wire value — the BindingPlane
//! variant is filtered out before reaching the marshaller.
//!
//! # Why a Rust inherent-impl fixture (not the C fixture from iter-1)
//!
//! `process_bfs_node_for_export` filters out nodes whose
//! `qualified_name` resolves to an empty string. Top-level C functions
//! emit empty qualified names under the current C plugin (same shape
//! as Rust crate-root free functions — see the note above
//! `export_graph_runs_and_respects_seed_resolution` in
//! `migration_golden_trace_test.rs`). The Rust inherent-impl shape
//! `AlphaMarker::helper` → `AlphaMarker::inner` produces non-empty
//! qualified names that survive that guard, and both call sites are
//! syntactic direct calls — exactly the `ResolvedVia::Direct`
//! provenance that the `export_graph` marshaller arm pins.

use anyhow::Result;
use serde_json::Value;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{ExportGraphArgs, PaginationArgs};
use sqry_mcp::tool_handlers::execute_export_graph;
use std::collections::HashSet;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the path-resolver discovery cache, engine cache, and the
/// trace-path / subgraph telemetry caches exactly once across the whole
/// test binary. `execute_export_graph` chains through
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

/// Write a Rust crate that produces a syntactic-direct
/// `AlphaMarker::helper` → `AlphaMarker::inner` Calls edge with
/// non-empty qualified names on both endpoints. See module rustdoc for
/// why the C fixture from iter-1 cannot be reused for `export_graph`.
fn write_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "u18_export_graph_resolved_via"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub struct AlphaMarker;
impl AlphaMarker {
    pub fn helper() {
        Self::inner();
    }
    pub fn inner() {}
}
",
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
fn export_graph_response_includes_resolved_via_field_for_calls_edges() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    // Seed on `AlphaMarker::helper` so that the outgoing BFS pulls in
    // the direct call to `AlphaMarker::inner` as a Calls edge with
    // non-empty qualified names on both endpoints.
    let args = ExportGraphArgs {
        file_path: None,
        symbol_name: Some("AlphaMarker::helper".to_string()),
        symbols: Vec::new(),
        path: workspace_arg(&temp),
        format: "json".to_string(),
        max_depth: 2,
        max_results: 100,
        pagination: paging(),
        include_calls: true,
        include_imports: false,
        include_exports: false,
        include_returns: false,
        languages: Vec::new(),
        verbose: false,
    };
    let result = execute_export_graph(&args)?;

    let edges = &result.data.edges;
    assert!(
        !edges.is_empty(),
        "expected at least one edge in the export_graph payload anchored on \
         `AlphaMarker::helper`, got 0"
    );

    // Every emitted Calls-relation edge must carry a `metadata` JSON
    // object that includes the U18 `resolved_via` field. The
    // `export_graph` arm in `classify_edge_for_export` pins the pattern
    // to `ResolvedVia::Direct`, so the only valid wire value here is
    // `"direct"` (snake_case per `ResolvedVia`'s
    // `#[serde(rename_all = "snake_case")]` at
    // `sqry-core/src/graph/unified/edge/kind.rs:60`).
    let mut seen_resolutions: HashSet<String> = HashSet::new();
    let mut calls_edge_count = 0usize;
    for edge in edges {
        if edge.relation_type != "calls" {
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
        assert_eq!(
            resolved_str, "direct",
            "`export_graph` filters its Calls arm on `ResolvedVia::Direct`; the marshaller \
             must therefore emit exactly `\"direct\"` (got {resolved_str:?})"
        );
        seen_resolutions.insert(resolved_str);
    }

    assert!(
        calls_edge_count > 0,
        "expected at least one Calls-relation edge (relation_type=\"calls\") in payload, \
         saw types: {:?}",
        edges.iter().map(|e| &e.relation_type).collect::<Vec<_>>()
    );

    // The fixture exercises `AlphaMarker::helper -> AlphaMarker::inner`,
    // which is the canonical syntactic Direct call. If the `"direct"`
    // wire value disappears the marshaller has regressed.
    assert!(
        seen_resolutions.contains("direct"),
        "expected at least one `resolved_via=\"direct\"` Calls edge \
         (AlphaMarker::helper → AlphaMarker::inner); saw: {seen_resolutions:?}"
    );

    Ok(())
}
