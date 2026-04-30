//! Smoke coverage for verivus-oss/sqry#79 / verivus-oss/sqry#158
//! (`D_TESTS` unit of the badliveware-go batch DAG).
//!
//! `D_JSON_THREAD` fixed the global `--json` flag so it threads into every
//! `sqry graph *` subcommand renderer. `global_json_flag_for_graph.rs`
//! locks the contract for `direct-callers` end-to-end and for the
//! gemini-iter-1 BLOCK arms (`provenance`, `resolve`, `status`). This file
//! closes the smoke gap: it enumerates **every** `sqry graph *`
//! subcommand listed by `sqry graph --help`, runs each one through all
//! three documented invocation orders, and asserts the parsed JSON is
//! equivalent across the three orders.
//!
//! Three invocation orders (per the regression guards in
//! `global_json_flag_for_graph.rs`):
//!
//!   1. `sqry --json graph <sub> ...`             (global `--json` before)
//!   2. `sqry graph <sub> ... --json`             (global `--json` after)
//!   3. `sqry graph --format json <sub> ...`      (per-graph `--format`)
//!
//! Comparison is **structural JSON equality** via `serde_json::Value`,
//! not byte-equality, per the DAG `critical_decisions` contract — the
//! renderer is allowed to vary key ordering as long as the parsed shape
//! matches.
//!
//! ## Drift guard
//!
//! `graph_subcommand_enumeration_matches_help_output` parses
//! `sqry graph --help` at runtime and asserts every subcommand listed
//! there is covered by `subcommand_smoke_cases`. This makes the
//! "someone adds a new `graph *` subcommand and forgets to thread
//! `--json`" failure mode (DAG `failure_modes`) impossible to land
//! silently — adding a new subcommand will fail this test until it is
//! registered here.
//!
//! ## Fixture
//!
//! All cases run against the BadLiveware Go fixture
//! (`test-fixtures/badliveware/main.go`) — the same fixture the
//! `direct-callers parseConfig` reproducer in `D_REPRO` /
//! verivus-oss/sqry#79 uses, so the smoke tests exercise the exact code
//! path the public bug reporter hit. The fixture is copied into a fresh
//! `TempDir` per test (no shared `.sqry/` state across tests, per the
//! DAG `constraints`).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Copy the BadLiveware Go fixture into `root`. Mirrors the public
/// reproducer in verivus-oss/sqry#79 so the smoke tests share the bug
/// reporter's actual code path.
fn write_badliveware_fixture(root: &Path) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test-fixtures")
        .join("badliveware")
        .join("main.go");
    let body = fs::read_to_string(&src)
        .unwrap_or_else(|e| panic!("read badliveware fixture {}: {e}", src.display()));
    fs::write(root.join("main.go"), body).expect("write fixture into temp root");
}

/// Build a fresh `.sqry/` index for the fixture so the graph commands
/// have a snapshot to load. One index per test (per DAG `constraints`:
/// no shared state).
fn index(root: &Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

/// One smoke case: a `graph *` subcommand plus the args we pass after
/// the subcommand keyword. The subcommand keyword is NOT included in
/// `args` — the harness inserts it where each invocation order needs it.
#[derive(Clone, Copy)]
struct SmokeCase {
    /// Subcommand keyword exactly as it appears under `sqry graph --help`.
    subcommand: &'static str,
    /// Positional / flag arguments passed after the subcommand keyword.
    /// Example: for `direct-callers parseConfig`, `args = &["parseConfig"]`.
    args: &'static [&'static str],
}

/// Strip fields that legitimately vary between invocations (wall-clock
/// timestamps, etc.) and canonicalize array ordering so structural
/// equality reflects the `--json` threading contract rather than
/// unrelated rendering non-determinism.
///
/// * `status` exposes `age_seconds`, a delta from the snapshot's
///   `created_at` to "now" — running three CLI invocations
///   sequentially will see this advance, so we drop it before
///   comparing. We do NOT drop `created_at` itself because the
///   snapshot is built once per test and read by all three
///   invocations, so it must match.
/// * Several subcommands (`dependency-tree`, `nodes`, `edges`, …)
///   render arrays whose element order depends on hash-map iteration
///   order, which is not stable across invocations. The narrow
///   contract this smoke test exists to lock is that **all three
///   invocation orders thread `--json` into the same renderer**, not
///   that the renderer guarantees array ordering. We therefore
///   recursively sort every JSON array by its serialized child form
///   so two runs that differ only in array element order compare
///   equal.
fn normalize_for_comparison(subcommand: &str, value: &mut Value) {
    if subcommand == "status"
        && let Some(obj) = value.as_object_mut()
    {
        obj.remove("age_seconds");
    }
    canonicalize_arrays(value);
}

/// Recursively sort every JSON array by its serialized child form so
/// element-order non-determinism in the renderer (hash-map iteration,
/// parallel collection, etc.) does not mask the `--json` threading
/// contract this test is locking. Object key order is not touched —
/// `serde_json::Value` already compares objects by key set, not by
/// insertion order.
fn canonicalize_arrays(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items.iter_mut() {
                canonicalize_arrays(item);
            }
            items.sort_by_key(|v| v.to_string());
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                canonicalize_arrays(v);
            }
        }
        _ => {}
    }
}

/// The full enumeration of `sqry graph *` subcommands as of the live
/// `sqry graph --help` output. The drift guard test
/// `graph_subcommand_enumeration_matches_help_output` reparses the help
/// text at runtime and fails if this list and the live help disagree —
/// adding a new subcommand without touching this list breaks the build.
///
/// Per-subcommand argument selection notes:
///
/// * `parseConfig` and `useSelector` exist in the BadLiveware fixture;
///   `useSelector` calls `parseConfig`, so both `direct-callers` and
///   `direct-callees` resolve to non-empty results, and `trace-path`
///   from `useSelector` to `parseConfig` finds a length-2 path.
/// * `dependency-tree main` walks the import graph from the package
///   entry point — produces a deterministic small JSON tree against
///   BadLiveware regardless of cycle state.
/// * `cross-language` / `nodes` / `edges` / `stats` / `cycles` /
///   `complexity` need no positional argument; their default flag set
///   produces deterministic JSON against the fixture.
/// * `is-in-cycle parseConfig` returns `in_cycle: false` against
///   BadLiveware — exit 0 with a stable JSON shape, which is exactly
///   what the smoke harness needs.
const SUBCOMMAND_SMOKE_CASES: &[SmokeCase] = &[
    SmokeCase {
        subcommand: "trace-path",
        args: &["useSelector", "parseConfig"],
    },
    SmokeCase {
        subcommand: "call-chain-depth",
        args: &["useSelector"],
    },
    SmokeCase {
        subcommand: "dependency-tree",
        args: &["main"],
    },
    SmokeCase {
        subcommand: "cross-language",
        args: &[],
    },
    SmokeCase {
        subcommand: "nodes",
        args: &[],
    },
    SmokeCase {
        subcommand: "edges",
        args: &[],
    },
    SmokeCase {
        subcommand: "stats",
        args: &[],
    },
    SmokeCase {
        subcommand: "status",
        args: &[],
    },
    SmokeCase {
        subcommand: "provenance",
        args: &["parseConfig"],
    },
    SmokeCase {
        subcommand: "resolve",
        args: &["parseConfig"],
    },
    SmokeCase {
        subcommand: "cycles",
        args: &[],
    },
    SmokeCase {
        subcommand: "complexity",
        args: &[],
    },
    SmokeCase {
        subcommand: "direct-callers",
        args: &["parseConfig"],
    },
    SmokeCase {
        subcommand: "direct-callees",
        args: &["useSelector"],
    },
    SmokeCase {
        subcommand: "call-hierarchy",
        args: &["useSelector"],
    },
    SmokeCase {
        subcommand: "is-in-cycle",
        args: &["parseConfig"],
    },
];

/// Run a single invocation and return the parsed JSON value. Fails
/// loudly with stdout / stderr context so a regression points straight
/// at the failing subcommand and order.
fn run_and_parse(label: &str, args: &[&str]) -> Value {
    let output = Command::new(sqry_bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{label}: spawn failed: {e}"));
    assert!(
        output.status.success(),
        "{label}: nonzero exit ({:?}). args={args:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("{label}: expected JSON. args={args:?}\nstdout={stdout}\nparse error: {e}")
    })
}

/// Drive one `SmokeCase` through all three documented invocation
/// orders and assert every order produces the same parsed JSON
/// (after `normalize_for_comparison`).
fn run_case_three_orders(fixture_root: &Path, case: &SmokeCase) {
    let path_str = fixture_root.to_str().expect("fixture path is valid UTF-8");

    // Order 1: `sqry --json graph --path <root> <sub> [args...]`
    let mut order_1_args: Vec<&str> = vec!["--json", "graph", "--path", path_str, case.subcommand];
    order_1_args.extend_from_slice(case.args);
    let label_1 = format!("{} order 1 (`--json graph <sub>`)", case.subcommand);
    let mut parsed_1 = run_and_parse(&label_1, &order_1_args);

    // Order 2: `sqry graph --path <root> <sub> [args...] --json`
    let mut order_2_args: Vec<&str> = vec!["graph", "--path", path_str, case.subcommand];
    order_2_args.extend_from_slice(case.args);
    order_2_args.push("--json");
    let label_2 = format!("{} order 2 (`graph <sub> --json`)", case.subcommand);
    let mut parsed_2 = run_and_parse(&label_2, &order_2_args);

    // Order 3: `sqry graph --path <root> --format json <sub> [args...]`
    let mut order_3_args: Vec<&str> = vec![
        "graph",
        "--path",
        path_str,
        "--format",
        "json",
        case.subcommand,
    ];
    order_3_args.extend_from_slice(case.args);
    let label_3 = format!("{} order 3 (`graph --format json <sub>`)", case.subcommand);
    let mut parsed_3 = run_and_parse(&label_3, &order_3_args);

    normalize_for_comparison(case.subcommand, &mut parsed_1);
    normalize_for_comparison(case.subcommand, &mut parsed_2);
    normalize_for_comparison(case.subcommand, &mut parsed_3);

    assert_eq!(
        parsed_1, parsed_2,
        "{} order 1 vs order 2 JSON mismatch\norder 1: {parsed_1}\norder 2: {parsed_2}",
        case.subcommand
    );
    assert_eq!(
        parsed_2, parsed_3,
        "{} order 2 vs order 3 JSON mismatch\norder 2: {parsed_2}\norder 3: {parsed_3}",
        case.subcommand
    );
}

/// The headline smoke test: every `graph *` subcommand, all three
/// invocation orders, structural JSON equality. One fresh `.sqry/`
/// index for the whole sweep — building it 16 times would pay no
/// additional invariant since each subcommand's three orders share
/// the same on-disk snapshot anyway.
#[test]
fn every_graph_subcommand_honors_all_three_invocation_orders() {
    let temp = TempDir::new().unwrap();
    write_badliveware_fixture(temp.path());
    index(temp.path());

    for case in SUBCOMMAND_SMOKE_CASES {
        run_case_three_orders(temp.path(), case);
    }
}

/// Specifically lock the public-report reproducer from
/// verivus-oss/sqry#79: `direct-callers parseConfig` against the
/// BadLiveware Go fixture. Asserts the JSON shape `{ "symbol": ..., "callers": [...] }`
/// is identical across all three invocation orders **and** that
/// `useSelector` shows up as a caller of `parseConfig`. This is the
/// exact assertion the public bug reporter would run to confirm the
/// fix on their own checkout.
#[test]
fn badliveware_direct_callers_parse_config_matches_public_report() {
    let temp = TempDir::new().unwrap();
    write_badliveware_fixture(temp.path());
    index(temp.path());

    let case = SmokeCase {
        subcommand: "direct-callers",
        args: &["parseConfig"],
    };
    run_case_three_orders(temp.path(), &case);

    // Belt-and-braces structural assertion against one of the orders
    // so a regression that returns valid-but-empty JSON across all
    // three orders still fails this test.
    let path_str = temp.path().to_str().unwrap();
    let parsed = run_and_parse(
        "badliveware direct-callers parseConfig",
        &[
            "--json",
            "graph",
            "--path",
            path_str,
            "direct-callers",
            "parseConfig",
        ],
    );
    assert_eq!(
        parsed["symbol"], "parseConfig",
        "expected `symbol: parseConfig` in JSON, got {parsed}"
    );
    let callers = parsed["callers"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `callers` array, got {parsed}"));
    let names: Vec<&str> = callers
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        names.contains(&"useSelector"),
        "expected `useSelector` in callers list, got {names:?} (full JSON: {parsed})"
    );
}

/// Drift guard: parses `sqry graph --help` at runtime, extracts the
/// subcommand keywords from the `Commands:` block, and asserts every
/// one is present in `SUBCOMMAND_SMOKE_CASES`. Without this guard,
/// adding a new `graph *` subcommand and forgetting to thread `--json`
/// through it would silently regress the contract — exactly the
/// failure mode the DAG calls out.
///
/// The `help` pseudo-subcommand printed by clap is excluded from the
/// comparison.
#[test]
fn graph_subcommand_enumeration_matches_help_output() {
    let output = Command::new(sqry_bin())
        .args(["graph", "--help"])
        .output()
        .expect("spawn `sqry graph --help`");
    assert!(
        output.status.success(),
        "`sqry graph --help` exited nonzero: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let help_text = String::from_utf8_lossy(&output.stdout);

    // Parse the `Commands:` block. clap emits one subcommand per line,
    // indented with whitespace, in the form:
    //     "  trace-path        Find shortest path between two symbols"
    // We grab the first whitespace-delimited token of every line that
    // sits inside the `Commands:` block.
    let mut in_commands = false;
    let mut help_subcommands: BTreeSet<String> = BTreeSet::new();
    for line in help_text.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            // The block ends at the first blank line or at any line
            // that isn't indented (clap moves to `Options:` next).
            if trimmed.is_empty() || !line.starts_with(' ') {
                break;
            }
            if let Some(token) = trimmed.split_whitespace().next() {
                help_subcommands.insert(token.to_string());
            }
        }
    }
    // Drop clap's auto-injected `help` pseudo-subcommand.
    help_subcommands.remove("help");

    assert!(
        !help_subcommands.is_empty(),
        "failed to parse any subcommands out of `sqry graph --help`. \
         Help text was:\n{help_text}"
    );

    let enumerated: BTreeSet<String> = SUBCOMMAND_SMOKE_CASES
        .iter()
        .map(|c| c.subcommand.to_string())
        .collect();

    let missing_from_enumeration: Vec<&String> = help_subcommands.difference(&enumerated).collect();
    let extra_in_enumeration: Vec<&String> = enumerated.difference(&help_subcommands).collect();

    assert!(
        missing_from_enumeration.is_empty(),
        "`SUBCOMMAND_SMOKE_CASES` is missing subcommand(s) listed by \
         `sqry graph --help`: {missing_from_enumeration:?}. \
         Add a `SmokeCase` entry for each so `--json` threading is \
         covered, then update this assertion."
    );
    assert!(
        extra_in_enumeration.is_empty(),
        "`SUBCOMMAND_SMOKE_CASES` references subcommand(s) that no \
         longer appear in `sqry graph --help`: {extra_in_enumeration:?}. \
         Remove the stale entry."
    );
}
