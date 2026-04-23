//! DB17 golden tests for the migrated trace / subgraph / export / cycle /
//! complexity handlers.
//!
//! Locks the migrated MCP surfaces against the Phase 3C dispatch taxonomy
//! established by DB14/DB15 + DB16 and extended by DB17:
//!
//! * [`execute_trace_path`] — **NodeId-anchored**. Resolves both
//!   `from_symbol` and `to_symbol` via
//!   `sqry_core::graph::unified::materialize::find_nodes_by_name` and runs
//!   a K-shortest-paths BFS from each `(from, to)` product entry. No
//!   sqry-db dispatch inside the walk, and the visited dedup is keyed on
//!   `NodeId` per path — two unrelated same-named helpers cannot
//!   cross-pollute the frontier.
//! * [`execute_subgraph`] — **NodeId-anchored bidirectional walk**. Seeds
//!   from resolved `NodeId`s; forward/backward BFS uses CSR edge lookups
//!   only; visited set keyed on `(NodeId, depth)`.
//! * [`execute_export_graph`] — **NodeId-anchored from resolved seed set**.
//!   Seeds resolved up front; single-direction BFS over
//!   `snapshot.edges().edges_from(current_id)`. Language / edge-kind
//!   filters apply during traversal.
//! * [`execute_find_cycles`] — **name-keyed predicate**. Routes through
//!   `sqry_db::queries::CyclesQuery` via
//!   [`sqry_mcp::execution::relation_dispatch::make_query_db`]. Honors
//!   `min_depth` / `max_depth` / `include_self_loops` / `max_results` and
//!   returns qualified-name cycle rows.
//! * [`execute_is_node_in_cycle`] — **hybrid**: strict-resolution name to
//!   NodeId, then predicate via `sqry_db::queries::IsInCycleQuery`.
//!   Ambiguous simple names are rejected (DB16 resolution policy). When
//!   true, the containing cycle is fetched via `CyclesQuery` (warm-cache
//!   second query sharing `SccQuery`).
//! * [`execute_complexity_metrics`] — **linear scan** (no sqry-db route
//!   exists today — DB17 scope is migration, not query invention).
//!
//! Fixtures:
//! * `write_two_module_chain_fixture` — two disjoint call chains
//!   (`alpha_root -> alpha_mid -> alpha_deep`, `beta_root -> beta_mid -> beta_deep`),
//!   exercises trace_path / subgraph / export_graph frontier invariants
//!   against **uniquely-named** symbols.
//! * `write_same_simple_name_chain_fixture` — DB17 followup (Codex post-review
//!   Medium 1/2/3). Two independent chains where the inner helper shares
//!   the **same simple name** (`helper`) but distinct qualified names
//!   (`AlphaMarker::helper` / `BetaMarker::helper`, via inherent impls on
//!   distinct marker types). Freezes the DB15-class frontier-broadening
//!   defect: a regression that re-dispatches the BFS frontier by simple
//!   name at depth 1 would leak the beta chain into alpha's trace_path /
//!   subgraph / export_graph results on this fixture and nowhere else.
//!   Same pattern the DB16 followup locked in via
//!   `migration_golden_analysis_test::dependency_impact_same_simple_name_qualified_query_no_frontier_leak`.
//! * `write_cycle_fixture` — a 3-node call cycle plus an acyclic helper,
//!   exercises `find_cycles` + `is_node_in_cycle`.
//! * `write_ambiguous_cycle_fixture` — two modules each with their own
//!   `spin` function; exercises `is_node_in_cycle`'s ambiguous-name
//!   rejection.
//! * `write_complexity_fixture` — a small Rust workspace with one
//!   branchy function and one straight-line function, exercises
//!   `complexity_metrics` filter semantics.
//!
//! These are *direction* and *shape* freezes, not byte-for-byte JSON
//! snapshots — file paths and node IDs vary per run.

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    ComplexityMetricsArgs, CycleType, ExportGraphArgs, FindCyclesArgs, IsNodeInCycleArgs,
    PaginationArgs, SubgraphArgs, TracePathArgs,
};
use sqry_mcp::tool_handlers::{
    execute_complexity_metrics, execute_export_graph, execute_find_cycles,
    execute_is_node_in_cycle, execute_subgraph, execute_trace_path,
};
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

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

fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn index_fixture(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

// ============================================================================
// Fixtures
// ============================================================================

/// Two disjoint call chains in one file. Each symbol name is unique so
/// `find_nodes_by_name` resolves deterministically; the frontier
/// invariant is "walking alpha's chain never leaks into beta's".
///
/// ```text
/// src/lib.rs   pub fn alpha_deep() {}
///              pub fn alpha_mid() { alpha_deep(); }
///              pub fn alpha_root() { alpha_mid(); }
///              pub fn beta_deep() {}
///              pub fn beta_mid() { beta_deep(); }
///              pub fn beta_root() { beta_mid(); }
/// ```
fn write_two_module_chain_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db17_chain_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn alpha_deep() {}

pub fn alpha_mid() {
    alpha_deep();
}

pub fn alpha_root() {
    alpha_mid();
}

pub fn beta_deep() {}

pub fn beta_mid() {
    beta_deep();
}

pub fn beta_root() {
    beta_mid();
}
",
    )?;
    Ok(temp)
}

/// A 3-node call cycle (`node_a -> node_b -> node_c -> node_a`) plus
/// an acyclic helper. Exercises `find_cycles` + `is_node_in_cycle`.
///
/// ```text
/// src/lib.rs   pub fn node_a() { node_b(); }
///              pub fn node_b() { node_c(); }
///              pub fn node_c() { node_a(); }
///              pub fn standalone() {}
/// ```
fn write_cycle_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db17_cycle_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn node_a() {
    node_b();
}

pub fn node_b() {
    node_c();
}

pub fn node_c() {
    node_a();
}

pub fn standalone() {}
",
    )?;
    Ok(temp)
}

/// Two modules each with their own `spin` function in a mutual
/// 2-cycle. Simple-name resolution for `spin` must be rejected by
/// `is_node_in_cycle`'s strict resolver.
///
/// ```text
/// src/lib.rs   pub mod alpha; pub mod beta;
/// src/alpha.rs pub fn spin() { helper(); } pub fn helper() { spin(); }
/// src/beta.rs  pub fn spin() { helper(); } pub fn helper() { spin(); }
/// ```
fn write_ambiguous_cycle_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db17_ambiguous_cycle"
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
        r"pub fn spin() {
    helper();
}

pub fn helper() {
    spin();
}
",
    )?;
    fs::write(
        root.join("src/beta.rs"),
        r"pub fn spin() {
    helper();
}

pub fn helper() {
    spin();
}
",
    )?;
    Ok(temp)
}

/// A branchy function and a straight-line function. Used to verify
/// `complexity_metrics` filter and min_complexity behavior.
///
/// ```text
/// src/lib.rs   pub fn branchy(x: i32) -> i32 { /* 4 branches */ }
///              pub fn straight() -> i32 { 42 }
/// ```
fn write_complexity_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db17_complexity_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn branchy(x: i32) -> i32 {
    if x > 10 {
        if x > 100 {
            1
        } else {
            2
        }
    } else if x > 0 {
        3
    } else if x > -10 {
        4
    } else {
        5
    }
}

pub fn straight() -> i32 {
    42
}
",
    )?;
    Ok(temp)
}

/// DB17 followup (Codex post-review Medium 1/2/3).
///
/// Two disjoint call chains in one file where the *inner* helper shares
/// the same simple name `helper` but has a distinct qualified name via
/// inherent-impl marker types. This is the same fixture shape DB16's
/// followup used in
/// `migration_golden_analysis_test::dependency_impact_same_simple_name_qualified_query_no_frontier_leak`
/// — using `impl AlphaMarker { fn helper() {} }` guarantees the Rust
/// plugin emits the qualified name `AlphaMarker::helper`, which
/// `find_nodes_by_name` resolves to a single NodeId via suffix match.
/// Free functions or functions inside inline `mod foo {}` blocks do
/// not always get their module prefix in the qualified name and would
/// fail to resolve uniquely; inherent impls do.
///
/// Each marker type additionally defines an `inner` method so the
/// intra-chain Calls edge (`helper -> inner`) connects two nodes with
/// *non-empty* qualified names. `export_graph` filters out nodes whose
/// qualified name is empty inside `process_bfs_node_for_export`, and
/// `paginate_export_graph` drops any node that doesn't participate in
/// an edge. Without the inherent-impl receiver on both sides, a walk
/// from `AlphaMarker::helper` would emit zero edges and therefore
/// zero nodes, making the positive-coverage assertion vacuous.
///
/// ```text
/// src/lib.rs   pub struct AlphaMarker;
///              impl AlphaMarker {
///                  pub fn helper() { Self::inner(); }
///                  pub fn inner() {}
///              }
///              pub fn caller_a() { AlphaMarker::helper(); }
///              pub fn root_a() { caller_a(); }
///
///              pub struct BetaMarker;
///              impl BetaMarker {
///                  pub fn helper() { Self::inner(); }
///                  pub fn inner() {}
///              }
///              pub fn caller_b() { BetaMarker::helper(); }
///              pub fn root_b() { caller_b(); }
/// ```
///
/// Frontier invariant: seeding a NodeId-anchored walk on
/// `AlphaMarker::helper` (qualified) must never pull in `caller_b` /
/// `root_b` / `BetaMarker::helper` / `BetaMarker::inner`. A DB15-class
/// regression that re-dispatches the depth-1 frontier by simple name
/// `helper` would leak the entire beta chain into alpha's result.
fn write_same_simple_name_chain_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db17_same_name_frontier"
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
pub fn caller_a() {
    AlphaMarker::helper();
}
pub fn root_a() {
    caller_a();
}

pub struct BetaMarker;
impl BetaMarker {
    pub fn helper() {
        Self::inner();
    }
    pub fn inner() {}
}
pub fn caller_b() {
    BetaMarker::helper();
}
pub fn root_b() {
    caller_b();
}
",
    )?;
    Ok(temp)
}

// ============================================================================
// trace_path — NodeId-anchored; frontier invariant + multi-path freeze
// ============================================================================

#[test]
fn trace_path_finds_direct_chain() -> Result<()> {
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = TracePathArgs {
        from_symbol: "alpha_root".to_string(),
        to_symbol: "alpha_deep".to_string(),
        path: workspace_arg(&temp),
        max_hops: 5,
        max_paths: 5,
        cross_language: false,
        min_confidence: 0.0,
    };
    let result = execute_trace_path(&args)?;

    assert!(
        !result.data.paths.is_empty(),
        "trace_path must find the alpha_root -> alpha_mid -> alpha_deep chain, got empty paths"
    );
    // Shortest path through the chain is 3 nodes, i.e. length=2 hops.
    let min_len = result
        .data
        .paths
        .iter()
        .map(|p| p.steps.len())
        .min()
        .unwrap_or(0);
    assert!(
        min_len >= 3,
        "shortest path should traverse all three chain nodes, got {min_len} steps"
    );
    Ok(())
}

#[test]
fn trace_path_stays_within_anchored_chain() -> Result<()> {
    // Frontier invariant: tracing alpha_root -> alpha_deep must never
    // emit a path that includes any beta_* symbol. The two chains are
    // disjoint in the edge store, so even a buggy name-keyed frontier
    // broadening would leak here if it existed.
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = TracePathArgs {
        from_symbol: "alpha_root".to_string(),
        to_symbol: "alpha_deep".to_string(),
        path: workspace_arg(&temp),
        max_hops: 10,
        max_paths: 20,
        cross_language: false,
        min_confidence: 0.0,
    };
    let result = execute_trace_path(&args)?;

    for path in &result.data.paths {
        for step in &path.steps {
            let name = step.symbol.name.as_str();
            assert!(
                !name.starts_with("beta_"),
                "frontier invariant: alpha_root -> alpha_deep must not contain any beta_* step, got {name:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn trace_path_empty_when_no_chain_exists() -> Result<()> {
    // No edge from alpha_* to beta_*; result must be empty paths.
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = TracePathArgs {
        from_symbol: "alpha_root".to_string(),
        to_symbol: "beta_deep".to_string(),
        path: workspace_arg(&temp),
        max_hops: 10,
        max_paths: 5,
        cross_language: false,
        min_confidence: 0.0,
    };
    let result = execute_trace_path(&args)?;
    assert!(
        result.data.paths.is_empty(),
        "trace_path must return no paths when no chain exists, got {} paths",
        result.data.paths.len()
    );
    Ok(())
}

#[test]
fn trace_path_same_simple_name_qualified_query_no_frontier_leak() -> Result<()> {
    // DB17 followup — Codex post-review Medium 1 freeze.
    //
    // The existing `trace_path_stays_within_anchored_chain` test uses
    // uniquely-named symbols (`alpha_*`, `beta_*`). That leaves the
    // DB15-class bug — simple-name re-dispatch at the BFS frontier —
    // unfrozen for `trace_path`. Use the inherent-impl fixture so the
    // Rust plugin emits `AlphaMarker::helper` / `BetaMarker::helper` as
    // distinct qualified names.
    //
    // Query a path from `root_a` to `AlphaMarker::helper` (qualified).
    // The path exists: `root_a -> caller_a -> AlphaMarker::helper`.
    // A regression that re-broadens the depth-1 frontier by the simple
    // name `helper` would surface spurious paths touching `caller_b`
    // or `root_b`. Assert no such step appears in any returned path.
    let temp = write_same_simple_name_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = TracePathArgs {
        from_symbol: "root_a".to_string(),
        to_symbol: "AlphaMarker::helper".to_string(),
        path: workspace_arg(&temp),
        max_hops: 10,
        max_paths: 20,
        cross_language: false,
        min_confidence: 0.0,
    };
    let result = execute_trace_path(&args)?;

    assert!(
        !result.data.paths.is_empty(),
        "trace_path must find root_a -> caller_a -> AlphaMarker::helper, got empty paths"
    );

    // Positive invariant: `caller_a` must appear somewhere in at least
    // one returned path (this is the real depth-1 counterpart).
    let has_caller_a = result
        .data
        .paths
        .iter()
        .flat_map(|p| p.steps.iter())
        .any(|step| step.symbol.name == "caller_a");
    assert!(
        has_caller_a,
        "at least one path must include caller_a (the direct caller of AlphaMarker::helper), \
         got paths={:?}",
        result
            .data
            .paths
            .iter()
            .map(|p| p
                .steps
                .iter()
                .map(|s| s.symbol.name.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );

    // Frontier invariant: no beta-side node may appear in any path.
    // This is exactly the DB15 same-simple-name frontier broadening
    // bug class, now frozen out for trace_path. Check both the simple
    // `name` field (caller_b / root_b have empty qnames under the
    // current Rust plugin emission) AND the qualified_name field
    // (BetaMarker::helper / BetaMarker::inner).
    for path in &result.data.paths {
        for step in &path.steps {
            let name = step.symbol.name.as_str();
            let qname = step.symbol.qualified_name.as_str();
            assert!(
                !matches!(name, "caller_b" | "root_b") && !qname.starts_with("BetaMarker"),
                "frontier invariant: trace_path root_a -> AlphaMarker::helper must not \
                 leak any beta-side node — DB15 same-simple-name regression freeze, \
                 got name={name:?} qname={qname:?}"
            );
        }
    }
    Ok(())
}

// ============================================================================
// subgraph — NodeId-anchored; bidirectional frontier invariant
// ============================================================================

#[test]
fn subgraph_extracts_bidirectional_neighbourhood() -> Result<()> {
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = SubgraphArgs {
        symbols: vec!["alpha_mid".to_string()],
        path: workspace_arg(&temp),
        max_depth: 3,
        max_nodes: 100,
        include_callers: true,
        include_callees: true,
        include_imports: false,
        cross_language: false,
        pagination: paging(),
    };
    let result = execute_subgraph(&args)?;

    let node_names: Vec<&str> = result.data.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        node_names.contains(&"alpha_mid"),
        "seed alpha_mid must appear in the subgraph nodes, got {node_names:?}"
    );
    assert!(
        node_names.contains(&"alpha_deep"),
        "forward walk must reach alpha_deep (alpha_mid calls it), got {node_names:?}"
    );
    assert!(
        node_names.contains(&"alpha_root"),
        "backward walk must reach alpha_root (alpha_root calls alpha_mid), got {node_names:?}"
    );
    Ok(())
}

#[test]
fn subgraph_stays_anchored_to_resolved_seed() -> Result<()> {
    // Frontier invariant: seeding on alpha_mid must NEVER produce any
    // beta_* node. The two chains share no edge in either direction.
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = SubgraphArgs {
        symbols: vec!["alpha_mid".to_string()],
        path: workspace_arg(&temp),
        max_depth: 5,
        max_nodes: 100,
        include_callers: true,
        include_callees: true,
        include_imports: false,
        cross_language: false,
        pagination: paging(),
    };
    let result = execute_subgraph(&args)?;
    for node in &result.data.nodes {
        let name = node.name.as_str();
        assert!(
            !name.starts_with("beta_"),
            "frontier invariant: alpha_mid subgraph must not contain beta_* node, got {name:?}"
        );
    }
    Ok(())
}

#[test]
fn subgraph_same_simple_name_qualified_query_no_frontier_leak() -> Result<()> {
    // DB17 followup — Codex post-review Medium 2 freeze.
    //
    // The existing `subgraph_stays_anchored_to_resolved_seed` test uses
    // uniquely-named symbols. Re-check the invariant against the same
    // same-simple-name pattern the DB16 followup used for
    // `dependency_impact` / `show_dependencies`: seed a subgraph walk
    // on `AlphaMarker::helper` (qualified). A DB15-class regression
    // that re-broadens the BFS frontier by simple name `helper` at
    // depth 1 would pull in `caller_b` / `root_b` / `BetaMarker::helper`.
    let temp = write_same_simple_name_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = SubgraphArgs {
        symbols: vec!["AlphaMarker::helper".to_string()],
        path: workspace_arg(&temp),
        max_depth: 5,
        max_nodes: 100,
        include_callers: true,
        include_callees: true,
        include_imports: false,
        cross_language: false,
        pagination: paging(),
    };
    let result = execute_subgraph(&args)?;

    let node_names: Vec<&str> = result.data.nodes.iter().map(|n| n.name.as_str()).collect();
    let node_qnames: Vec<&str> = result
        .data
        .nodes
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();

    // Positive invariant: the seed's forward walk must reach
    // `AlphaMarker::inner` (AlphaMarker::helper calls AlphaMarker::inner
    // in the fixture). The backward walk should also reach `caller_a`
    // (the direct caller of AlphaMarker::helper). If neither appears,
    // the resolution / walk is broken and the negative invariant below
    // is vacuous.
    let has_alpha_expansion =
        node_qnames.iter().any(|q| q == &"AlphaMarker::inner") || node_names.contains(&"caller_a");
    assert!(
        has_alpha_expansion,
        "subgraph walk from AlphaMarker::helper must reach AlphaMarker::inner (forward) \
         or caller_a (backward), got names={node_names:?} qnames={node_qnames:?}"
    );

    // Frontier invariant: no beta-side node may appear.
    for node in &result.data.nodes {
        let name = node.name.as_str();
        let qname = node.qualified_name.as_str();
        assert!(
            !matches!(name, "caller_b" | "root_b") && !qname.starts_with("BetaMarker"),
            "frontier invariant: AlphaMarker::helper subgraph must not leak any beta-side node — \
             DB15 same-simple-name regression freeze, got name={name:?} qname={qname:?}"
        );
    }
    Ok(())
}

// ============================================================================
// export_graph — whole-graph BFS from seeds; file-filter sanity
// ============================================================================

#[test]
fn export_graph_runs_and_respects_seed_resolution() -> Result<()> {
    // Smoke test: on the unique-name two-chain fixture, export_graph
    // resolves an `alpha_root` seed without error and the handler runs
    // end-to-end. The emitted payload may be empty on this fixture —
    // crate-root free functions emit empty qualified names under the
    // current Rust plugin, and `process_bfs_node_for_export` +
    // `paginate_export_graph` filter those out. The non-vacuous
    // positive + negative coverage is in the companion test
    // `export_graph_same_simple_name_qualified_query_no_frontier_leak`
    // below, which uses the inherent-impl marker fixture where
    // qualified names survive the filter.
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = ExportGraphArgs {
        file_path: None,
        symbol_name: Some("alpha_root".to_string()),
        symbols: Vec::new(),
        path: workspace_arg(&temp),
        format: "json".to_string(),
        max_depth: 5,
        max_results: 200,
        pagination: paging(),
        include_calls: true,
        include_imports: false,
        include_exports: false,
        include_returns: false,
        languages: Vec::new(),
        verbose: false,
    };
    // Handler must run end-to-end without bailing on seed resolution.
    let _ = execute_export_graph(&args)?;
    Ok(())
}

#[test]
fn export_graph_same_simple_name_qualified_query_no_frontier_leak() -> Result<()> {
    // DB17 followup — Codex post-review Medium 2/3 freeze.
    //
    // The previous `export_graph_runs_and_respects_seed_resolution`
    // test only asserted `!beta_leak` and could pass vacuously on
    // empty output (Medium 2). This test replaces the vacuous freeze
    // with non-vacuous positive + negative coverage using the
    // inherent-impl marker fixture so the Rust plugin emits the
    // qualified names `AlphaMarker::helper` / `AlphaMarker::inner` /
    // `BetaMarker::helper` / `BetaMarker::inner` — all of which
    // survive the qualified-name guard inside
    // `process_bfs_node_for_export` and the edge-endpoint filter
    // inside `paginate_export_graph`. A DB15-class regression that
    // re-broadens the depth-1 frontier by simple name `helper` would
    // leak `BetaMarker::helper` / `BetaMarker::inner` into the export
    // output.
    let temp = write_same_simple_name_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = ExportGraphArgs {
        file_path: None,
        symbol_name: Some("AlphaMarker::helper".to_string()),
        symbols: Vec::new(),
        path: workspace_arg(&temp),
        format: "json".to_string(),
        max_depth: 5,
        max_results: 200,
        pagination: paging(),
        include_calls: true,
        include_imports: false,
        include_exports: false,
        include_returns: false,
        languages: Vec::new(),
        verbose: false,
    };
    let result = execute_export_graph(&args)?;

    let node_qnames: Vec<&str> = result
        .data
        .nodes
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    let edge_qname_endpoints: Vec<(&str, &str)> = result
        .data
        .edges
        .iter()
        .map(|e| {
            (
                e.from
                    .as_ref()
                    .map(|n| n.qualified_name.as_str())
                    .unwrap_or(""),
                e.to.as_ref()
                    .map(|n| n.qualified_name.as_str())
                    .unwrap_or(""),
            )
        })
        .collect();

    // Positive invariant (Medium 2 fix): the resolved seed must
    // materialize *something*. `AlphaMarker::helper` calls
    // `AlphaMarker::inner` — both have non-empty qualified names, so
    // the `process_bfs_node_for_export` qualified-name guard and the
    // `paginate_export_graph` edge-endpoint filter both let these
    // through. At least one alpha-side node or edge endpoint must
    // appear. Accept either qualified-name ("AlphaMarker::…") or any
    // `caller_a` / `root_a` if the plugin ever starts emitting
    // non-empty qnames for crate-root free functions.
    let has_alpha_presence = node_qnames.iter().any(|n| n.starts_with("AlphaMarker"))
        || edge_qname_endpoints
            .iter()
            .any(|(from, to)| from.starts_with("AlphaMarker") || to.starts_with("AlphaMarker"));
    assert!(
        has_alpha_presence,
        "export_graph must emit at least one AlphaMarker-side node or edge for \
         seed=AlphaMarker::helper (non-vacuous positive coverage — Medium 2 \
         freeze), got nodes={node_qnames:?} edges={edge_qname_endpoints:?}"
    );

    // Frontier invariant: no beta-side node or edge endpoint may
    // appear under an alpha-side seed. A DB15-class frontier
    // regression that re-dispatches by simple name `helper` at depth
    // 1 would pull `BetaMarker::helper` / `BetaMarker::inner` into
    // the export.
    let beta_leak = node_qnames.iter().any(|n| n.starts_with("BetaMarker"))
        || edge_qname_endpoints
            .iter()
            .any(|(from, to)| from.starts_with("BetaMarker") || to.starts_with("BetaMarker"));
    assert!(
        !beta_leak,
        "frontier invariant: no BetaMarker-side node/edge may appear under \
         seed=AlphaMarker::helper — DB15 same-simple-name regression freeze, \
         got nodes={node_qnames:?} edges={edge_qname_endpoints:?}"
    );
    // Also guard against any beta simple-name leak in case emission
    // ever changes to populate crate-root function qnames.
    let beta_simple_leak = result
        .data
        .nodes
        .iter()
        .any(|n| matches!(n.name.as_str(), "caller_b" | "root_b"))
        || result.data.edges.iter().any(|e| {
            e.from
                .as_ref()
                .is_some_and(|n| matches!(n.name.as_str(), "caller_b" | "root_b"))
                || e.to
                    .as_ref()
                    .is_some_and(|n| matches!(n.name.as_str(), "caller_b" | "root_b"))
        });
    assert!(
        !beta_simple_leak,
        "frontier invariant: caller_b / root_b must not appear under \
         seed=AlphaMarker::helper — DB15 same-simple-name regression freeze"
    );
    Ok(())
}

#[test]
fn export_graph_rejects_unknown_symbol_seed() -> Result<()> {
    // export_graph bails when no seeds resolve. Locks the failure mode
    // for the DB17 migration: the handler is NodeId-anchored from a
    // resolved seed set; if `find_nodes_by_name` returns empty for
    // every requested symbol AND no file_path is supplied, the
    // operation must error rather than silently return an empty graph.
    let temp = write_two_module_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = ExportGraphArgs {
        file_path: None,
        symbol_name: Some("definitely_not_a_real_symbol_xyz".to_string()),
        symbols: Vec::new(),
        path: workspace_arg(&temp),
        format: "json".to_string(),
        max_depth: 5,
        max_results: 200,
        pagination: paging(),
        include_calls: true,
        include_imports: false,
        include_exports: false,
        include_returns: false,
        languages: Vec::new(),
        verbose: false,
    };
    let err = execute_export_graph(&args)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("No seed symbols found") || err.contains("not found"),
        "export_graph must reject unresolvable seeds, got: {err:?}"
    );
    Ok(())
}

// ============================================================================
// find_cycles — name-keyed predicate via sqry-db CyclesQuery
// ============================================================================

#[test]
fn find_cycles_detects_three_node_call_cycle() -> Result<()> {
    let temp = write_cycle_fixture()?;
    index_fixture(temp.path())?;

    let args = FindCyclesArgs {
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 2,
        max_depth: None,
        include_self_loops: false,
        max_results: 10,
        pagination: paging(),
    };
    let result = execute_find_cycles(&args)?;

    assert_eq!(result.data.cycle_type, "calls");
    assert!(
        result.data.total >= 1,
        "expected at least one cycle, got {}",
        result.data.total
    );
    let has_3_cycle = result.data.cycles.iter().any(|cycle| {
        let names: Vec<&str> = cycle.nodes.iter().map(|n| n.name.as_str()).collect();
        cycle.depth == 3
            && names.contains(&"node_a")
            && names.contains(&"node_b")
            && names.contains(&"node_c")
    });
    assert!(
        has_3_cycle,
        "expected a 3-node cycle covering node_a/b/c, got cycles={:?}",
        result.data.cycles
    );
    Ok(())
}

#[test]
fn find_cycles_min_depth_filter_hides_small_cycles() -> Result<()> {
    let temp = write_cycle_fixture()?;
    index_fixture(temp.path())?;

    let args = FindCyclesArgs {
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 4, // our 3-node cycle is below this
        max_depth: None,
        include_self_loops: false,
        max_results: 10,
        pagination: paging(),
    };
    let result = execute_find_cycles(&args)?;
    assert_eq!(
        result.data.total, 0,
        "min_depth=4 must filter out the 3-node cycle, got {:?}",
        result.data.cycles
    );
    Ok(())
}

#[test]
fn find_cycles_respects_max_results_cap() -> Result<()> {
    let temp = write_cycle_fixture()?;
    index_fixture(temp.path())?;

    let args = FindCyclesArgs {
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 2,
        max_depth: None,
        include_self_loops: false,
        max_results: 0, // explicit zero cap
        pagination: paging(),
    };
    let result = execute_find_cycles(&args)?;
    assert_eq!(
        result.data.total, 0,
        "max_results=0 must suppress all cycles, got {:?}",
        result.data.cycles
    );
    Ok(())
}

// ============================================================================
// is_node_in_cycle — hybrid; strict name resolution + sqry-db predicate
// ============================================================================

#[test]
fn is_node_in_cycle_reports_true_for_cycle_member() -> Result<()> {
    let temp = write_cycle_fixture()?;
    index_fixture(temp.path())?;

    let args = IsNodeInCycleArgs {
        symbol: "node_b".to_string(),
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 2,
        max_depth: None,
        include_self_loops: false,
        file_path: None,
    };
    let result = execute_is_node_in_cycle(&args)?;
    assert!(
        result.data.in_cycle,
        "node_b participates in a 3-node call cycle, got in_cycle={}",
        result.data.in_cycle
    );
    assert_eq!(result.data.cycle_type, "calls");
    let cycle_names = result.data.cycle.clone().unwrap_or_default();
    assert!(
        cycle_names.iter().any(|n| n.contains("node_a"))
            && cycle_names.iter().any(|n| n.contains("node_c")),
        "containing cycle must include node_a + node_c, got {cycle_names:?}"
    );
    Ok(())
}

#[test]
fn is_node_in_cycle_reports_false_for_acyclic_symbol() -> Result<()> {
    let temp = write_cycle_fixture()?;
    index_fixture(temp.path())?;

    let args = IsNodeInCycleArgs {
        symbol: "standalone".to_string(),
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 2,
        max_depth: None,
        include_self_loops: false,
        file_path: None,
    };
    let result = execute_is_node_in_cycle(&args)?;
    assert!(
        !result.data.in_cycle,
        "standalone has no outgoing/incoming calls, must not be in a cycle"
    );
    assert!(
        result.data.cycle.is_none(),
        "cycle field must be None when in_cycle=false, got {:?}",
        result.data.cycle
    );
    Ok(())
}

#[test]
fn is_node_in_cycle_rejects_ambiguous_simple_name() -> Result<()> {
    let temp = write_ambiguous_cycle_fixture()?;
    index_fixture(temp.path())?;

    // `spin` is defined in both alpha.rs and beta.rs. Strict resolution
    // must reject this request rather than arbitrarily pick one node.
    let args = IsNodeInCycleArgs {
        symbol: "spin".to_string(),
        path: workspace_arg(&temp),
        cycle_type: CycleType::Calls,
        min_depth: 2,
        max_depth: None,
        include_self_loops: false,
        file_path: None,
    };
    let err = execute_is_node_in_cycle(&args)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("ambiguous") || err.contains("canonical qualified name"),
        "ambiguous-name rejection must hint at the canonical-name workaround, got: {err:?}"
    );
    Ok(())
}

// ============================================================================
// complexity_metrics — linear scan; min_complexity + target filter
// ============================================================================

#[test]
fn complexity_metrics_returns_per_symbol_rows() -> Result<()> {
    let temp = write_complexity_fixture()?;
    index_fixture(temp.path())?;

    let args = ComplexityMetricsArgs {
        path: workspace_arg(&temp),
        target: None,
        min_complexity: 0,
        sort_by_complexity: true,
        max_results: 100,
    };
    let result = execute_complexity_metrics(&args)?;

    assert!(
        result.data.total > 0,
        "complexity_metrics must return at least one row on a non-empty workspace, got {}",
        result.data.total
    );
    if result.data.max_complexity > 0 {
        assert!(
            result.data.average_complexity <= f64::from(result.data.max_complexity),
            "average complexity ({}) must not exceed max ({}) — sanity check",
            result.data.average_complexity,
            result.data.max_complexity
        );
    }
    Ok(())
}

#[test]
fn complexity_metrics_min_complexity_filter_narrows_set() -> Result<()> {
    let temp = write_complexity_fixture()?;
    index_fixture(temp.path())?;

    let args_all = ComplexityMetricsArgs {
        path: workspace_arg(&temp),
        target: None,
        min_complexity: 0,
        sort_by_complexity: true,
        max_results: 100,
    };
    let baseline = execute_complexity_metrics(&args_all)?;

    let args_filtered = ComplexityMetricsArgs {
        path: workspace_arg(&temp),
        target: None,
        min_complexity: 2, // excludes the straight-line function
        sort_by_complexity: true,
        max_results: 100,
    };
    let filtered = execute_complexity_metrics(&args_filtered)?;

    assert!(
        filtered.data.total <= baseline.data.total,
        "min_complexity=2 filter must not expand the result set ({} vs {})",
        filtered.data.total,
        baseline.data.total
    );
    for metric in &filtered.data.metrics {
        assert!(
            metric.complexity >= 2,
            "min_complexity=2 filter must exclude complexity<2 rows, got {}",
            metric.complexity
        );
    }
    Ok(())
}
