//! Cross-language `returns:` predicate integration suite.
//!
//! Closes the BadLiveware Go-batch DAG `B2_TESTS` unit
//! (`docs/development/public-issue-triage/2026-04-29_badliveware_go_batch_dag.toml`)
//! which in turn closes verivus-oss/sqry#76 and verivus-oss/sqry#155.
//!
//! # Surfaces under test
//!
//! For every covered language we build a fresh `.sqry/` index in a temp
//! directory and exercise three independent code paths:
//!
//! 1. **Planner CLI** — `sqry --json plan-query "kind:<k> returns:<T>"`.
//!    This drives `sqry-db::planner::execute_plan` end-to-end through
//!    the user-facing CLI binary. Locks the contract delivered by
//!    `B2_PLANNER` (commit `8cedb66f1`): byte-exact match against
//!    `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }`
//!    edge targets.
//!
//! 2. **Legacy graph backend CLI** — `sqry --json query "returns:<T>"`.
//!    This drives the legacy `sqry-core::query::executor::graph_eval`
//!    pipeline through the user-facing CLI binary. Locks the contract
//!    delivered by `B2_EXECUTOR` (commit `7277dc46b`): the legacy
//!    backend must agree with the planner — same byte-exact edge-based
//!    semantics, no fall-back to `NodeEntry.signature` substring
//!    matching.
//!
//! 3. **MCP `relation_query`** — `sqry_mcp::tool_handlers::execute_relation_query`
//!    with `relation: RelationType::Returns`. Drives the MCP tool entry
//!    point that AI assistants and the LSP host hit. Locks the contract
//!    that the handler does not panic and returns a deterministic shape
//!    against fixtures whose plugin emits `TypeOf{Return}` edges.
//!
//! # Per-language coverage matrix
//!
//! Per `B2_TESTS.acceptance` every covered language must have BOTH a
//! positive match (the function whose declared return type matches)
//! AND a negative non-match (a different function returning a different
//! type does NOT appear in the positive query's result set).
//!
//! The byte-exact return-type strings encoded in [`CASES`] were
//! verified against each plugin's actual emission via
//! `sqry --json graph edges --kind type_of` against the same fixtures
//! used in this test, per the DAG `failure_modes` directive
//! ("language emits Return edge but with a name that doesn't match the
//! user-visible type spelling - fix in plugin, not in test"). The lookup
//! table is intentionally a literal `const`-like construct so missing
//! combinations are syntactically obvious.
//!
//! # TypeScript carve-out (resolved)
//!
//! The first iteration of this suite locked the TypeScript row to an
//! empty-set guard because of a then-believed gap in
//! `sqry-lang-typescript/src/relations/graph_builder.rs`'s
//! `build_return_type_edges` dispatch (the
//! `ASTGraph::get_callable_context(node.id())` guard supposedly rejecting
//! `function_declaration` / `method_definition` nodes). That assumption
//! turned out to be wrong: against the live TS fixture below, all three
//! surfaces (`plan-query`, legacy `query`, MCP `relation_query`) emit a
//! real `TypeOf{Return}` edge for `fetchUser -> Promise<User>` and
//! `getUserId -> string`. The regression-guard assertion fired during the
//! BadLiveware Go-batch B2 cluster's MCP-side fix and forced flipping
//! `plugin_emits_return_edges` to `true` — exactly the signal the empty-
//! set guard was designed to produce. Today TypeScript participates in
//! the same byte-exact positive-match contract as the other six languages.
//!
//! # Fresh-index temp workspace per test
//!
//! Per `B2_TESTS.constraints` ("fresh-index temp workspace per test, no
//! shared `.sqry/` state"), every `#[test]` allocates its own
//! [`tempfile::TempDir`], writes its language fixture, and invokes
//! `sqry index` against that root before any query runs. The temp dir
//! is dropped at end-of-test, removing the `.sqry/` directory. Tests
//! never share an index across language boundaries.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{PaginationArgs, RelationQueryArgs, RelationType};
use sqry_mcp::tool_handlers::execute_relation_query;

// ============================================================================
// Per-language coverage matrix
// ============================================================================

/// One row of the cross-language `returns:` matrix.
///
/// Locks: language label, `kind:` discriminator (so `plan-query` builds a
/// well-formed plan), the positive function name + its byte-exact return
/// type, the negative function name + its byte-exact return type, and
/// the source-text fixture written to disk.
///
/// `pos_return_type` and `neg_return_type` are byte-exact target-node
/// names — they MUST match the plugin's actual `TypeOf{Return}` emission
/// per the DAG's "fix in plugin, not in test" rule.
#[derive(Debug, Clone, Copy)]
struct LangCase {
    label: &'static str,
    kind_discriminator: &'static str,
    pos_function: &'static str,
    pos_return_type: &'static str,
    neg_function: &'static str,
    neg_return_type: &'static str,
    fixture_filename: &'static str,
    fixture_source: &'static str,
    /// `true` iff the plugin's user-facing emission produces at least one
    /// `TypeOf{Return}` edge for `pos_function` against `fixture_source`.
    /// When `false`, the test locks the observed empty-set behavior across
    /// all three surfaces as a regression guard (see crate-level
    /// "TypeScript carve-out" docs).
    plugin_emits_return_edges: bool,
}

const GO_FIXTURE: &str = "package main

import \"fmt\"

type Config struct{}

func parseConfig() error {
    return fmt.Errorf(\"nope\")
}

func useSelector() bool {
    return true
}

func main() {
    _ = parseConfig()
    _ = useSelector()
}
";

const RUST_FIXTURE: &str = "pub struct User { pub name: String }

pub fn find_user(id: u32) -> Option<User> {
    let _ = id;
    None
}

pub fn count_users() -> usize {
    0
}
";

/// Sibling Cargo.toml so the Rust plugin treats the fixture as a
/// real crate root (single-file libs without a manifest still parse,
/// but `Cargo.toml` makes the index discoverable through the same
/// path the user CLI follows).
const RUST_CARGO_TOML: &str = "[package]
name = \"sqry_returns_fixture\"
version = \"0.0.1\"
edition = \"2021\"
";

const TS_FIXTURE: &str = "type User = { name: string };

export function fetchUser(id: number): Promise<User> {
    return Promise.resolve({ name: String(id) });
}

export function getUserId(): string {
    return \"abc\";
}
";

const PYTHON_FIXTURE: &str = "def parse_config(path: str) -> int:
    _ = path
    return 42

def get_name() -> str:
    return \"x\"
";

const JAVA_FIXTURE: &str = "import java.util.Optional;

public class Repo {
    static class User {}

    public Optional<User> findUser(int id) {
        return Optional.empty();
    }

    public int userCount() {
        return 0;
    }
}
";

const KOTLIN_FIXTURE: &str = "data class User(val id: Int)

class Repo {
    fun findUser(id: Int): User? {
        return null
    }
    fun userCount(): Int {
        return 0
    }
}
";

const CSHARP_FIXTURE: &str = "using System.Threading.Tasks;

namespace Demo {
    public class User {}

    public class Repo {
        public Task<User> FetchAsync(int id) {
            return Task.FromResult(new User());
        }
        public int GetCount() {
            return 0;
        }
    }
}
";

/// The 7-language matrix. Order matches the DAG `B2_TESTS.summary` enumeration.
const CASES: &[LangCase] = &[
    LangCase {
        label: "go",
        kind_discriminator: "function",
        pos_function: "parseConfig",
        pos_return_type: "error",
        neg_function: "useSelector",
        neg_return_type: "bool",
        fixture_filename: "main.go",
        fixture_source: GO_FIXTURE,
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "rust",
        kind_discriminator: "function",
        pos_function: "find_user",
        pos_return_type: "Option<User>",
        neg_function: "count_users",
        neg_return_type: "usize",
        fixture_filename: "src/lib.rs",
        fixture_source: RUST_FIXTURE,
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "typescript",
        kind_discriminator: "function",
        pos_function: "fetchUser",
        pos_return_type: "Promise<User>",
        neg_function: "getUserId",
        neg_return_type: "string",
        fixture_filename: "index.ts",
        fixture_source: TS_FIXTURE,
        // The TS plugin DOES emit `TypeOf{Return}` edges end-to-end against
        // this fixture: `fetchUser` -> `Promise<User>` and `getUserId` ->
        // `string` were both observed against all three surfaces during
        // the BadLiveware Go-batch B2 cluster's MCP-side fix. The
        // crate-level "TypeScript carve-out" docs above describe the
        // historical state that prompted this row to start at `false`;
        // the regression-guard assertion fired and forced this flip, as
        // designed.
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "python",
        kind_discriminator: "function",
        pos_function: "parse_config",
        pos_return_type: "int",
        neg_function: "get_name",
        neg_return_type: "str",
        fixture_filename: "main.py",
        fixture_source: PYTHON_FIXTURE,
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "java",
        kind_discriminator: "method",
        pos_function: "findUser",
        pos_return_type: "Optional<User>",
        neg_function: "userCount",
        neg_return_type: "int",
        fixture_filename: "Repo.java",
        fixture_source: JAVA_FIXTURE,
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "kotlin",
        kind_discriminator: "method",
        pos_function: "findUser",
        pos_return_type: "User?",
        neg_function: "userCount",
        neg_return_type: "Int",
        fixture_filename: "Repo.kt",
        fixture_source: KOTLIN_FIXTURE,
        plugin_emits_return_edges: true,
    },
    LangCase {
        label: "csharp",
        kind_discriminator: "method",
        pos_function: "FetchAsync",
        pos_return_type: "Task<User>",
        neg_function: "GetCount",
        neg_return_type: "int",
        fixture_filename: "Repo.cs",
        fixture_source: CSHARP_FIXTURE,
        plugin_emits_return_edges: true,
    },
];

// ============================================================================
// Fixture / index helpers
// ============================================================================

/// Initialize the path-resolver discovery cache, engine cache, and the
/// trace-path / subgraph telemetry caches exactly once across the test
/// binary. The MCP relation handler chains through `build_graph_metadata`
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

/// Locate the `sqry` CLI binary built next to the test binary in
/// `target/{debug,release}/sqry`. Mirrors the resolver used by
/// `sqry-mcp/tests/installed_feature_surface_e2e.rs`.
fn sqry_bin() -> PathBuf {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.parent().expect("workspace root");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let binary_name = if exe_suffix.is_empty() {
        "sqry".to_string()
    } else {
        format!("sqry{exe_suffix}")
    };

    let debug_path = workspace.join("target/debug").join(&binary_name);
    if debug_path.is_file() {
        return debug_path;
    }
    let release_path = workspace.join("target/release").join(&binary_name);
    if release_path.is_file() {
        return release_path;
    }
    panic!(
        "Could not find sqry binary. Tried target/debug/{binary_name} and target/release/{binary_name}. \
         Run `cargo build --bin sqry` first or set SQRY_E2E_SQRY_BIN."
    );
}

/// Materialize the language fixture under a fresh `TempDir` and return
/// the workspace root. For Rust we also drop a Cargo.toml beside the
/// `src/lib.rs` so the plugin's discovery sees the canonical layout.
fn write_fixture(case: &LangCase) -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();

    let target = root.join(case.fixture_filename);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, case.fixture_source)?;

    if case.label == "rust" {
        fs::write(root.join("Cargo.toml"), RUST_CARGO_TOML)?;
    }

    Ok(temp)
}

/// Build a fresh `.sqry/` index for the fixture by invoking the live
/// `sqry index` CLI binary. The CLI codepath exercises the same plugin
/// pipeline the user hits, so this matches end-to-end behavior.
fn build_index(root: &Path) -> Result<()> {
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .output()
        .with_context(|| format!("invoke `sqry index {}`", root.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "`sqry index {}` failed:\nstdout:\n{}\nstderr:\n{}",
            root.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(())
}

// ============================================================================
// Surface 1 — planner CLI (`sqry plan-query`)
// ============================================================================

/// Run `sqry --json plan-query "kind:<k> returns:<T>"` against the fixture
/// and return the matched function names extracted from the JSON payload.
fn planner_query_names(root: &Path, case: &LangCase, return_type: &str) -> Result<Vec<String>> {
    let query = format!(
        "kind:{kind} returns:{ret}",
        kind = case.kind_discriminator,
        ret = return_type
    );
    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("plan-query")
        .arg(&query)
        .arg(root)
        .output()
        .with_context(|| format!("invoke `sqry --json plan-query {query}`"))?;
    if !output.status.success() {
        anyhow::bail!(
            "`sqry plan-query {query}` failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse plan-query JSON for `{query}`"))?;
    let arr = parsed
        .as_array()
        .with_context(|| format!("plan-query JSON for `{query}` must be an array"))?;
    Ok(arr
        .iter()
        .filter_map(|hit| hit.get("name").and_then(Value::as_str))
        .map(String::from)
        .collect())
}

/// Lock the planner-CLI contract for one language: positive function
/// matches its declared return type, negative function does not appear
/// in the positive query's result set.
fn assert_planner_cli(case: &LangCase) -> Result<()> {
    let temp = write_fixture(case)?;
    build_index(temp.path())?;

    let pos_hits = planner_query_names(temp.path(), case, case.pos_return_type)?;
    let neg_hits = planner_query_names(temp.path(), case, case.neg_return_type)?;

    if case.plugin_emits_return_edges {
        assert!(
            pos_hits.iter().any(|n| n == case.pos_function),
            "[{lang}] plan-query `kind:{kind} returns:{ret}` must include `{pos}`; got {pos_hits:?}",
            lang = case.label,
            kind = case.kind_discriminator,
            ret = case.pos_return_type,
            pos = case.pos_function,
        );
        assert!(
            !pos_hits.iter().any(|n| n == case.neg_function),
            "[{lang}] plan-query `kind:{kind} returns:{ret}` must NOT include `{neg}` \
             (neg returns `{nret}`, not `{ret}`); got {pos_hits:?}",
            lang = case.label,
            kind = case.kind_discriminator,
            ret = case.pos_return_type,
            neg = case.neg_function,
            nret = case.neg_return_type,
        );
        // Symmetric: negative type matches the negative function and not the positive.
        assert!(
            neg_hits.iter().any(|n| n == case.neg_function),
            "[{lang}] plan-query `kind:{kind} returns:{nret}` must include `{neg}`; got {neg_hits:?}",
            lang = case.label,
            kind = case.kind_discriminator,
            nret = case.neg_return_type,
            neg = case.neg_function,
        );
        assert!(
            !neg_hits.iter().any(|n| n == case.pos_function),
            "[{lang}] plan-query `kind:{kind} returns:{nret}` must NOT include `{pos}`; got {neg_hits:?}",
            lang = case.label,
            kind = case.kind_discriminator,
            nret = case.neg_return_type,
            pos = case.pos_function,
        );
    } else {
        // Plugin emission gap — lock empty-set as a regression guard against
        // silent fall-back to signature-text substring matching. When the
        // plugin gap is closed these assertions will start to fail and force
        // the matrix entry to flip `plugin_emits_return_edges = true`.
        assert!(
            pos_hits.is_empty(),
            "[{lang}] plan-query `kind:{kind} returns:{ret}` is expected to be empty until \
             the {lang} plugin emits TypeOf{{Return}} edges; got {pos_hits:?}. \
             If this fires, the plugin started emitting Return-context edges — flip \
             plugin_emits_return_edges to true and update the assertion to a positive match.",
            lang = case.label,
            kind = case.kind_discriminator,
            ret = case.pos_return_type,
        );
        assert!(
            neg_hits.is_empty(),
            "[{lang}] plan-query `kind:{kind} returns:{nret}` is expected to be empty until \
             the {lang} plugin emits TypeOf{{Return}} edges; got {neg_hits:?}",
            lang = case.label,
            kind = case.kind_discriminator,
            nret = case.neg_return_type,
        );
    }

    Ok(())
}

// ============================================================================
// Surface 2 — legacy graph backend CLI (`sqry query`)
// ============================================================================

/// Run `sqry --json query "returns:<T>"` against the fixture and return
/// the matched function names extracted from `results[].name` in the
/// JSON payload. The legacy backend's JSON shape is documented in
/// `sqry-cli/tests/boolean_query_regression_tests.rs`.
fn legacy_query_names(root: &Path, return_type: &str) -> Result<Vec<String>> {
    let pattern = format!("returns:{return_type}");
    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("query")
        .arg(&pattern)
        .arg(root)
        .output()
        .with_context(|| format!("invoke `sqry --json query {pattern}`"))?;
    if !output.status.success() {
        anyhow::bail!(
            "`sqry query {pattern}` failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse legacy-query JSON for `{pattern}`"))?;
    Ok(parsed
        .get("results")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default())
}

/// Lock the legacy-graph-backend contract for one language. Same shape
/// as [`assert_planner_cli`] — the legacy backend MUST agree with the
/// planner per `B2_EXECUTOR.acceptance` ("legacy backend and planner
/// produce identical results").
fn assert_legacy_query_cli(case: &LangCase) -> Result<()> {
    let temp = write_fixture(case)?;
    build_index(temp.path())?;

    let pos_hits = legacy_query_names(temp.path(), case.pos_return_type)?;
    let neg_hits = legacy_query_names(temp.path(), case.neg_return_type)?;

    if case.plugin_emits_return_edges {
        assert!(
            pos_hits.iter().any(|n| n == case.pos_function),
            "[{lang}] legacy `query returns:{ret}` must include `{pos}`; got {pos_hits:?}",
            lang = case.label,
            ret = case.pos_return_type,
            pos = case.pos_function,
        );
        assert!(
            !pos_hits.iter().any(|n| n == case.neg_function),
            "[{lang}] legacy `query returns:{ret}` must NOT include `{neg}`; got {pos_hits:?}",
            lang = case.label,
            ret = case.pos_return_type,
            neg = case.neg_function,
        );
        assert!(
            neg_hits.iter().any(|n| n == case.neg_function),
            "[{lang}] legacy `query returns:{nret}` must include `{neg}`; got {neg_hits:?}",
            lang = case.label,
            nret = case.neg_return_type,
            neg = case.neg_function,
        );
        assert!(
            !neg_hits.iter().any(|n| n == case.pos_function),
            "[{lang}] legacy `query returns:{nret}` must NOT include `{pos}`; got {neg_hits:?}",
            lang = case.label,
            nret = case.neg_return_type,
            pos = case.pos_function,
        );
    } else {
        assert!(
            pos_hits.is_empty(),
            "[{lang}] legacy `query returns:{ret}` is expected to be empty until the {lang} \
             plugin emits TypeOf{{Return}} edges; got {pos_hits:?}. If this fires, the plugin \
             started emitting Return-context edges — flip plugin_emits_return_edges to true.",
            lang = case.label,
            ret = case.pos_return_type,
        );
        assert!(
            neg_hits.is_empty(),
            "[{lang}] legacy `query returns:{nret}` is expected to be empty until the {lang} \
             plugin emits TypeOf{{Return}} edges; got {neg_hits:?}",
            lang = case.label,
            nret = case.neg_return_type,
        );
    }

    Ok(())
}

// ============================================================================
// Surface 3 — MCP `relation_query`
// ============================================================================

/// Reduced view of one `RelationEdgeData` entry surfaced by
/// `execute_relation_query` so we don't have to name the (currently
/// `pub(crate)`) `RelationEdgeData` / `RelationQueryData` types from
/// `sqry_mcp::execution::types` directly.
#[derive(Debug, Clone)]
struct McpRelationEdgeView {
    from_name: Option<String>,
    to_name: Option<String>,
}

/// Reduced view of one `relation_query` response.
#[derive(Debug, Clone)]
struct McpRelationsView {
    relation_type: String,
    edges: Vec<McpRelationEdgeView>,
}

/// Run `execute_relation_query` for the given symbol and project the
/// response into [`McpRelationsView`].
fn mcp_relation_query_returns(root: &Path, symbol: &str) -> Result<McpRelationsView> {
    init_caches();
    let args = RelationQueryArgs {
        symbol: symbol.to_string(),
        relation: RelationType::Returns,
        path: root.to_string_lossy().into_owned(),
        max_depth: 1,
        max_results: 100,
        pagination: PaginationArgs {
            offset: 0,
            size: 100,
        },
        framework: None,
        resolved_via: None,
    };
    let exec = execute_relation_query(&args)?;
    let data = exec.data;
    let edges: Vec<McpRelationEdgeView> = data
        .relations
        .iter()
        .map(|edge| McpRelationEdgeView {
            from_name: edge.from.as_ref().map(|f| f.name.clone()),
            to_name: edge.to.as_ref().map(|t| t.name.clone()),
        })
        .collect();
    Ok(McpRelationsView {
        relation_type: data.relation_type,
        edges,
    })
}

/// Lock the MCP `relation_query relation:returns` shape for one language.
///
/// **Real edge contract.** Post-fix (B2 cluster of the BadLiveware Go-batch
/// DAG), `sqry_mcp::execution::tools::relations::collect_returns` walks
/// outgoing `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }`
/// edges directly. This means the MCP surface MUST agree byte-for-byte
/// with the planner / legacy CLI surfaces:
///
/// - `relation_type == "returns"`
/// - For the positive symbol: at least one `RelationEdgeData` with
///   `from.name == pos_function` AND `to.name == pos_return_type`.
/// - For the negative symbol: same — at least one entry with
///   `to.name == neg_return_type`. The negative symbol's response must
///   NOT contain any entry with `to.name == pos_return_type` (proves
///   that we did not regress to a "match anything" stub or to
///   cross-symbol leakage).
///
/// For languages whose plugin does not yet emit `TypeOf{Return}` edges
/// (only TypeScript today — see crate-level "TypeScript carve-out"
/// docs), the MCP surface is locked to an empty `relations` set,
/// matching the planner / legacy contract.
fn assert_mcp_relation_query(case: &LangCase) -> Result<()> {
    let temp = write_fixture(case)?;
    build_index(temp.path())?;

    let pos_view = mcp_relation_query_returns(temp.path(), case.pos_function)?;
    assert_eq!(
        pos_view.relation_type, "returns",
        "[{}] MCP relation_query for `{}` must echo relation_type=returns",
        case.label, case.pos_function
    );

    let neg_view = mcp_relation_query_returns(temp.path(), case.neg_function)?;
    assert_eq!(
        neg_view.relation_type, "returns",
        "[{}] MCP relation_query for `{}` must echo relation_type=returns",
        case.label, case.neg_function
    );

    if case.plugin_emits_return_edges {
        assert!(
            pos_view.edges.iter().any(|e| {
                e.from_name.as_deref() == Some(case.pos_function)
                    && e.to_name.as_deref() == Some(case.pos_return_type)
            }),
            "[{lang}] MCP relation_query relation:returns symbol:{pos} must include an edge \
             whose `from.name == {pos:?}` AND `to.name == {ret:?}`; got {pos_view:#?}",
            lang = case.label,
            pos = case.pos_function,
            ret = case.pos_return_type,
        );
        // Symmetric: negative symbol must surface its own real Return edge.
        assert!(
            neg_view.edges.iter().any(|e| {
                e.from_name.as_deref() == Some(case.neg_function)
                    && e.to_name.as_deref() == Some(case.neg_return_type)
            }),
            "[{lang}] MCP relation_query relation:returns symbol:{neg} must include an edge \
             whose `from.name == {neg:?}` AND `to.name == {nret:?}`; got {neg_view:#?}",
            lang = case.label,
            neg = case.neg_function,
            nret = case.neg_return_type,
        );
        // Cross-symbol leakage guard: the negative symbol's response must
        // never carry the positive return type. (Equality on `to.name` is
        // sufficient — we already established the positive symbol's own
        // response is byte-exact to `pos_return_type`.)
        assert!(
            !neg_view
                .edges
                .iter()
                .any(|e| e.to_name.as_deref() == Some(case.pos_return_type)),
            "[{lang}] MCP relation_query relation:returns symbol:{neg} must NOT carry the \
             positive return type `{ret}` (cross-symbol leakage); got {neg_view:#?}",
            lang = case.label,
            neg = case.neg_function,
            ret = case.pos_return_type,
        );
    } else {
        // Plugin-emission gap. Lock the empty-set as a regression guard:
        // when the plugin starts emitting Return edges these assertions
        // will fire and force flipping `plugin_emits_return_edges = true`.
        assert!(
            pos_view.edges.is_empty(),
            "[{lang}] MCP relation_query relation:returns symbol:{pos} is expected to be empty \
             until the {lang} plugin emits TypeOf{{Return}} edges; got {pos_view:#?}. \
             If this fires, flip plugin_emits_return_edges to true and update the assertion \
             to check for a real `from`/`to` edge entry.",
            lang = case.label,
            pos = case.pos_function,
        );
        assert!(
            neg_view.edges.is_empty(),
            "[{lang}] MCP relation_query relation:returns symbol:{neg} is expected to be empty \
             until the {lang} plugin emits TypeOf{{Return}} edges; got {neg_view:#?}",
            lang = case.label,
            neg = case.neg_function,
        );
    }

    Ok(())
}

// ============================================================================
// Per-language tests — 7 langs × 3 surfaces = 21 #[test] entries
// ============================================================================
//
// Each #[test] resolves to exactly one row of the [`CASES`] matrix by
// label. The split (rather than one parameterized loop) is intentional:
// `cargo test` reports each language × surface independently, so a
// regression in (say) the Kotlin planner CLI surfaces as exactly one
// failing test name rather than burying the full matrix under one
// aggregated failure.

fn case_for(label: &str) -> &'static LangCase {
    CASES
        .iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("missing matrix entry for language `{label}`"))
}

// ---------- Go ----------

#[test]
fn go_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("go"))
}

#[test]
fn go_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("go"))
}

#[test]
fn go_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("go"))
}

// ---------- Rust ----------

#[test]
fn rust_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("rust"))
}

#[test]
fn rust_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("rust"))
}

#[test]
fn rust_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("rust"))
}

// ---------- TypeScript ----------

#[test]
fn typescript_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("typescript"))
}

#[test]
fn typescript_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("typescript"))
}

#[test]
fn typescript_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("typescript"))
}

// ---------- Python ----------

#[test]
fn python_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("python"))
}

#[test]
fn python_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("python"))
}

#[test]
fn python_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("python"))
}

// ---------- Java ----------

#[test]
fn java_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("java"))
}

#[test]
fn java_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("java"))
}

#[test]
fn java_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("java"))
}

// ---------- Kotlin ----------

#[test]
fn kotlin_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("kotlin"))
}

#[test]
fn kotlin_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("kotlin"))
}

#[test]
fn kotlin_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("kotlin"))
}

// ---------- C# ----------

#[test]
fn csharp_planner_cli_returns_predicate() -> Result<()> {
    assert_planner_cli(case_for("csharp"))
}

#[test]
fn csharp_legacy_query_cli_returns_predicate() -> Result<()> {
    assert_legacy_query_cli(case_for("csharp"))
}

#[test]
fn csharp_mcp_relation_query_returns() -> Result<()> {
    assert_mcp_relation_query(case_for("csharp"))
}

// ============================================================================
// Sanity: the matrix covers every language listed in the DAG
// ============================================================================

#[test]
fn matrix_covers_every_dag_language() {
    let expected = [
        "go",
        "rust",
        "typescript",
        "python",
        "java",
        "kotlin",
        "csharp",
    ];
    for lang in expected {
        assert!(
            CASES.iter().any(|c| c.label == lang),
            "DAG B2_TESTS scope requires `{lang}`, but it is missing from the CASES matrix"
        );
    }
    assert_eq!(
        CASES.len(),
        expected.len(),
        "CASES matrix has {} entries but DAG B2_TESTS scope requires exactly {}",
        CASES.len(),
        expected.len()
    );
}
