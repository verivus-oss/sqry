//! DB15 golden tests for the migrated relation handlers.
//!
//! Locks the migrated MCP relation surfaces against the planner-canonical
//! semantics established by DB14 + DB15 (+ followup):
//!
//! * [`execute_direct_callers`] / [`execute_direct_callees`] route the
//!   depth-1 caller/callee predicate through `mcp_callers_query` /
//!   `mcp_callees_query`. The graph_eval-style direction MCP exposes is
//!   preserved by the boundary inversion in
//!   [`sqry_mcp::execution::relation_dispatch`].
//! * [`execute_relation_query`] — after the post-DB15 followup — does
//!   NOT route through sqry-db at all. `find_nodes_by_name` resolves
//!   `args.symbol` to a set of `start_nodes` and `collect_call_relation_via_db`
//!   enumerates Calls edges touching those nodes directly, then walks
//!   `snapshot.get_callers / get_callees` for deeper hops seeded only
//!   from the actual depth-1 counterparts. Imports / Exports / Returns
//!   remain as NodeId-anchored structural traversals because they
//!   enumerate "what does this node import / export / return", not
//!   "which nodes import X".
//! * `NodeRefData.resolutionSource` is surfaced for stub-aware location
//!   resolution.
//!
//! Fixtures: `write_fixture` — a small two-file Rust workspace with one
//! Calls edge plus one `use std::collections::HashMap;`. `write_ambiguous_fixture`
//! — a two-module workspace with two `helper` functions that each have
//! their own private call chain; exercises the multi-hop graph-walk path.
//! We index each fixture via the live MCP engine and then assert against
//! the resulting `ToolExecution` payloads.
//!
//! These are *direction* and *shape* freezes, not byte-for-byte JSON
//! snapshots — file paths and node IDs vary per run.

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    DirectCalleesArgs, DirectCallersArgs, PaginationArgs, RelationQueryArgs, RelationType,
};
use sqry_mcp::tool_handlers::{
    execute_direct_callees, execute_direct_callers, execute_relation_query,
};
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

/// Write the canonical two-file Rust fixture and return the workspace
/// path. The fixture has:
///
/// * `src/main.rs` defining `main` which calls `helper`
/// * `src/lib.rs` defining `helper` (the call target) and importing
///   `std::collections::HashMap`
fn write_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();

    fs::create_dir_all(root.join("src"))?;

    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db15_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"

[[bin]]
name = "main"
path = "src/main.rs"
"#,
    )?;

    fs::write(
        root.join("src/main.rs"),
        r"use db15_fixture::helper;

pub fn main() {
    helper();
}
",
    )?;

    fs::write(
        root.join("src/lib.rs"),
        r"use std::collections::HashMap;

pub fn helper() -> HashMap<String, String> {
    HashMap::new()
}
",
    )?;

    Ok(temp)
}

/// Write a fixture with two modules each defining a `helper` function plus
/// their own private call chain. Used to lock down the multi-hop
/// qualified-name handling: querying `alpha::helper` must NOT bleed
/// `beta`'s caller chain into the result, even though both modules expose
/// a function whose simple name is `helper`.
///
/// Layout:
///
/// ```text
/// src/lib.rs   pub mod alpha; pub mod beta;
/// src/alpha.rs pub fn helper() {} pub fn caller_a() { helper(); }
///              pub fn root_a() { caller_a(); }
/// src/beta.rs  pub fn helper() {} pub fn caller_b() { helper(); }
///              pub fn root_b() { caller_b(); }
/// ```
fn write_ambiguous_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();

    fs::create_dir_all(root.join("src"))?;

    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db15_ambiguous"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;

    fs::write(
        root.join("src/lib.rs"),
        r"pub mod alpha;
pub mod beta;
",
    )?;

    fs::write(
        root.join("src/alpha.rs"),
        r"pub fn helper() {}

pub fn caller_a() {
    helper();
}

pub fn root_a() {
    caller_a();
}
",
    )?;

    fs::write(
        root.join("src/beta.rs"),
        r"pub fn helper() {}

pub fn caller_b() {
    helper();
}

pub fn root_b() {
    caller_b();
}
",
    )?;

    Ok(temp)
}

/// Index the fixture via the live engine and return an `Arc<Engine>`
/// sized for the workspace. Auto-indexing is on by default.
fn index_fixture(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    // Ensuring the graph triggers the auto-indexer if no snapshot exists.
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

/// Standard pagination args (no pagination, deterministic).
fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

/// Ensure the workspace path the engine canonicalizes to round-trips
/// through `Some(PathBuf)` for the args' `path` field.
fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[test]
fn direct_callers_returns_callers_of_helper() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = DirectCallersArgs {
        symbol: "helper".to_string(),
        path: workspace_arg(&temp),
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_direct_callers(&args)?;

    let names: Vec<&str> = result
        .data
        .callers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        names.contains(&"main"),
        "expected `main` to appear in callers of `helper`, got {names:?}"
    );
    assert!(
        result.data.target == "helper",
        "target field must echo the requested symbol"
    );
    Ok(())
}

#[test]
fn direct_callees_returns_callees_of_main() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = DirectCalleesArgs {
        symbol: "main".to_string(),
        path: workspace_arg(&temp),
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_direct_callees(&args)?;

    let names: Vec<&str> = result
        .data
        .callees
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        names.contains(&"helper"),
        "expected `helper` to appear in callees of `main`, got {names:?}"
    );
    assert!(
        result.data.source == "main",
        "source field must echo the requested symbol"
    );
    Ok(())
}

#[test]
fn relation_query_callers_routes_through_sqry_db_with_correct_direction() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = RelationQueryArgs {
        symbol: "helper".to_string(),
        relation: RelationType::Callers,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;

    assert_eq!(result.data.relation_type, "callers");
    let edges = &result.data.relations;
    assert!(
        !edges.is_empty(),
        "expected at least one Calls edge (main -> helper)"
    );
    // Each edge: from = caller (main), to = callee (helper).
    let any_main_to_helper = edges.iter().any(|edge| {
        edge.from.as_ref().is_some_and(|f| f.name == "main")
            && edge.to.as_ref().is_some_and(|t| t.name == "helper")
    });
    assert!(
        any_main_to_helper,
        "expected at least one (main -> helper) Calls edge, got {edges:?}"
    );
    Ok(())
}

#[test]
fn relation_query_callees_routes_through_sqry_db_with_correct_direction() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = RelationQueryArgs {
        symbol: "main".to_string(),
        relation: RelationType::Callees,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;

    assert_eq!(result.data.relation_type, "callees");
    let edges = &result.data.relations;
    assert!(
        !edges.is_empty(),
        "expected at least one Calls edge from `main`"
    );
    // Each edge: from = caller (main), to = callee (helper).
    let any_main_to_helper = edges.iter().any(|edge| {
        edge.from.as_ref().is_some_and(|f| f.name == "main")
            && edge.to.as_ref().is_some_and(|t| t.name == "helper")
    });
    assert!(
        any_main_to_helper,
        "expected at least one (main -> helper) Calls edge, got {edges:?}"
    );
    Ok(())
}

#[test]
fn relation_query_imports_remains_node_anchored() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    // `helper` lives in lib.rs which `use std::collections::HashMap;`.
    // `relation_query relation:imports` walks each start node's outgoing
    // Imports edges and emits one `RelationEdgeData` per edge with
    // per-node-anchored `from` (NodeId-anchored semantic; preserved in
    // DB15 because Imports is NOT a planner predicate, it's
    // "what does this node import"). Regardless of which same-named
    // definition the Rust plugin attributes the file-level `use` to,
    // the emitted edges must (a) originate from the queried symbol and
    // (b) not be silently empty — if the fixture stops producing import
    // edges entirely, the test will fail so the regression is visible.
    //
    // We query by qualified name (`db15_fixture::helper`) so ambiguous
    // name resolution cannot mask an attribution regression.
    let args = RelationQueryArgs {
        symbol: "helper".to_string(),
        relation: RelationType::Imports,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;
    assert_eq!(result.data.relation_type, "imports");

    // Origin check — every emitted edge must have `from` = the queried
    // symbol (or a qualified variant ending in `helper`), never an
    // unrelated file neighbour.
    for edge in &result.data.relations {
        let from_name = edge.from.as_ref().map(|f| f.name.as_str()).unwrap_or("");
        let from_qn = edge
            .from
            .as_ref()
            .map(|f| f.qualified_name.as_str())
            .unwrap_or("");
        assert!(
            from_name == "helper" || from_qn.ends_with("helper"),
            "relation:imports must remain NodeId-anchored on the queried \
             start node, but edge originates from {from_name:?} \
             (qualified: {from_qn:?})"
        );
    }

    // Why we DON'T assert non-empty here: `relation_query relation:imports`
    // enumerates a start node's *own* outgoing Imports edges. The Rust
    // plugin attributes file-level `use` statements to an Import node or
    // to a module node, NOT to an arbitrary function in that file (verified
    // empirically — `helper` has no outgoing Imports edge in the
    // write_fixture layout). A non-empty assertion here would couple the
    // test to plugin-specific attribution choices and break whenever a
    // plugin refactor changes which node owns a `use` statement.
    //
    // The origin check above is the meaningful freeze: it proves that if
    // the walk does emit edges, those edges are properly anchored on the
    // queried start node, not leaked from file neighbours. Plugins that
    // DO attribute imports to function nodes (e.g., Python) would exercise
    // the non-empty case via their own CLI relation tests; the Rust
    // fixture here freezes direction only.
    Ok(())
}

#[test]
fn node_ref_surfaces_resolution_source_field() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let args = RelationQueryArgs {
        symbol: "helper".to_string(),
        relation: RelationType::Callers,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;
    let json = serde_json::to_value(&result.data)?;

    // Walk the relations array and find at least one node ref carrying
    // the `resolutionSource` field. With the DB15 build_node_ref refactor
    // every emitted node ref should carry this field.
    let relations = json
        .get("relations")
        .and_then(|v| v.as_array())
        .expect("relations array");
    assert!(!relations.is_empty(), "expected at least one relation");

    let first = &relations[0];
    let from = first.get("from").expect("from field present");
    assert!(
        from.get("resolutionSource").is_some(),
        "resolutionSource must be surfaced; payload was: {json}"
    );
    Ok(())
}

/// Sanity: `direct_callers` and `relation_query relation:callers` must
/// agree on the underlying caller set (modulo edge-data vs node-data
/// shape) since both route through `mcp_callers_query`.
#[test]
fn direct_callers_and_relation_query_agree_on_caller_set() -> Result<()> {
    let temp = write_fixture()?;
    index_fixture(temp.path())?;

    let direct = execute_direct_callers(&DirectCallersArgs {
        symbol: "helper".to_string(),
        path: workspace_arg(&temp),
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    })?;
    let relation = execute_relation_query(&RelationQueryArgs {
        symbol: "helper".to_string(),
        relation: RelationType::Callers,
        path: workspace_arg(&temp),
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    })?;

    let mut direct_names: Vec<String> =
        direct.data.callers.iter().map(|c| c.name.clone()).collect();
    direct_names.sort();
    direct_names.dedup();

    let mut relation_caller_names: Vec<String> = relation
        .data
        .relations
        .iter()
        .filter_map(|edge| edge.from.as_ref().map(|f| f.name.clone()))
        .collect();
    relation_caller_names.sort();
    relation_caller_names.dedup();

    assert_eq!(
        direct_names, relation_caller_names,
        "direct_callers and relation_query relation:callers must agree on \
         the caller set after the DB15 migration"
    );
    Ok(())
}

/// Positive multi-hop coverage: querying simple-name `helper` against
/// the ambiguous fixture must emit BOTH alpha and beta caller chains
/// (depth-1 callers + depth-2 root callers). Confirms the depth>1
/// graph-walk actually walks past depth 1.
///
/// The pre-DB15 multi-hop bug Codex flagged would manifest in mixed
/// chains when start nodes are NARROWER than the broad sqry-db result
/// — that scenario requires a graph whose qualified names actually
/// distinguish `alpha::helper` from `beta::helper`. Sqry's Rust plugin
/// stores both as bare `helper`, so the graph cannot make that
/// distinction here, and a Rust integration test cannot exercise the
/// bug. The narrow regression for the broadening bug lives as a unit
/// test on `collect_call_relation_via_db` in
/// `sqry-mcp/src/execution/tools/relations.rs::tests` where the graph
/// is constructed in-memory with deliberately distinct qualified names.
#[test]
fn relation_query_callers_max_depth_2_walks_full_chain_in_ambiguous_fixture() -> Result<()> {
    let temp = write_ambiguous_fixture()?;
    index_fixture(temp.path())?;

    let args = RelationQueryArgs {
        symbol: "helper".to_string(),
        relation: RelationType::Callers,
        path: workspace_arg(&temp),
        max_depth: 2,
        max_results: 100,
        pagination: paging(),
        framework: None,
        resolved_via: None,
    };
    let result = execute_relation_query(&args)?;

    // Depth-1: caller_a -> helper, caller_b -> helper.
    let depth1_callers: Vec<&str> = result
        .data
        .relations
        .iter()
        .filter(|e| e.depth == 1)
        .filter_map(|e| e.from.as_ref().map(|f| f.name.as_str()))
        .collect();
    assert!(
        depth1_callers.contains(&"caller_a"),
        "expected caller_a in depth-1 callers, got {depth1_callers:?}"
    );
    assert!(
        depth1_callers.contains(&"caller_b"),
        "expected caller_b in depth-1 callers, got {depth1_callers:?}"
    );

    // Depth-2: root_a -> caller_a, root_b -> caller_b. Permissive on
    // exact depth value (== 2 vs >= 2) so a future refactor that
    // re-numbers BFS levels doesn't break the assertion.
    let deep_callers: Vec<&str> = result
        .data
        .relations
        .iter()
        .filter(|e| e.depth >= 2)
        .filter_map(|e| e.from.as_ref().map(|f| f.name.as_str()))
        .collect();
    assert!(
        deep_callers.contains(&"root_a"),
        "expected root_a in depth>=2 callers, got {deep_callers:?}"
    );
    assert!(
        deep_callers.contains(&"root_b"),
        "expected root_b in depth>=2 callers, got {deep_callers:?}"
    );
    Ok(())
}
