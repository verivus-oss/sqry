//! DB16 golden tests for the migrated analysis handlers.
//!
//! Locks the migrated MCP surfaces against the Phase 3C dispatch taxonomy
//! established by DB14/DB15 and extended by DB16:
//!
//! * [`execute_find_unused`] — **name-keyed predicate**. Routes through
//!   `sqry_db::queries::UnusedQuery` via
//!   [`sqry_mcp::execution::relation_dispatch::make_query_db`]. Ambiguous
//!   / broad scopes (e.g., `UnusedScope::All`) return the union of every
//!   unused node the planner enumerates, with MCP's `languages` / `kinds`
//!   / scope post-filter applied on the way out.
//! * [`execute_dependency_impact`] — **NodeId-anchored**. Resolves the
//!   user-supplied symbol via `resolve_global_symbol_strict` (ambiguous
//!   names are rejected with a canonical-name hint) and BFS-walks
//!   `snapshot.get_callers(current_id)` from that single seed. No sqry-db
//!   dispatch inside the BFS loop — the multi-hop frontier-broadening bug
//!   DB15's followup fixed for `relation_query` cannot manifest here.
//! * [`execute_get_dependencies`] — **NodeId-anchored**. Seeds come from
//!   `file_path` OR `symbol_name` (resolved via `find_nodes_by_name`),
//!   and BFS-walks `snapshot.edges().edges_from(current)` /
//!   `edges_to(current)` in both directions. An ambiguous simple name
//!   expands to a union of per-seed walks; the walks never bleed into
//!   each other because the visited dedup is keyed on
//!   `(NodeId, depth)` and expansion uses direct CSR edge lookups only.
//!
//! Fixtures:
//! * `write_unused_fixture` — a small Rust workspace with `main` calling
//!   `used_helper`, plus an unreachable `unused_helper` and an
//!   unreachable `unused_struct`. Exercises the name-keyed predicate
//!   semantic of `find_unused`.
//! * `write_ambiguous_chain_fixture` — two independent modules
//!   `alpha`/`beta`, each with its own `helper()` function and private
//!   call chain. Exercises the NodeId-anchored frontier invariant of
//!   `dependency_impact` and `show_dependencies` — querying one module's
//!   `helper` must not pull in the other module's chain.
//!
//! These are *direction* and *shape* freezes, not byte-for-byte JSON
//! snapshots — file paths and node IDs vary per run.

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    DependencyImpactArgs, FindUnusedArgs, PaginationArgs, ShowDependenciesArgs, UnusedScope,
};
use sqry_mcp::tool_handlers::{
    execute_dependency_impact, execute_find_unused, execute_get_dependencies,
};
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
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

/// Fixture for `find_unused`:
///
/// ```text
/// src/main.rs  pub fn main() { used_helper(); }
/// src/lib.rs   pub fn used_helper() {}
///              fn unused_helper() {}
///              pub struct UnusedStruct;         (public → entry point)
///              struct UnusedPrivateStruct;      (private, unreachable)
///              pub(crate) struct UsedStruct;    (referenced → reachable)
///              pub fn consume(_: UsedStruct) {} (uses `UsedStruct`)
/// ```
fn write_unused_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_unused_fixture"
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
        r"use db16_unused_fixture::used_helper;

pub fn main() {
    used_helper();
}
",
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn used_helper() {}

fn unused_helper() {}

pub struct UnusedStruct;

// Private + unreachable → MUST appear under MCP `UnusedScope::Struct`
// (Codex finding #4: the prior fixture's only struct was public and
// therefore an entry point, so the struct-filter test was vacuous).
struct UnusedPrivateStruct;

// Referenced by the public `consume` signature → reachable → MUST NOT
// appear as unused.
pub(crate) struct UsedStruct;

pub fn consume(_: UsedStruct) {}
",
    )?;
    Ok(temp)
}

/// Fixture for the `UnusedScope::Struct` parity freeze:
/// exercises the Codex finding #1 surface by declaring private
/// structs, classes, traits, and interfaces that must all survive
/// MCP's `Struct` scope filter. `class` / `interface` don't exist in
/// Rust source syntax; instead we rely on the MCP handler treating
/// [`sqry_core::graph::unified::node::NodeKind::Trait`] as a
/// `Struct`-scope match. This freezes the contract: a private unused
/// trait MUST appear in `UnusedScope::Struct` results.
fn write_struct_scope_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_struct_scope"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"// Private unused struct — under the pre-DB16 contract this was
// always reported, and the DB16 pass-through of MCP `Struct` to
// sqry-db `Struct` happened to preserve it.
struct UnusedPrivateStruct;

// Private unused trait — under the pre-DB16 contract this WAS
// reported because MCP's `matches_scope_filter` includes `Trait` in
// the `Struct` scope. The DB16 pass-through silently dropped it
// because sqry-db's `Struct` only covers `Struct | Class`. This
// test freezes the fix.
trait UnusedPrivateTrait {
    fn method(&self);
}

// Public function, used by main, acts as an entry point so
// `main` itself isn't the sole entry-point holding reachability up.
pub fn main() {
    let _ = 0;
}
",
    )?;
    Ok(temp)
}

/// Fixture for the NodeId-anchored frontier invariant on
/// `dependency_impact` / `show_dependencies`:
///
/// ```text
/// src/lib.rs   pub mod alpha; pub mod beta;
/// src/alpha.rs pub fn helper() {}
///              pub fn caller_a() { helper(); }
///              pub fn root_a() { caller_a(); }
/// src/beta.rs  pub fn helper() {}
///              pub fn caller_b() { helper(); }
///              pub fn root_b() { caller_b(); }
/// ```
fn write_ambiguous_chain_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_ambiguous"
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

// ============================================================================
// find_unused — planner-canonical name-keyed predicate
// ============================================================================

#[test]
fn find_unused_returns_unreachable_symbols_via_sqry_db() -> Result<()> {
    let temp = write_unused_fixture()?;
    index_fixture(temp.path())?;

    let args = FindUnusedArgs {
        path: workspace_arg(&temp),
        scope: UnusedScope::All,
        languages: Vec::new(),
        kinds: Vec::new(),
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_find_unused(&args)?;

    assert_eq!(result.data.scope, "all");

    // The planner-canonical semantic: anything not reachable from entry
    // points (main, pub, test, export, Test/Export kinds) is included,
    // subject to scope + kind + language filters. This freezes the
    // MUST-appear set (the test fixtures whose unreachability is stable
    // across Rust plugin refactors); the MUST-NOT-appear set for `main`
    // is covered by the entry-point detection in
    // `sqry_db::queries::EntryPointsQuery` and its dedicated unit tests.
    let names: Vec<&str> = result
        .data
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        !names.contains(&"main"),
        "`main` is an entry point and must NOT be reported as unused, got {names:?}"
    );
    assert!(
        names.contains(&"unused_helper"),
        "`unused_helper` is private + unreachable — must be reported, got {names:?}"
    );
    Ok(())
}

#[test]
fn find_unused_scope_filter_narrows_to_functions_only() -> Result<()> {
    let temp = write_unused_fixture()?;
    index_fixture(temp.path())?;

    let args = FindUnusedArgs {
        path: workspace_arg(&temp),
        scope: UnusedScope::Function,
        languages: Vec::new(),
        kinds: Vec::new(),
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_find_unused(&args)?;
    assert_eq!(result.data.scope, "function");

    let kinds: Vec<&str> = result
        .data
        .symbols
        .iter()
        .map(|s| s.kind.as_str())
        .collect();
    for kind in &kinds {
        assert!(
            *kind == "function" || *kind == "method",
            "UnusedScope::Function must narrow to functions/methods, got {kind:?} in {kinds:?}"
        );
    }
    Ok(())
}

#[test]
fn find_unused_kind_filter_preserves_mcp_api_surface() -> Result<()> {
    // Codex finding #4: the prior fixture's only struct was public
    // (`pub struct UnusedStruct;`), which makes it an entry point
    // under [`sqry_db::queries::EntryPointsQuery`] and therefore
    // never unused. The old assertion `symbol.kind == "struct"` was
    // vacuously satisfied by an empty result set. The fixture now
    // also contains a private `UnusedPrivateStruct`, so the kinds
    // filter has something to include — assert non-empty AND that
    // every returned row is a struct.
    let temp = write_unused_fixture()?;
    index_fixture(temp.path())?;

    // MCP-specific `kinds` filter — not something sqry-db's `UnusedScope`
    // expresses directly. The post-filter in the handler must honour it.
    let args = FindUnusedArgs {
        path: workspace_arg(&temp),
        scope: UnusedScope::All,
        languages: Vec::new(),
        kinds: vec!["struct".to_string()],
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_find_unused(&args)?;
    let names: Vec<&str> = result
        .data
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        !result.data.symbols.is_empty(),
        "non-vacuous freeze: `UnusedPrivateStruct` is private + unreachable + a struct, \
         so the kinds-filtered result set must be non-empty. names={names:?}"
    );
    assert!(
        names.contains(&"UnusedPrivateStruct"),
        "kinds=['struct'] filter must include the private unused struct, got {names:?}"
    );
    assert!(
        !names.contains(&"UsedStruct"),
        "`UsedStruct` is referenced by `consume` → reachable → MUST NOT appear, \
         got {names:?}"
    );
    for symbol in &result.data.symbols {
        assert_eq!(
            symbol.kind, "struct",
            "kinds filter must narrow to struct only, got {:?}",
            symbol.kind
        );
    }
    Ok(())
}

#[test]
fn find_unused_struct_scope_includes_traits_and_interfaces() -> Result<()> {
    // Codex blocker finding #1: MCP's `UnusedScope::Struct` covers
    // `Struct | Class | Interface | Trait`, but DB16 initially
    // passed the scope straight through to sqry-db whose `Struct`
    // only matches `Struct | Class`. Unused traits / interfaces
    // silently disappeared.
    //
    // After the followup, sqry-db is dispatched with the superset
    // `UnusedScope::All` and the MCP post-filter narrows to the full
    // `Struct | Class | Interface | Trait` set. Freeze that contract
    // with a fixture containing one private unused struct and one
    // private unused trait; both MUST appear.
    let temp = write_struct_scope_fixture()?;
    index_fixture(temp.path())?;

    let args = FindUnusedArgs {
        path: workspace_arg(&temp),
        scope: UnusedScope::Struct,
        languages: Vec::new(),
        kinds: Vec::new(),
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_find_unused(&args)?;
    let names: Vec<&str> = result
        .data
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();

    assert!(
        names.contains(&"UnusedPrivateStruct"),
        "UnusedScope::Struct must include private unused struct, got {names:?}"
    );
    assert!(
        names.contains(&"UnusedPrivateTrait"),
        "UnusedScope::Struct must include private unused TRAIT (regression freeze for \
         Codex blocker finding #1 — DB16 dropped this), got {names:?}"
    );
    // Every result MUST be one of the four Struct-scope kinds.
    for symbol in &result.data.symbols {
        assert!(
            matches!(
                symbol.kind.as_str(),
                "struct" | "class" | "interface" | "trait"
            ),
            "UnusedScope::Struct must only return struct|class|interface|trait, got {:?}",
            symbol.kind
        );
    }
    Ok(())
}

#[test]
fn find_unused_post_filter_completeness_no_early_truncation() -> Result<()> {
    // Codex finding #2: the prior handler asked sqry-db for
    // `16 × max_results` raw candidates and let sqry-db truncate
    // *before* the MCP language / kind / stricter-Private filters
    // ran. When the first 16 × max_results raw candidates were
    // mostly rejected MCP-side, valid later matches silently fell
    // off the window.
    //
    // The fix: when the MCP post-filter may narrow further, sqry-db
    // is asked for the full pool (`max_results = node_count`). This
    // test freezes that behavior with a fixture whose unused-struct
    // count is larger than any plausible `args.max_results × 16`
    // window the handler might compute internally. Even with
    // `max_results = 1`, MCP post-filter on `kinds=['struct']` must
    // find the sole struct match, even if it sorts after many
    // filtered-out function rows in sqry-db's traversal order.
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_completeness"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    // Emit 40 private unused functions followed by a private unused
    // struct and a private unused trait. Under MCP
    // `UnusedScope::Struct` + `kinds=["struct"]`, every function is
    // post-filter-rejected; the late struct MUST survive.
    //
    // The old cap heuristic (pre-fix) asked sqry-db for only
    // `16 × max_results` raw candidates and let sqry-db truncate
    // before MCP's `kinds` filter ran. With `max_results = 1` the
    // window was 16 candidates; any struct whose NodeId sorted after
    // the 16th function would silently drop off. The fix passes
    // `node_count` to sqry-db whenever MCP may narrow, so the late
    // struct survives.
    let mut body = String::new();
    for i in 0..40 {
        body.push_str(&format!("fn noise{i}() {{}}\n"));
    }
    body.push_str("struct LateStruct;\n");
    body.push_str("trait LateTrait {}\n");
    body.push_str("pub fn main() {}\n");
    fs::write(root.join("src/lib.rs"), body)?;
    index_fixture(temp.path())?;

    let args = FindUnusedArgs {
        path: workspace_arg(&temp),
        scope: UnusedScope::Struct,
        languages: Vec::new(),
        kinds: vec!["struct".to_string()],
        // `max_results = 1` forces the issue: if sqry-db caps before
        // MCP filters, and the struct sorts after the first 16
        // functions in traversal order, this assertion fails. The
        // fix passes `node_count` to sqry-db whenever MCP may narrow.
        max_results: 1,
        pagination: paging(),
    };
    let result = execute_find_unused(&args)?;
    let names: Vec<&str> = result
        .data
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        names.contains(&"LateStruct"),
        "LateStruct must survive the MCP post-filter even with max_results=1 and 40 \
         preceding filtered-out functions (Codex finding #2 regression freeze), got {names:?}"
    );
    Ok(())
}

// ============================================================================
// dependency_impact — NodeId-anchored; ambiguous frontier invariant
// ============================================================================

#[test]
fn dependency_impact_stays_anchored_to_resolved_start_node() -> Result<()> {
    // The `write_ambiguous_chain_fixture` is tuned for multi-hop
    // relation_query, but every symbol in it either (a) shares a simple
    // name across modules (`helper`) or (b) has a call-site stub under
    // the same simple name (`caller_a`/`caller_b`), so
    // `resolve_global_symbol_strict` rejects them. Instead use a custom
    // fixture with module-unique symbol names plus a disjoint second
    // chain, to exercise both the resolver and the frontier invariant:
    //
    //   src/lib.rs   pub fn alpha_helper() {}
    //                pub fn alpha_caller() { alpha_helper(); }
    //                pub fn alpha_root()   { alpha_caller(); }
    //                pub fn beta_helper()  {}
    //                pub fn beta_caller()  { beta_helper(); }
    //                pub fn beta_root()    { beta_caller(); }
    //
    // Querying `alpha_helper` resolves strictly to one node; the BFS
    // must walk to `alpha_caller` (depth 1) and `alpha_root` (depth 2)
    // and must NOT reach any beta_* symbol.
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_frontier_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn alpha_helper() {}

pub fn alpha_caller() {
    alpha_helper();
}

pub fn alpha_root() {
    alpha_caller();
}

pub fn beta_helper() {}

pub fn beta_caller() {
    beta_helper();
}

pub fn beta_root() {
    beta_caller();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "alpha_helper".to_string(),
        path: workspace_arg(&temp),
        max_depth: 5,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };
    let result = execute_dependency_impact(&args)?;

    let names: Vec<&str> = result
        .data
        .impacted_symbols
        .iter()
        .map(|s| s.symbol.name.as_str())
        .collect();

    assert!(
        names.contains(&"alpha_caller"),
        "expected alpha_caller (direct caller of alpha_helper) in impact chain, got {names:?}"
    );
    // The NodeId-anchored BFS must NOT leak beta_* into alpha's impact
    // chain — there is no Calls edge between the two disjoint chains.
    assert!(
        !names.contains(&"beta_caller"),
        "beta_caller must NOT leak into alpha's impact result (frontier invariant), got {names:?}"
    );
    assert!(
        !names.contains(&"beta_helper"),
        "beta_helper must NOT leak into alpha's impact result (frontier invariant), got {names:?}"
    );
    assert!(
        !names.contains(&"beta_root"),
        "beta_root must NOT leak into alpha's impact result (frontier invariant), got {names:?}"
    );
    Ok(())
}

#[test]
fn dependency_impact_multi_hop_records_correct_depths() -> Result<()> {
    // Build a three-deep chain: deep_fn → mid_fn → root_fn (root_fn
    // calls mid_fn which calls deep_fn). Querying `deep_fn` must report
    // mid_fn at depth 1 and root_fn at depth 2.
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_chain_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn deep_fn() {}

pub fn mid_fn() {
    deep_fn();
}

pub fn root_fn() {
    mid_fn();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "deep_fn".to_string(),
        path: workspace_arg(&temp),
        max_depth: 5,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };
    let result = execute_dependency_impact(&args)?;

    // Depth 1 must include the direct caller (mid_fn).
    let depth1: Vec<&str> = result
        .data
        .impacted_symbols
        .iter()
        .filter(|s| s.depth == 1)
        .map(|s| s.symbol.name.as_str())
        .collect();
    assert!(
        depth1.contains(&"mid_fn"),
        "depth-1 must include direct caller mid_fn, got {depth1:?}"
    );

    // Depth >=2 must include root_fn (caller of mid_fn).
    let depth2_or_more: Vec<&str> = result
        .data
        .impacted_symbols
        .iter()
        .filter(|s| s.depth >= 2)
        .map(|s| s.symbol.name.as_str())
        .collect();
    assert!(
        depth2_or_more.contains(&"root_fn"),
        "depth-2 must include root_fn (caller of mid_fn), got {depth2_or_more:?}"
    );
    Ok(())
}

#[test]
fn dependency_impact_same_simple_name_qualified_query_no_frontier_leak() -> Result<()> {
    // Codex post-review finding #3: the existing
    // `dependency_impact_stays_anchored_to_resolved_start_node` test
    // uses uniquely-named symbols (`alpha_helper`, `beta_helper`), so
    // a regression that re-introduced the DB15 bug class —
    // qualified-name broadening through same-simple-name depth-1
    // dispatch — would still pass.
    //
    // Freeze the minimal reproducer with two same-simple-name
    // `helper()` functions in disjoint inline modules so the Rust
    // plugin emits distinct qualified names `alpha::helper` /
    // `beta::helper`:
    //
    //   pub mod alpha {
    //       impl AlphaMarker { pub fn helper() {} }
    //       pub fn caller_a() { AlphaMarker::helper(); }
    //   }
    //   pub mod beta {
    //       impl BetaMarker { pub fn helper() {} }
    //       pub fn caller_b() { BetaMarker::helper(); }
    //   }
    //
    // We use `impl Foo { fn helper() {} }` because the Rust plugin
    // reliably emits `Foo::helper` as the qualified name for
    // inherent methods, whereas free functions inside inline or
    // file-based modules don't always get their module prefix.
    // Query `AlphaMarker::helper` (canonical qualified name). The
    // strict resolver MUST land on exactly one NodeId; the BFS
    // frontier MUST NOT re-dispatch by the simple name `helper` at
    // depth 1 (that is precisely the DB15 bug).
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_same_name_frontier"
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
    pub fn helper() {}
}
pub fn caller_a() {
    AlphaMarker::helper();
}
pub fn root_a() {
    caller_a();
}

pub struct BetaMarker;
impl BetaMarker {
    pub fn helper() {}
}
pub fn caller_b() {
    BetaMarker::helper();
}
pub fn root_b() {
    caller_b();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "AlphaMarker::helper".to_string(),
        path: workspace_arg(&temp),
        max_depth: 5,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };
    let result = execute_dependency_impact(&args)?;

    let names: Vec<&str> = result
        .data
        .impacted_symbols
        .iter()
        .map(|s| s.symbol.name.as_str())
        .collect();

    // `AlphaMarker::helper` is called by `caller_a`, which is called
    // by `root_a`. Depth-1 / depth-2 MUST contain those.
    assert!(
        names.contains(&"caller_a"),
        "depth-1 must include caller_a (direct caller of AlphaMarker::helper), got {names:?}"
    );
    assert!(
        names.contains(&"root_a"),
        "depth-2 must include root_a (transitive caller), got {names:?}"
    );

    // The DB15-class frontier bug would broaden the depth-1 frontier
    // by the simple name `helper`, pulling in `caller_b` and
    // `root_b`. Freeze that bug out.
    assert!(
        !names.contains(&"caller_b"),
        "caller_b must NOT leak into AlphaMarker::helper's impact result — \
         this is the DB15 same-simple-name frontier broadening bug class, got {names:?}"
    );
    assert!(
        !names.contains(&"root_b"),
        "root_b must NOT leak into AlphaMarker::helper's impact result \
         (DB15 frontier regression freeze), got {names:?}"
    );
    Ok(())
}

#[test]
fn dependency_impact_rejects_ambiguous_simple_name() -> Result<()> {
    let temp = write_ambiguous_chain_fixture()?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "helper".to_string(), // ambiguous across alpha + beta
        path: workspace_arg(&temp),
        max_depth: 5,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };
    let result = execute_dependency_impact(&args);
    let err = match result {
        Ok(_) => panic!(
            "dependency_impact must reject ambiguous simple-name queries rather than \
             silently broadening the BFS frontier (NodeId-anchored invariant)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("ambiguous") || err.contains("canonical qualified name"),
        "error message must hint at the canonical-name workaround, got: {err}"
    );
    Ok(())
}

// ============================================================================
// show_dependencies — NodeId-anchored; bidirectional frontier invariant
// ============================================================================

#[test]
fn show_dependencies_stays_anchored_to_resolved_seed() -> Result<()> {
    // Two disjoint chains in one file, with module-unique simple names
    // so `find_nodes_by_name` resolves to a single seed. Any leakage
    // from the alpha_* seed into the beta_* chain would indicate a
    // name-keyed broadening bug in the BFS frontier.
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_deps_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn alpha_helper() {}

pub fn alpha_caller() {
    alpha_helper();
}

pub fn alpha_root() {
    alpha_caller();
}

pub fn beta_helper() {}

pub fn beta_caller() {
    beta_helper();
}

pub fn beta_root() {
    beta_caller();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = ShowDependenciesArgs {
        file_path: None,
        symbol_name: Some("alpha_caller".to_string()),
        path: workspace_arg(&temp),
        max_depth: 5,
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_get_dependencies(&args)?;

    let mut node_names: Vec<String> = Vec::new();
    for edge in &result.data.edges {
        if let Some(from) = &edge.from {
            node_names.push(from.name.clone());
        }
        if let Some(to) = &edge.to {
            node_names.push(to.name.clone());
        }
    }
    node_names.sort();
    node_names.dedup();

    assert!(
        node_names.iter().any(|n| n == "alpha_root"),
        "incoming walk from alpha_caller must reach alpha_root \
         (alpha_root calls alpha_caller), got {node_names:?}"
    );
    // The frontier invariant: no name-keyed fan-out can introduce
    // beta_* into alpha's dependency tree.
    assert!(
        !node_names.iter().any(|n| n == "beta_caller"),
        "beta_caller must NOT appear in alpha's dependency tree \
         (frontier invariant), got {node_names:?}"
    );
    assert!(
        !node_names.iter().any(|n| n == "beta_root"),
        "beta_root must NOT appear in alpha's dependency tree \
         (frontier invariant), got {node_names:?}"
    );
    assert!(
        !node_names.iter().any(|n| n == "beta_helper"),
        "beta_helper must NOT appear in alpha's dependency tree \
         (frontier invariant), got {node_names:?}"
    );
    Ok(())
}

#[test]
fn show_dependencies_same_simple_name_qualified_query_no_frontier_leak() -> Result<()> {
    // Codex post-review finding #3 (outgoing-direction analog): the
    // existing `show_dependencies_stays_anchored_to_resolved_seed`
    // test uses uniquely-named symbols. That leaves the DB15-class
    // bug (depth-1 frontier re-dispatch by simple name across
    // disjoint chains) unfrozen.
    //
    // Use an inherent-impl fixture so the Rust plugin emits
    // `AlphaMarker::helper` / `BetaMarker::helper` as distinct
    // qualified names. Seed the walk on `AlphaMarker::helper`
    // (qualified) and assert no BetaMarker-side node ever reaches
    // the result edges.
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_deps_same_name"
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
    pub fn helper() {}
}
pub fn caller_a() {
    AlphaMarker::helper();
}
pub fn root_a() {
    caller_a();
}

pub struct BetaMarker;
impl BetaMarker {
    pub fn helper() {}
}
pub fn caller_b() {
    BetaMarker::helper();
}
pub fn root_b() {
    caller_b();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = ShowDependenciesArgs {
        file_path: None,
        symbol_name: Some("AlphaMarker::helper".to_string()),
        path: workspace_arg(&temp),
        max_depth: 5,
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_get_dependencies(&args)?;

    let mut node_names: Vec<String> = Vec::new();
    for edge in &result.data.edges {
        if let Some(from) = &edge.from {
            node_names.push(from.name.clone());
        }
        if let Some(to) = &edge.to {
            node_names.push(to.name.clone());
        }
    }
    node_names.sort();
    node_names.dedup();

    // AlphaMarker::helper has an incoming Calls edge from caller_a.
    // `caller_a` must appear in the dependency tree.
    assert!(
        node_names.iter().any(|n| n == "caller_a"),
        "incoming walk from AlphaMarker::helper must reach caller_a, got {node_names:?}"
    );
    // The DB15-class frontier broadening would re-dispatch by the
    // simple name `helper` at depth 1 and pull in `caller_b` /
    // `root_b`. Freeze that bug out.
    assert!(
        !node_names.iter().any(|n| n == "caller_b"),
        "caller_b must NOT appear in AlphaMarker::helper's dependency tree — \
         DB15 same-simple-name frontier regression freeze, got {node_names:?}"
    );
    assert!(
        !node_names.iter().any(|n| n == "root_b"),
        "root_b must NOT appear in AlphaMarker::helper's dependency tree \
         (DB15 frontier regression freeze), got {node_names:?}"
    );
    Ok(())
}

#[test]
fn show_dependencies_bidirectional_emits_callers_and_callees() -> Result<()> {
    // Three-node chain in one file so cross-file unification is not a
    // prerequisite: root_fn -> mid_fn -> deep_fn. Seeding on mid_fn must
    // produce one incoming edge (from root_fn) and one outgoing edge (to
    // deep_fn).
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "db16_bidirectional_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    fs::write(
        root.join("src/lib.rs"),
        r"pub fn deep_fn() {}

pub fn mid_fn() {
    deep_fn();
}

pub fn root_fn() {
    mid_fn();
}
",
    )?;
    index_fixture(temp.path())?;

    let args = ShowDependenciesArgs {
        file_path: None,
        symbol_name: Some("mid_fn".to_string()),
        path: workspace_arg(&temp),
        max_depth: 5,
        max_results: 100,
        pagination: paging(),
    };
    let result = execute_get_dependencies(&args)?;

    let has_incoming = result
        .data
        .edges
        .iter()
        .any(|e| e.relation_type == "callers");
    let has_outgoing = result
        .data
        .edges
        .iter()
        .any(|e| e.relation_type == "callees");
    assert!(
        has_incoming,
        "bidirectional walk must emit `callers` edges (incoming), got {:?}",
        result.data.edges
    );
    assert!(
        has_outgoing,
        "bidirectional walk must emit `callees` edges (outgoing), got {:?}",
        result.data.edges
    );
    Ok(())
}

// Avoid a dead-code clippy lint if only some tests run.
#[allow(dead_code)]
fn _silence_unused_pathbuf_warning() {
    let _: PathBuf = PathBuf::new();
}
