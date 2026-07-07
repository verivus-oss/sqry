//! Integration tests for the CLI ambiguous-symbol disambiguation cluster.
//!
//! Covers three related UX defects fixed as one branch:
//!
//! * verivus-oss/sqry#512: `sqry explain` accepts `--in <FILE>` and
//!   `--line <N>`, and the ambiguity message only ever names a flag the
//!   command actually implements (never a bare `--in` on a same-file
//!   collision that `--in` cannot resolve).
//! * verivus-oss/sqry#516: `sqry visualize` resolves an ambiguous bare
//!   name to every matching definition (union), so it finds the relations
//!   `sqry graph` / `sqry query` find, instead of an edge-less node-only
//!   diagram. `--in` / `--line` narrow the root.
//! * verivus-oss/sqry#514: an `@alias` run accepts trailing flags such as
//!   `-c`, and `sqry query @name` / `sqry search @name` run the alias.

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use std::fs;
use tempfile::TempDir;

/// A Rust fixture with two collisions:
///
/// * two methods named `summary` in the *same* file (`Report::summary` at a
///   lower line, `Ledger::summary` at a higher line), the same-file case a
///   file path alone cannot disambiguate;
/// * two free functions named `resolve_it`, one with callees (`lib.rs`) and
///   one edge-less (`aaa.rs`), the ambiguous-visualize case.
fn build_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        r"pub struct Report;

impl Report {
    pub fn summary(&self) -> u32 { pick_a() }
}

pub struct Ledger;

impl Ledger {
    pub fn summary(&self) -> u32 { pick_b() }
}

pub fn resolve_it() -> u32 { pick_a() + pick_b() }

fn pick_a() -> u32 { 1 }
fn pick_b() -> u32 { 2 }

pub mod aaa;
",
    )
    .unwrap();
    fs::write(
        dir.path().join("aaa.rs"),
        "pub fn resolve_it() -> u32 { 0 }\n",
    )
    .unwrap();

    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["index", "."])
        .assert()
        .success();
    dir
}

// ---------------------------------------------------------------------------
// verivus-oss/sqry#512: explain --in / --line
// ---------------------------------------------------------------------------

#[test]
fn explain_ambiguity_message_names_line_flag_for_same_file_collision() {
    let dir = build_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["explain", "lib.rs", "summary"])
        .assert()
        .code(4);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    // Both candidates live in lib.rs, so `--in` cannot disambiguate them.
    // The message must steer the user to `--line`, a flag explain implements.
    assert!(
        stderr.contains("--line"),
        "same-file ambiguity must suggest --line, got {stderr:?}"
    );
    assert!(
        !stderr.contains("--in <file>"),
        "must not suggest --in when every candidate shares one file, got {stderr:?}"
    );
    assert!(
        stderr.contains("Candidates:"),
        "must enumerate candidates, got {stderr:?}"
    );
}

#[test]
fn explain_line_disambiguates_same_file_collision() {
    let dir = build_fixture();
    // Report::summary is defined on line 4.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["explain", "lib.rs", "summary", "--line", "4"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Report::summary"),
        "--line 4 must resolve Report::summary, got {stdout:?}"
    );
    assert!(
        !stdout.contains("Ledger::summary"),
        "--line 4 must not resolve Ledger::summary, got {stdout:?}"
    );
}

#[test]
fn explain_in_flag_overrides_positional_file_scope() {
    let dir = build_fixture();
    // `--in` overrides the (deliberately wrong) positional file, and `--line`
    // picks Ledger::summary (line 10). This proves `--in` is a real, working
    // flag on explain (the #512 headline).
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "explain",
            "does-not-exist.rs",
            "summary",
            "--in",
            "lib.rs",
            "--line",
            "10",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Ledger::summary"),
        "--in + --line must resolve Ledger::summary, got {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// verivus-oss/sqry#516: visualize ambiguous resolution (union) + narrow
// ---------------------------------------------------------------------------

#[test]
fn visualize_ambiguous_name_finds_relations() {
    let dir = build_fixture();
    // `resolve_it` matches two definitions; the lib.rs one calls pick_a/pick_b.
    // Pre-fix, visualize truncated to the first-sorted (edge-less aaa.rs) node
    // and rendered an empty diagram. Post-fix it traverses from both.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["visualize", "callees:resolve_it"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") && stdout.contains("pick_b"),
        "union resolution must surface both callees, got {stdout:?}"
    );
    assert!(
        stdout.contains("-> pick_a"),
        "diagram must contain call edges, got {stdout:?}"
    );
}

#[test]
fn visualize_in_flag_narrows_root_to_named_file() {
    let dir = build_fixture();
    // Narrowing to the edge-less aaa.rs definition yields no relations.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["visualize", "callees:resolve_it", "--in", "aaa.rs"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("pick_a") && !stdout.contains("pick_b"),
        "aaa.rs definition has no callees, got {stdout:?}"
    );
}

/// Fixture for the qualified `Type::method` parity case: `Alpha::helper` and
/// `Beta::helper` share the method segment `helper` and each call a distinct
/// callee.
fn build_qualified_method_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        r"pub struct Alpha;
impl Alpha {
    pub fn helper(&self) -> u32 { alpha_callee() }
}
pub struct Beta;
impl Beta {
    pub fn helper(&self) -> u32 { beta_callee() }
}
fn alpha_callee() -> u32 { 1 }
fn beta_callee() -> u32 { 2 }
pub fn drive() -> u32 { Alpha.helper() + Beta.helper() }
",
    )
    .unwrap();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["index", "."])
        .assert()
        .success();
    dir
}

#[test]
fn visualize_qualified_method_matches_graph_direct_callees() {
    let dir = build_qualified_method_fixture();

    // `graph direct-callees Alpha::helper` unions the same-method-segment
    // candidates (Alpha::helper and Beta::helper), returning both callees.
    let graph_out = String::from_utf8(
        Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["graph", "direct-callees", "Alpha::helper"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        graph_out.contains("alpha_callee") && graph_out.contains("beta_callee"),
        "graph baseline must union both callees, got {graph_out:?}"
    );

    // visualize must resolve the identical root set (segment-aware), so the
    // diagram carries both callees, not just the exact-qualified node's.
    let viz_out = String::from_utf8(
        Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["visualize", "callees:Alpha::helper"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        viz_out.contains("alpha_callee") && viz_out.contains("beta_callee"),
        "visualize must match graph direct-callees for a qualified name, got {viz_out:?}"
    );
}

// ---------------------------------------------------------------------------
// verivus-oss/sqry#514: alias trailing flags + `query @name` form
// ---------------------------------------------------------------------------

/// Save a search alias `@picks` (matches the pick_* helpers) in the fixture.
fn build_fixture_with_alias() -> TempDir {
    let dir = build_fixture();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["search", "pick_", ".", "--save-as", "picks"])
        .assert()
        .success();
    dir
}

#[test]
fn alias_accepts_trailing_count_flag() {
    let dir = build_fixture_with_alias();
    // `-c` is a top-level shorthand flag the `search` subcommand does not
    // accept; pre-fix the alias expanded to `search ... -c` and clap rejected
    // it with "Usage: sqry search ...". Post-fix it expands to the shorthand.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", ".", "-c"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("matches found"),
        "--count on an alias must print a match count, got {stdout:?}"
    );
}

/// Save a query alias `@qpicks` = `query "kind:$k"` with `--var k=function`, so
/// `@qpicks` expands to a structural query for every function
/// (verivus-oss/sqry#528 round 6 query-scope class members).
fn build_fixture_with_query_alias() -> TempDir {
    let dir = build_fixture();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "query",
            "kind:$k",
            "--var",
            "k=function",
            "--save-as",
            "qpicks",
        ])
        .assert()
        .success();
    dir
}

// ---------------------------------------------------------------------------
// verivus-oss/sqry#528 round 6: alias-invocation flag/path PLACEMENT class.
//
// Every prior round patched one arrangement of trailing flags / path and left
// the "path is the token right after the alias" model intact, so a new
// placement kept slipping through. These cases exercise the order-independent
// partition end to end: a subcommand-scoped flag BEFORE an explicit path, its
// `=`-joined form, a top-level flag before the path, and a bundled short
// cluster, in both the explicit-`search` and top-level-shorthand forms, plus
// the `query` equivalents. Each of these exits 2 on the round-5 tip
// (5cccaa60c) and exits 0 here.
// ---------------------------------------------------------------------------

#[test]
fn search_alias_subcommand_flag_before_explicit_path() {
    let dir = build_fixture_with_alias();
    // `sqry search @picks --cfg-filter test .`: `--cfg-filter` is a
    // `search`-only flag and the path `.` trails it. Pre-fix the scan peeked
    // only at `--cfg-filter` (the token after the alias), resolved no path,
    // synthesized a default `.`, AND swept the real `.` into the trailing
    // flags, so clap saw two path positionals and exited 2.
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["search", "@picks", "--cfg-filter", "test", "."])
        .assert()
        .success();
}

#[test]
fn search_shorthand_subcommand_flag_before_explicit_path() {
    let dir = build_fixture_with_alias();
    // Same placement via the bare shorthand form (no `search` word typed):
    // `sqry @picks --cfg-filter test .`.
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "--cfg-filter", "test", "."])
        .assert()
        .success();
}

#[test]
fn search_shorthand_equals_joined_flag_before_explicit_path() {
    let dir = build_fixture_with_alias();
    // `=`-joined value must partition identically to the space-separated form:
    // `sqry @picks --cfg-filter=test .`.
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "--cfg-filter=test", "."])
        .assert()
        .success();
}

#[test]
fn search_shorthand_top_level_flag_before_explicit_path() {
    let dir = build_fixture_with_alias();
    // `sqry @picks -c .`: a top-level flag before the path keeps the shorthand
    // form and prints a count.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "-c", "."])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("matches found"),
        "-c before the path must still print a match count, got {stdout:?}"
    );
}

#[test]
fn search_shorthand_bundled_short_cluster() {
    let dir = build_fixture_with_alias();
    // `sqry @picks -ic .`: `-ic` bundles two top-level shorts (`-i`
    // ignore_case, `-c` count). Pre-fix the whole `-ic` token was judged an
    // unknown, subcommand-scoped flag, so the run kept the `search` word and
    // expanded to `search pick_ . -ic`, which the `search` subcommand (no
    // `-i`/`-c`) rejected with exit 2. Splitting the cluster classifies both
    // members as top-level and keeps the shorthand form.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "-ic", "."])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("matches found"),
        "-ic (bundled -i -c) must classify as top-level and print a count, got {stdout:?}"
    );
}

#[test]
fn search_alias_two_paths_is_ambiguous_error() {
    let dir = build_fixture_with_alias();
    // The alias contract admits a single optional path, so two positionals is a
    // clear error (not a silently dropped argument).
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "src", "lib"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("Ambiguous alias invocation"),
        "two paths after an alias must report the ambiguity, got {stderr:?}"
    );
}

#[test]
fn query_alias_flag_before_explicit_path() {
    let dir = build_fixture_with_query_alias();
    // `sqry query @qpicks --var k=function .`: the subcommand-scoped `--var`
    // and its value precede the path `.`. The partition consumes the value and
    // keeps `.` as the sole positional.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "@qpicks", "--var", "k=function", "."])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "query @alias --var k=v . must run the substituted query, got {stdout:?}"
    );
}

#[test]
fn query_alias_equals_joined_flag_before_explicit_path() {
    let dir = build_fixture_with_query_alias();
    // `=`-joined value form: `sqry query @qpicks --var=k=function .`.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "@qpicks", "--var=k=function", "."])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "query @alias --var=k=v . must run the substituted query, got {stdout:?}"
    );
}

#[test]
fn query_alias_leading_flag_before_alias() {
    let dir = build_fixture_with_query_alias();
    // Control: the leading-flag form `sqry query --var k=function @qpicks .`
    // that earlier rounds already fixed must stay green.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "--var", "k=function", "@qpicks", "."])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "query --var k=v @alias . must run the substituted query, got {stdout:?}"
    );
}

#[test]
fn query_prefixed_alias_runs_the_alias() {
    let dir = build_fixture_with_alias();
    // `sqry query @picks` used to reach the planner, which choked on '@'.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "@picks"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "query @alias must run the stored search, got {stdout:?}"
    );
}

#[test]
fn search_prefixed_alias_runs_the_alias() {
    let dir = build_fixture_with_alias();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["search", "@picks"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "search @alias must run the stored search, got {stdout:?}"
    );
}

#[test]
fn query_prefixed_alias_with_intervening_flag_runs_the_alias() {
    let dir = build_fixture_with_alias();
    // `sqry query --json @picks`: a global flag between the prefix word and the
    // `@alias` must not derail alias recognition (verivus-oss/sqry#514). The
    // `--json` also proves the flag still applies to the run.
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "--json", "@picks"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_") && stdout.trim_start().starts_with('{'),
        "query --json @alias must run the alias as JSON, got {stdout:?}"
    );
}

#[test]
fn search_prefixed_alias_with_intervening_flag_runs_the_alias() {
    let dir = build_fixture_with_alias();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["search", "--json", "@picks"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_") && stdout.trim_start().starts_with('{'),
        "search --json @alias must run the alias as JSON, got {stdout:?}"
    );
}

#[test]
fn query_alias_with_value_taking_flag_runs_the_alias() {
    // `sqry query --var k=function @qfuncs`: `--var` is a value-taking `query`
    // flag. Pre-fix, the alias scan advanced only one token past `--var`,
    // landed on the value `k=function`, treated it as a positional, and never
    // expanded the alias (verivus-oss/sqry#514). Now `--var` is in the
    // value-flag table, so the scan skips its value, finds `@qfuncs`, and the
    // expansion keeps `--var` after the `query` word where it applies.
    let dir = build_fixture();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "query",
            "kind:$k",
            "--var",
            "k=function",
            "--save-as",
            "qfuncs",
        ])
        .assert()
        .success();

    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["query", "--var", "k=function", "@qfuncs"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("pick_a") || stdout.contains("pick_b"),
        "query --var k=v @alias must run the alias with the substitution, got {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// verivus-oss/sqry#528 round 7: `--` end-of-options in alias invocation.
//
// `partition_alias_tail` had no `--` handling: a bare `--` was itself
// classified as an unknown flag token, so an escaped hyphen-leading path
// (`sqry @picks -- -5`) was swept into the trailing flags and clap rejected
// it at the PARSE level ("unexpected argument '-5' found", exit 2). The
// direct `sqry search "<pattern>" -- -5` form parses fine (clap's own `--`
// escape takes over) and only fails later on the runtime path check, so the
// alias form must reach that same state, not diverge at parse time.
// ---------------------------------------------------------------------------

/// Save a GLOBAL-scope alias, isolated to `config_dir` via `SQRY_CONFIG_DIR`.
///
/// The alias-index lookup path `expand_alias_args` uses to find a LOCAL alias
/// is derived from the resolved search path itself; once that path is a
/// nonexistent, hyphen-leading string like `-5` (exactly the case this round
/// fixes at the parse level), a LOCAL alias would fail to resolve for an
/// unrelated, pre-existing reason (the lookup path no longer points at this
/// fixture's `.sqry` directory). A GLOBAL alias sidesteps that: global lookup
/// does not depend on the resolved search path at all, so these tests isolate
/// the `--` parse/re-emit fix from that separate lookup-path behavior.
fn build_fixture_with_global_alias(config_dir: &std::path::Path) -> TempDir {
    let dir = build_fixture();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .env("SQRY_CONFIG_DIR", config_dir)
        .args(["search", "pick_", ".", "--save-as", "gpicks", "--global"])
        .assert()
        .success();
    dir
}

#[test]
fn alias_double_dash_hyphen_path_parses() {
    let config_dir = TempDir::new().unwrap();
    let dir = build_fixture_with_global_alias(config_dir.path());

    // Direct form: clap's own `--` end-of-options escape makes `-5` the PATH
    // positional, so this fails at runtime (the path does not exist), not at
    // the clap parse level.
    let direct = Command::new(sqry_bin())
        .current_dir(&dir)
        .env("SQRY_CONFIG_DIR", config_dir.path())
        .args(["search", "pick_", "--", "-5"])
        .assert()
        .failure();
    let direct_stderr = String::from_utf8(direct.get_output().stderr.clone()).unwrap();

    // Alias form must reach the identical state: PARSE SUCCESS (the `--`
    // marker survives the alias-tail partition and is re-emitted ahead of the
    // escaped path), then the same runtime path-check failure.
    let alias = Command::new(sqry_bin())
        .current_dir(&dir)
        .env("SQRY_CONFIG_DIR", config_dir.path())
        .args(["@gpicks", "--", "-5"])
        .assert()
        .failure();
    let alias_stderr = String::from_utf8(alias.get_output().stderr.clone()).unwrap();

    assert!(
        !alias_stderr.contains("unexpected argument"),
        "alias form must not hit the clap parse-level rejection the round-6 tip did, \
         got {alias_stderr:?}"
    );
    assert_eq!(
        alias_stderr, direct_stderr,
        "alias and direct forms must fail identically once past the parse stage"
    );
}

#[test]
fn alias_double_dash_two_paths_after_alias_is_ambiguous() {
    let dir = build_fixture_with_alias();
    // `sqry @picks -- a b`: two positionals after the `--` escape is still an
    // alias-invocation ambiguity, the same class of error as two positionals
    // without one (`search_alias_two_paths_is_ambiguous_error` above).
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["@picks", "--", "a", "b"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("Ambiguous alias invocation"),
        "two paths after -- must report the ambiguity, got {stderr:?}"
    );
}

/// A JavaScript fixture with TWO methods that each intern with a `/`
/// (object-literal string keys, the `semantic_name_for_node_input` slash
/// short-circuit shape) sharing the trailing method segment `fetchUsers` but
/// with otherwise unrelated qualifiers (`api.js` vs `other.js`), each calling
/// a distinct callee.
///
/// This is the genuine divergence case: `segments_match` requires one name to
/// be a suffix of the other at a separator boundary, and neither
/// `"frontend/api.js::fetchUsers"` nor `"frontend/other.js::fetchUsers"` is a
/// suffix of the other (they diverge before the shared `::fetchUsers` tail),
/// so plain segment matching cannot union them. Only the trailing-method-name
/// fallback (`extract_method_name` peeling `fetchUsers` off both) unions the
/// second definition in, mirroring the unit test
/// `resolve_call_roots_matches_path_qualified_name_via_method_segment`
/// end-to-end through the real CLI dispatch instead of a hand-built graph.
fn build_slash_method_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("o.js"),
        r#"function doFetch() { return 1; }
function otherFetch() { return 2; }
const api = {
  "frontend/api.js::fetchUsers"() { return doFetch(); },
};
const other = {
  "frontend/other.js::fetchUsers"() { return otherFetch(); },
};
"#,
    )
    .unwrap();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["index", "."])
        .assert()
        .success();
    dir
}

#[test]
fn visualize_slash_method_matches_graph_direct_callees() {
    let dir = build_slash_method_fixture();
    let target = "callees:\"frontend/api.js::fetchUsers\"";
    let symbol = "\"frontend/api.js::fetchUsers\"";

    // Baseline: graph direct-callees queries only the exact name of the FIRST
    // slash-named method, yet unions in the SECOND method's callee too,
    // because sqry-db's `method_segment_matches` fallback treats any node
    // sharing the trailing `fetchUsers` segment as a match for the query
    // (verivus-oss/sqry#516). If this fallback were absent, the baseline
    // itself would only report `doFetch`, so this assertion also locks the
    // sqry-db side of the union, not just the visualize side.
    let graph_out = String::from_utf8(
        Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["graph", "direct-callees", symbol])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        graph_out.contains("doFetch") && graph_out.contains("otherFetch"),
        "graph direct-callees baseline must union both slash-named callables via the \
         method-segment fallback, got {graph_out:?}"
    );

    // visualize must resolve the identical root set. Without the
    // `method_match` fallback in `resolve_call_relation_roots`,
    // `"frontend/other.js::fetchUsers"` would never become a traversal root
    // (its own name does not segment-match the queried
    // `"frontend/api.js::fetchUsers"`), so the diagram would carry only
    // `doFetch` and silently diverge from the graph baseline above
    // (verivus-oss/sqry#516).
    let viz_out = String::from_utf8(
        Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["visualize", target])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        viz_out.contains("doFetch") && viz_out.contains("otherFetch"),
        "visualize must match graph direct-callees for both slash-named callables, \
         got {viz_out:?}"
    );
}
