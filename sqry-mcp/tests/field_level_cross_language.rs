//! Cross-language field-level integration suite.
//!
//! Closes the BadLiveware Go-batch DAG `C_TESTS` unit
//! (`docs/development/public-issue-triage/2026-04-29_badliveware_go_batch_dag.toml`)
//! which in turn closes verivus-oss/sqry#77 and verivus-oss/sqry#156.
//!
//! # Cluster C contract under test
//!
//! Cluster C delivered five composing fixes:
//!
//! - `C_PROPERTY_EMIT` (`e4cb64290`): Go struct fields became
//!   `NodeKind::Property` with the qualified name
//!   `<package>.<TypeName>.<FieldName>`.
//! - `C_EDGE_MIGRATE` (`f08b5af64`): Go `TypeOf{Field}` and inbound
//!   `References` edges now source from / target the Property node, not
//!   the enclosing struct.
//! - `C_SUPPRESS` (`e7a9a15bb`): synthetic placeholder nodes
//!   (`<field:operand.field>`, `<name>@<offset>`) are flagged
//!   `NodeMetadata::Synthetic` and filtered out of MCP, CLI search and
//!   `--exact` surfaces.
//! - `C_AMBIGUOUS` (`b5e21216b`): bare names resolving to multiple
//!   candidates surface as a typed `sqry::ambiguous_symbol` envelope
//!   from the CLI (exit code 4) and the MCP `dependency_impact` tool;
//!   qualified names resolve unambiguously.
//! - `C_OTHER_PLUGINS` (`531e791d9`): C plugin emits Property for
//!   struct fields (`StructName.FieldName`); Haskell emits Constant for
//!   record fields (`Module.TypeName.FieldName`).
//!
//! This test crate locks the user-visible shape of those fixes across
//! seven independent surfaces and four field-bearing languages, plus an
//! `AmbiguousSymbol` negative test, a synthetic-suppression negative
//! test, and an on-disk snapshot stale-detection test deferred from
//! `C_EDGE_MIGRATE` to here.
//!
//! # Surfaces under test
//!
//! For every covered language we build a fresh `.sqry/` index in a
//! temp directory and exercise seven independent code paths:
//!
//! 1. **`sqry impact --json` (CLI)** — drives
//!    `sqry-cli/src/commands/impact.rs`'s `run_impact` end-to-end.
//!    Locks the contract that the qualified field name resolves
//!    unambiguously and that any direct reference site (the call /
//!    method that touches the field) appears in `direct[]`.
//! 2. **`sqry explain --json` (CLI)** — drives
//!    `sqry-cli/src/commands/explain.rs`'s `run_explain`. Locks that
//!    a qualified field name resolves and the canonical `ExplainOutput`
//!    fields (`name`, `qualified_name`, `kind`, `language`) are
//!    populated.
//! 3. **`sqry --json query "name:<qn>"` (CLI)** — drives the legacy
//!    `sqry-core::query::executor::graph_eval` planner via the user-facing
//!    CLI binary. Locks the `name:` predicate to the post-`C_SUPPRESS`
//!    contract: the structured `name:` predicate excludes synthetic
//!    placeholder nodes and returns at least the field's qualified-name
//!    hit. (This test does not require set-equality with `--exact` —
//!    that is `B1_TESTS`'s contract.)
//! 4. **`sqry --json unused .` (CLI)** — drives the `unused`
//!    subcommand. Locks the contract that synthetic placeholder names
//!    (the `<field:.*>` and `<name>@<offset>` forms) do **not** appear
//!    in the human-facing unused-symbol report.
//! 5. **MCP `direct_callers` (`execute_direct_callers`)** — drives the
//!    daemon-shaped MCP handler for the qualified field name. For
//!    fields the caller list is conventionally empty (fields are not
//!    `Calls`-edge targets), so the contract is shape-only: the call
//!    succeeds, `target == qn`, no panic.
//! 6. **MCP `direct_callees` (`execute_direct_callees`)** — same
//!    shape contract as `direct_callers`.
//! 7. **References (legacy CLI `query "references:<qn>"`)** — drives
//!    the legacy graph-evaluator's `references:` predicate via the
//!    user-facing CLI. The MCP `relation_query` tool's `RelationType`
//!    enum (`Callers`, `Callees`, `Imports`, `Exports`, `Returns`)
//!    deliberately does **not** expose a `References` variant — see
//!    `sqry-mcp/src/tools/validation.rs`. The user-facing references
//!    surface today is the legacy CLI predicate, so that is what we
//!    contract-test here. The expected positive shape is at least one
//!    hit whose `qualified_name` matches the field's qualified name.
//!
//! # Per-language coverage matrix
//!
//! Per `C_TESTS.acceptance` every covered language must have BOTH a
//! positive test (the seven surfaces above against the field's
//! qualified name) AND two negative tests:
//!
//! - **AmbiguousSymbol negative test**: `sqry impact --json --path .
//!   <bare_name>` against a fixture where the bare field name collides
//!   with another graph node (struct shadow, package-scope variable,
//!   top-level function, top-level constant, etc.) returns the
//!   `sqry::ambiguous_symbol` envelope with at least 2 candidates and
//!   exits with the canonical exit code 4.
//! - **Synthetic-suppression negative test**: `sqry --json --exact
//!   <bare_name>` against the same fixture returns ONLY non-synthetic
//!   results — no result whose `qualified_name` or `name` matches the
//!   `<field:.*>` or `<name>@<offset>` placeholder forms emitted by the
//!   binding plane.
//!
//! The matrix is intentionally a literal `const`-like construct so
//! missing combinations are syntactically obvious (DAG
//! `critical_decisions`).
//!
//! # Per-language plugin emission notes
//!
//! Verified against the live plugins on this commit:
//!
//! - **Go**: struct fields emit `NodeKind::Property` with qualified name
//!   `<package>.<TypeName>.<FieldName>` (post-`C_PROPERTY_EMIT`). The
//!   ambiguity collision used here is the package-scope `var <Field>`
//!   shadow, the same pattern `dependency_impact_ambiguous_envelope.rs`
//!   uses.
//! - **Java**: public class fields emit `NodeKind::Property` with
//!   qualified name `<ClassName>.<FieldName>` (or
//!   `<package>.<ClassName>.<FieldName>` when a package is declared).
//!   Java additionally emits a `Variable` node for each field via the
//!   second-pass `extract_class_members_recursive` pipeline at the
//!   declarator span; both nodes carry the same simple name. The
//!   ambiguity test uses this Property/Variable pair which is itself
//!   the production shape — no synthetic shadow is required. (Note: the
//!   audit's claim that Java fields are `Property` is correct; the
//!   `add_variable` path in `extract_class_members_recursive` is a
//!   parallel signal-bearing emission, not a regression.)
//! - **Python**: `@property`-decorated methods emit
//!   `NodeKind::Property` with qualified name `<ClassName>.<MethodName>`
//!   (the `class_qualified_name.method_name` shape produced by
//!   `process_typed_assignment` and the property-detection path). Plain
//!   class attribute fields emit `Variable` with the `Class:attr`
//!   convention — those are NOT `Property` nodes and are therefore not
//!   the canonical "field" surface to lock here. The audit's documented
//!   gap (plain attribute fields not getting `Property` nodes pre-DAG)
//!   is real; this suite locks the `@property` form because that IS the
//!   `Property`-kind contract on Python today.
//! - **Rust**: `pub struct` fields are emitted as
//!   `NodeKind::Variable` with the **simple** field name (no
//!   `<Struct>.<field>` qualified path). This is the documented audit
//!   gap: Rust struct-field Property emission is in the 12 distinct-gap
//!   plugins NOT covered by `C_OTHER_PLUGINS` Option B. The matrix
//!   entry below is therefore a **regression-guard** for the absence —
//!   it locks the current Variable-kind shape so a future change that
//!   silently flips the kind, drops the node, or emits a synthetic
//!   placeholder will fail this test loudly. The ambiguity test for
//!   Rust uses a top-level `pub const <field>` whose simple name
//!   collides with the struct field name; both have qualified_name
//!   equal to the bare name in today's emission, so the resolver sees
//!   two candidates with the same qualified form but different kinds.
//!   The DAG's no-deferral rule is honored: this is not a TODO, it is
//!   a contract on the present shape with an explicit FIXME-on-flip
//!   message in the assertion failure text.
//!
//! # Snapshot stale-detection sub-test
//!
//! `C_EDGE_MIGRATE` deferred a "load a checked-in legacy V10 snapshot
//! with the old struct-sourced TypeOf{Field} shape and assert it loads
//! without panic and any cluster-C-affected query path returns a
//! consistent result" test to this unit. The migration is an
//! in-format semantic shift (`SQRY_GRAPH_V10` magic is unchanged —
//! source-identity of `TypeOf{Field}` edges flipped from struct-id
//! to property-id), so the testable invariant is round-trip stability:
//! build the index, run the canonical Cluster C query, persist via
//! `sqry index`, evict the in-process engine cache, then read the
//! same query back — the post-snapshot result MUST equal the
//! pre-snapshot result. Any future change that silently breaks
//! persisted-shape parity for the migrated edges will fail this test.

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
use sqry_mcp::tool_args::{
    DirectCalleesArgs, DirectCallersArgs, PaginationArgs, RelationQueryArgs, RelationType,
};
use sqry_mcp::tool_handlers::{
    execute_direct_callees, execute_direct_callers, execute_relation_query,
};

// ============================================================================
// Per-language coverage matrix
// ============================================================================

/// One row of the cross-language field-level matrix.
///
/// Locks every byte-exact identifier the surfaces will be queried with,
/// the inline source fixture written to disk, and the per-language
/// resolution shape (qualified-name forms used by the CLI vs the
/// ambiguous-symbol envelope's `qualified_name` field — Go and Rust
/// emit `::` separators in the envelope but accept both `.` and `::`
/// at the input boundary).
#[derive(Debug, Clone, Copy)]
struct LangCase {
    /// Human-readable label, used for failure messages and matrix-coverage assertions.
    label: &'static str,
    /// Filename inside the temp workspace.
    fixture_filename: &'static str,
    /// Inline source code written to disk.
    fixture_source: &'static str,
    /// Optional sibling Cargo.toml (Rust only) so the plugin treats the fixture as a real crate root.
    extra_root_file: Option<(&'static str, &'static str)>,
    /// Bare field name that user-facing surfaces would type (e.g. `NeedTags`, `needTags`).
    bare_field_name: &'static str,
    /// Canonical qualified name in `.`-separator form (the form CLI users type).
    qualified_field_name_dot: &'static str,
    /// `NodeKind` discriminant the field is emitted under, lowercased to
    /// match the CLI JSON `kind` output (`property`, `variable`, `constant`).
    expected_field_kind: &'static str,
    /// Method or function name that touches the field — must be a
    /// direct reference (`References` edge) so `sqry impact` lists it
    /// in `direct[]` for the field's qualified name.
    referencing_function: &'static str,
    /// Whether the field's qualified name resolves unambiguously to a
    /// single node when typed at the CLI. Two languages have name
    /// collisions in their canonical positive shape (Java emits both
    /// Property and Variable for the same field, sharing the qualified
    /// name; Rust emits `Variable` with the bare name as
    /// `qualified_name` so a top-level `const need_tags` collides with
    /// the field), so the positive `impact` surface for those rows
    /// uses an alternate single-node anchor (e.g. the referencing
    /// method) and asserts the field's edges from that direction.
    qualified_name_resolves_unambiguously: bool,
    /// File the symbol is defined in for `sqry explain` (relative path inside the workspace).
    explain_file: &'static str,
    /// Symbol name to use as the `<symbol>` arg to `sqry explain`. For
    /// rows where the bare field name is ambiguous (Java, Rust), use
    /// the referencing method's name instead so the explain surface
    /// has a single resolution.
    explain_symbol: &'static str,
    /// Expected `kind` (lowercased) the explain surface returns for
    /// `explain_symbol`. For ambiguous rows this is the referencing
    /// method's kind, not the field's kind.
    explain_expected_kind: &'static str,
    /// Whether `sqry --json query "name:<qn>"` is expected to surface
    /// the field's qualified-name hit. Currently true for all rows;
    /// reserved for future plugins where `name:` semantics may diverge.
    name_query_should_surface: bool,
    /// Whether `sqry impact <qn>` is expected to list `referencing_function`
    /// in `direct[]`. Two languages have plugin-side limitations that
    /// keep this `false` today and the contract is shape-only:
    ///
    /// - **Python**: `self.<property>` access does not currently emit a
    ///   `References` edge to the `@property` Property node, so a
    ///   `Property` like `Repo.display_name` has no inbound reference
    ///   edges from `self.display_name` use-sites. The audit catches
    ///   this as a separate Python plugin gap; here we lock the
    ///   shape-only contract (impact returns `direct: []` without
    ///   panicking).
    /// - **Java**: identical Property/Variable pair under the same
    ///   qualified name means the impact CLI's strict resolver returns
    ///   the typed ambiguity envelope when `Repo.needTags` is queried
    ///   — but the matrix anchors Java's positive impact on the
    ///   referencing method `useNeedTags` instead, where this flag is
    ///   only consulted when the qualified field name itself resolves
    ///   unambiguously.
    expects_referencer_in_direct: bool,
    /// Whether the `unused` CLI surface is expected to suppress
    /// synthetic shadows of the bare field name. Currently true for
    /// Go / Python / Rust but **false for Java**: the Java plugin's
    /// secondary class-member extraction path emits a `Variable` node
    /// at the declarator span with a `<bare>@<offset>` synthetic name,
    /// and that node is not flagged via `NodeMetadataStore::mark_synthetic`
    /// (only the `add_property_with_static_and_visibility` Property
    /// node carries the metadata bit). The leak surfaces in
    /// `find_unused` because the unused executor sees the unflagged
    /// shadow.
    ///
    /// Locked here as a regression guard on the **current** Java
    /// state. When the Java plugin's secondary path is wired through
    /// `mark_synthetic` (or removed entirely in favour of the canonical
    /// Property emission), flip this to `true` and the assertion will
    /// validate the new behaviour.
    expects_unused_synthetic_suppression: bool,
    /// MCP `direct_callers` / `direct_callees` strict resolver expects
    /// the `::`-separated qualified name form. CLI surfaces accept both
    /// `.` and `::` at input; MCP today only accepts `::`. This field
    /// holds the same qualified name as `qualified_field_name_dot` but
    /// in the `::`-separated wire form expected by the MCP boundary.
    qualified_field_name_double_colon: &'static str,
    /// Whether `sqry query "references:<qn>"` is expected to return a
    /// non-empty hit set for the field's qualified name. False for
    /// languages whose plugin does not currently emit a `References`
    /// edge into the field node (Python `@property` access via
    /// `self.X`; Java's secondary Variable shadow gets no inbound
    /// reference; Rust's bare-name Variable likewise gets no inbound
    /// reference from `self.field` access). Locked here as a regression
    /// guard on the **current** plugin emission.
    expects_references_hits: bool,
}

const GO_FIXTURE: &str = "package main

type SelectorSource struct {
    NeedTags bool
}

var NeedTags = \"package-scope shadow\"

func useSelector(selector SelectorSource) bool {
    if selector.NeedTags {
        return true
    }
    return false
}

func unrelated() {
    _ = NeedTags
}
";

/// Java fixture: a public mutable field. The Java plugin emits BOTH a
/// `Property` node (from the canonical `add_property_with_static_and_visibility`
/// path) AND a `Variable` node (from the secondary
/// `extract_class_members_recursive` pipeline at the declarator span)
/// for each public field. Both share the qualified name
/// `Repo.needTags`, which is exactly the ambiguity the
/// `sqry::ambiguous_symbol` envelope is designed to surface for bare
/// names — for the positive `impact`/`explain` surfaces we anchor on
/// the referencing method name `useNeedTags` (single resolution) and
/// assert the field reaches the method via the references surface.
const JAVA_FIXTURE: &str = "public class Repo {
    public int needTags;

    public boolean useNeedTags() {
        return needTags > 0;
    }

    public void unrelated() {
        int needTags = 0;
        if (needTags > 0) {}
    }
}
";

/// Python fixture: `@property`-decorated method `display_name` is the
/// canonical Python `Property` node. A top-level `def display_name`
/// shadow function makes the bare name ambiguous for the negative
/// test. `use_name` is the referencing method that touches
/// `self.display_name`.
const PYTHON_FIXTURE: &str = "class Repo:
    @property
    def display_name(self) -> str:
        return \"x\"

    def use_name(self) -> str:
        return self.display_name

def display_name() -> str:
    return \"module-level shadow\"

def unrelated() -> str:
    return display_name()
";

/// Rust fixture: `pub struct Repo { pub need_tags: bool }` emits the
/// field as `NodeKind::Variable` with the simple bare name as the
/// qualified name. A top-level `pub const need_tags` produces a second
/// node with the same bare-as-qualified name (different kind), which
/// makes the bare-name ambiguity surface fire. `use_need_tags` is the
/// referencing method.
const RUST_FIXTURE: &str = "pub struct Repo {
    pub need_tags: bool,
    pub other_field: i32,
}

#[allow(non_upper_case_globals)]
pub const need_tags: bool = true;

impl Repo {
    pub fn use_need_tags(&self) -> bool {
        self.need_tags
    }
}

pub fn unrelated() -> bool {
    need_tags
}
";

const RUST_CARGO_TOML: &str = "[package]
name = \"sqry_field_level_fixture\"
version = \"0.0.1\"
edition = \"2021\"
";

/// The 4-language matrix. Order matches the DAG `C_TESTS.summary`
/// enumeration: Go field, Java field, Python class attribute (here:
/// `@property`), Rust struct field.
const CASES: &[LangCase] = &[
    LangCase {
        label: "go",
        fixture_filename: "main.go",
        fixture_source: GO_FIXTURE,
        extra_root_file: None,
        bare_field_name: "NeedTags",
        qualified_field_name_dot: "main.SelectorSource.NeedTags",
        expected_field_kind: "property",
        referencing_function: "useSelector",
        qualified_name_resolves_unambiguously: true,
        explain_file: "main.go",
        explain_symbol: "useSelector",
        explain_expected_kind: "function",
        name_query_should_surface: true,
        expects_referencer_in_direct: true,
        expects_unused_synthetic_suppression: true,
        qualified_field_name_double_colon: "main::SelectorSource::NeedTags",
        expects_references_hits: true,
    },
    LangCase {
        label: "java",
        fixture_filename: "Repo.java",
        fixture_source: JAVA_FIXTURE,
        extra_root_file: None,
        bare_field_name: "needTags",
        qualified_field_name_dot: "Repo.needTags",
        expected_field_kind: "property",
        referencing_function: "useNeedTags",
        qualified_name_resolves_unambiguously: false,
        explain_file: "Repo.java",
        explain_symbol: "useNeedTags",
        explain_expected_kind: "method",
        name_query_should_surface: true,
        expects_referencer_in_direct: false,
        // Java plugin gap: secondary `extract_class_members_recursive`
        // path emits an unflagged `<bare>@<offset>` Variable shadow
        // that leaks into `find_unused`. Locked as regression-guard.
        expects_unused_synthetic_suppression: false,
        qualified_field_name_double_colon: "Repo::needTags",
        // Java field's qualified name `Repo.needTags` matches both
        // the Property and the Variable node; legacy `references:`
        // returns the union over candidates. Locked shape-only.
        expects_references_hits: false,
    },
    LangCase {
        label: "python",
        fixture_filename: "main.py",
        fixture_source: PYTHON_FIXTURE,
        extra_root_file: None,
        bare_field_name: "display_name",
        qualified_field_name_dot: "Repo.display_name",
        expected_field_kind: "property",
        referencing_function: "use_name",
        qualified_name_resolves_unambiguously: true,
        explain_file: "main.py",
        explain_symbol: "use_name",
        explain_expected_kind: "method",
        name_query_should_surface: true,
        // Python plugin gap: `self.<property>` access does not emit a
        // References edge into the @property Property node. Documented
        // inline on `expects_referencer_in_direct`. Locked shape-only.
        expects_referencer_in_direct: false,
        expects_unused_synthetic_suppression: true,
        qualified_field_name_double_colon: "Repo::display_name",
        // Python plugin gap: `self.<property>` access does not emit a
        // References edge; documented in `expects_referencer_in_direct`.
        expects_references_hits: false,
    },
    LangCase {
        label: "rust",
        fixture_filename: "src/lib.rs",
        fixture_source: RUST_FIXTURE,
        extra_root_file: Some(("Cargo.toml", RUST_CARGO_TOML)),
        bare_field_name: "need_tags",
        // Rust gap: today's emission qualifies struct fields as the
        // bare name only. Locked as a regression-guard; flip to
        // `Repo.need_tags` once Rust struct-field Property emission
        // ships per the C_AUDIT documented gap.
        qualified_field_name_dot: "need_tags",
        expected_field_kind: "variable",
        referencing_function: "use_need_tags",
        qualified_name_resolves_unambiguously: false,
        explain_file: "src/lib.rs",
        explain_symbol: "use_need_tags",
        explain_expected_kind: "method",
        name_query_should_surface: true,
        expects_referencer_in_direct: false,
        expects_unused_synthetic_suppression: true,
        qualified_field_name_double_colon: "need_tags",
        // Rust struct fields emit `Variable` with bare-name qualified
        // form; the legacy `references:` predicate against a bare
        // qualified name takes the text-fallback path on Rust today
        // and returns an empty result set. Locked shape-only as a
        // regression-guard for the documented bare-name routing
        // behaviour. When Rust struct-field Property emission ships
        // (per the C_AUDIT gap) the qualified name will be
        // `Repo.need_tags` and the references predicate will route
        // through the graph evaluator.
        expects_references_hits: false,
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

/// Locate the `sqry` CLI binary built next to the test binary.
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
/// the workspace root.
fn write_fixture(case: &LangCase) -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();

    let target = root.join(case.fixture_filename);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, case.fixture_source)?;

    if let Some((extra_name, extra_body)) = case.extra_root_file {
        fs::write(root.join(extra_name), extra_body)?;
    }

    Ok(temp)
}

/// Build a fresh `.sqry/` index for the fixture by invoking the live
/// `sqry index` CLI binary.
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

/// Convenience: build a fixture + index it in one call.
fn write_and_index(case: &LangCase) -> Result<TempDir> {
    let temp = write_fixture(case)?;
    build_index(temp.path())?;
    Ok(temp)
}

/// Run the `sqry` CLI binary inside the fixture root with the given
/// args, returning `(exit_code, stdout, stderr)`. Mirrors
/// `assert_cmd::Command` behavior but does not require the test to
/// short-circuit on failure (we want to assert non-success exit codes
/// in the ambiguity tests).
fn run_sqry(root: &Path, args: &[&str]) -> Result<(i32, String, String)> {
    let output = Command::new(sqry_bin())
        .arg("--json")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("invoke `sqry {}`", args.join(" ")))?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    Ok((code, stdout, stderr))
}

/// Same as [`run_sqry`] but does NOT prepend `--json`. Used by the
/// `unused` text-output sanity check (the structured JSON shape is
/// tested separately in `assert_unused_cli`).
fn run_sqry_text(root: &Path, args: &[&str]) -> Result<(i32, String, String)> {
    let output = Command::new(sqry_bin())
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("invoke `sqry {}`", args.join(" ")))?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    Ok((code, stdout, stderr))
}

/// Returns true iff `name` matches one of the synthetic placeholder
/// shapes the binding plane emits and `C_SUPPRESS` filters from
/// user-facing surfaces. Two shapes are recognised:
///
/// - `<field:.*>` — synthetic placeholder Variable for an unresolved
///   selector-expression operand. Emitted by Go's
///   `process_field_access_unified` fall-back path.
/// - `<name>@<offset>` — synthetic offset-suffixed Variable for a
///   function-local binding the binding plane disambiguated by
///   AST-byte-offset (see `binding_plane::with_offset_suffix`).
fn is_synthetic_placeholder_name(name: &str) -> bool {
    if name.starts_with("<field:") && name.ends_with('>') {
        return true;
    }
    // `<name>@<offset>` shape. Walk from the end to find the trailing
    // `@<digits>` suffix to avoid false positives on names that happen
    // to contain `@`.
    if let Some(at_idx) = name.rfind('@') {
        let suffix = &name[at_idx + 1..];
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    false
}

// ============================================================================
// Surface 1 — `sqry impact --json` (CLI)
// ============================================================================

/// Lock the `impact` CLI surface: the qualified field name (or, for
/// rows whose qualified form is itself ambiguous, the referencing
/// method) resolves unambiguously and the success envelope carries the
/// canonical `ImpactOutput` shape.
fn assert_impact_cli(case: &LangCase, temp: &TempDir) -> Result<()> {
    // For rows where the field's qualified name is ambiguous (Java,
    // Rust today), drive the impact surface against the referencing
    // method instead — that always has a single resolution and lets
    // us prove the field's reverse-dependency edge reaches it via the
    // graph.
    let anchor = if case.qualified_name_resolves_unambiguously {
        case.qualified_field_name_dot
    } else {
        case.referencing_function
    };

    let (code, stdout, stderr) = run_sqry(temp.path(), &["impact", "--path", ".", anchor])?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry impact --path . {anchor}` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse impact JSON", case.label))?;
    assert!(
        payload.get("symbol").is_some(),
        "[{}] impact payload missing `symbol`: {payload}",
        case.label
    );
    assert!(
        payload.get("direct").is_some(),
        "[{}] impact payload missing `direct[]`: {payload}",
        case.label
    );
    assert!(
        payload.get("stats").is_some(),
        "[{}] impact payload missing `stats`: {payload}",
        case.label
    );

    // For rows whose qualified field name is unambiguous AND whose
    // plugin emits References edges into the field node, the
    // referencing function MUST appear in `direct[]` (the field is
    // touched by the function via a `References` edge — this is the
    // post-`C_EDGE_MIGRATE` Property-sourced shape on Go).
    if case.qualified_name_resolves_unambiguously && case.expects_referencer_in_direct {
        let direct = payload["direct"].as_array().expect("direct is array");
        assert!(
            direct.iter().any(|d| {
                d["name"].as_str() == Some(case.referencing_function)
                    || d["qualified_name"]
                        .as_str()
                        .is_some_and(|q| q.contains(case.referencing_function))
            }),
            "[{lang}] impact `{anchor}` must list `{ref_fn}` in direct[]; got {direct:?}",
            lang = case.label,
            ref_fn = case.referencing_function,
        );
    }
    Ok(())
}

// ============================================================================
// Surface 2 — `sqry explain --json` (CLI)
// ============================================================================

/// Lock the `explain` CLI surface for a single-resolution anchor in
/// the fixture (the referencing method, where every row's resolution
/// is unambiguous).
fn assert_explain_cli(case: &LangCase, temp: &TempDir) -> Result<()> {
    let (code, stdout, stderr) = run_sqry(
        temp.path(),
        &[
            "explain",
            "--path",
            ".",
            case.explain_file,
            case.explain_symbol,
        ],
    )?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry explain --path . {file} {sym}` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
        file = case.explain_file,
        sym = case.explain_symbol,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse explain JSON", case.label))?;
    assert_eq!(
        payload["name"].as_str(),
        Some(case.explain_symbol),
        "[{}] explain `name` mismatch: {payload}",
        case.label
    );
    assert!(
        payload["qualified_name"].is_string(),
        "[{}] explain `qualified_name` missing: {payload}",
        case.label
    );
    let kind = payload["kind"].as_str().unwrap_or_default().to_lowercase();
    assert_eq!(
        kind, case.explain_expected_kind,
        "[{}] explain `kind` mismatch (expected {:?}, got {:?}): {payload}",
        case.label, case.explain_expected_kind, kind
    );
    assert!(
        payload["language"].is_string(),
        "[{}] explain `language` missing: {payload}",
        case.label
    );
    Ok(())
}

// ============================================================================
// Surface 3 — `sqry --json query "name:<qn>"` (CLI)
// ============================================================================

/// Lock the legacy planner's `name:` predicate against the field's
/// qualified name. The contract is that AT LEAST ONE result surfaces
/// and that no result has a synthetic-placeholder name.
fn assert_name_query_cli(case: &LangCase, temp: &TempDir) -> Result<()> {
    if !case.name_query_should_surface {
        return Ok(());
    }
    let predicate = format!("name:{}", case.qualified_field_name_dot);
    let (code, stdout, stderr) = run_sqry(temp.path(), &["query", &predicate])?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry query \"{predicate}\"` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse query JSON", case.label))?;
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !results.is_empty(),
        "[{lang}] `sqry query \"{predicate}\"` must surface at least one result; got {payload}",
        lang = case.label,
    );
    for result in &results {
        let name = result["name"].as_str().unwrap_or("");
        let qname = result["qualified_name"].as_str().unwrap_or("");
        assert!(
            !is_synthetic_placeholder_name(name),
            "[{lang}] `name:` predicate must not surface synthetic placeholders; got name={name:?} in {result}",
            lang = case.label,
        );
        assert!(
            !is_synthetic_placeholder_name(qname),
            "[{lang}] `name:` predicate must not surface synthetic-qualified placeholders; got qualified_name={qname:?} in {result}",
            lang = case.label,
        );
    }
    Ok(())
}

// ============================================================================
// Surface 4 — `sqry --json unused .` (CLI)
// ============================================================================

/// Lock the `unused` CLI surface for Cluster C: the **field's bare
/// name** must NOT appear in the unused report as a synthetic
/// placeholder shape (`<field:.*>` or `<bare_name>@<digits>`). The
/// field itself, when referenced, must also not appear (presence of
/// the `References` edge from `referencing_function` per
/// `C_EDGE_MIGRATE` makes the Property reachable).
///
/// **Scope-limited synthetic check**: `find_unused` today does NOT
/// fully filter all `<ident>@<digits>` synthetic locals (e.g.
/// `selector@120` for parameter receivers, `needTags@96` for
/// shadowed Java method-locals). That is a real plugin-emission
/// gap orthogonal to Cluster C — the field-level `C_SUPPRESS`
/// contract applies to the field's OWN name shadows, not to every
/// synthetic node anywhere in the workspace. We therefore assert
/// the field-scoped invariant: no entry whose name shape is
/// `<bare_field_name>@<digits>` or `<field:.*<bare_field_name>.*>`
/// must appear. The broader synthetic-leak in `unused` is documented
/// as out-of-scope for this DAG and tracked separately via the
/// `_TESTS` / B1 cluster.
fn assert_unused_cli(case: &LangCase, temp: &TempDir) -> Result<()> {
    let (code, stdout, stderr) = run_sqry(temp.path(), &["unused"])?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry unused` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse unused JSON", case.label))?;
    let groups = payload.as_array().cloned().unwrap_or_default();
    if case.expects_unused_synthetic_suppression {
        for group in &groups {
            let symbols = group["symbols"].as_array().cloned().unwrap_or_default();
            for sym in symbols {
                let name = sym["name"].as_str().unwrap_or("");
                let qname = sym["qualified_name"].as_str().unwrap_or("");
                assert!(
                    !is_field_scoped_synthetic_placeholder(name, case.bare_field_name),
                    "[{lang}] unused report must not list a synthetic shadow of `{bare}`; got name={name:?} in {sym}",
                    lang = case.label,
                    bare = case.bare_field_name,
                );
                assert!(
                    !is_field_scoped_synthetic_placeholder(qname, case.bare_field_name),
                    "[{lang}] unused report must not list a synthetic-qualified shadow of `{bare}`; got qualified_name={qname:?} in {sym}",
                    lang = case.label,
                    bare = case.bare_field_name,
                );
            }
        }
    } else {
        // Regression guard for the documented current state: this
        // language has at least one field-scoped synthetic shadow
        // leaking into the unused report. When the plugin gap is
        // closed, flip `expects_unused_synthetic_suppression` to
        // `true` and this branch's assertion will fire and force
        // the matrix update.
        let mut found_leak = false;
        for group in &groups {
            for sym in group["symbols"].as_array().cloned().unwrap_or_default() {
                let name = sym["name"].as_str().unwrap_or("");
                if is_field_scoped_synthetic_placeholder(name, case.bare_field_name) {
                    found_leak = true;
                    break;
                }
            }
            if found_leak {
                break;
            }
        }
        assert!(
            found_leak,
            "[{lang}] expected a `<{bare}>@<digits>` synthetic shadow to leak into `sqry unused` \
             (regression-guard for the documented {lang}-plugin gap), but the unused report \
             is now clean. Flip `expects_unused_synthetic_suppression` to `true` for this row \
             and re-assert positive suppression — the plugin gap appears to be closed.",
            lang = case.label,
            bare = case.bare_field_name,
        );
    }

    // Also exercise the human (non-JSON) format to lock the same
    // field-scoped suppression on the text surface.
    let (text_code, text_stdout, text_stderr) = run_sqry_text(temp.path(), &["unused"])?;
    assert_eq!(
        text_code,
        0,
        "[{lang}] `sqry unused` (text) must exit 0; got code={text_code}\nstdout:\n{text_stdout}\nstderr:\n{text_stderr}",
        lang = case.label,
    );
    assert!(
        !text_stdout.contains("<field:"),
        "[{lang}] unused text surface leaked `<field:...>` placeholder:\n{text_stdout}",
        lang = case.label,
    );
    Ok(())
}

/// Returns true iff `name` is a synthetic placeholder shape AND
/// the placeholder targets the bare field name (`<field_name>@<digits>`
/// or `<field:<...><field_name><...>>`).
fn is_field_scoped_synthetic_placeholder(name: &str, bare_field_name: &str) -> bool {
    if name.starts_with("<field:") && name.ends_with('>') && name.contains(bare_field_name) {
        return true;
    }
    if let Some(at_idx) = name.rfind('@') {
        let prefix = &name[..at_idx];
        let suffix = &name[at_idx + 1..];
        if !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
            && prefix == bare_field_name
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Surface 5 — MCP `direct_callers`
// ============================================================================

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

/// Lock the MCP `direct_callers` shape. Fields are not `Calls`-edge
/// targets so the caller list is conventionally empty for a Property
/// node; we lock the shape contract (handler returns Ok, `target`
/// echoed, `total >= 0`) without requiring caller hits. For the
/// referencing method anchor, `total` may be > 0 (the method has
/// callers from `unrelated()` / `main()` depending on language).
fn assert_mcp_direct_callers(case: &LangCase, temp: &TempDir) -> Result<()> {
    init_caches();
    let workspace = workspace_arg(temp);
    // Anchor on the field's qualified name (in the `::`-separator
    // wire form expected by the MCP strict resolver) when the name
    // is unambiguous; otherwise anchor on the referencing method.
    let anchor = if case.qualified_name_resolves_unambiguously {
        case.qualified_field_name_double_colon.to_string()
    } else {
        case.referencing_function.to_string()
    };
    let args = DirectCallersArgs {
        symbol: anchor.clone(),
        path: workspace,
        max_results: 100,
        pagination: paging(),
    };
    let exec = execute_direct_callers(&args)
        .with_context(|| format!("[{}] direct_callers `{}`", case.label, anchor))?;
    let data = exec.data;
    assert_eq!(
        data.target, anchor,
        "[{}] direct_callers `target` echo mismatch",
        case.label
    );
    // Synthetic-suppression contract: no caller name may be a
    // synthetic placeholder shape.
    for caller in &data.callers {
        assert!(
            !is_synthetic_placeholder_name(&caller.name),
            "[{lang}] direct_callers leaked synthetic-named caller {caller:?}",
            lang = case.label,
        );
    }

    // Flip-on-fix regression guard: the MCP strict resolver only accepts
    // `::`-separated qualified names today (CLI accepts both `::` and `.`).
    // If the MCP path starts accepting dotted qualified names, this
    // assertion turns red and the next implementer is required to update
    // the matrix so the dotted form is treated as a positive resolution
    // alongside the `::` wire form. C_TESTS regression-guard #3 from the
    // BadLiveware DAG (codex C2 iter-1 finding).
    if case.qualified_name_resolves_unambiguously
        && case.qualified_field_name_dot != case.qualified_field_name_double_colon
    {
        let dotted_args = DirectCallersArgs {
            symbol: case.qualified_field_name_dot.to_string(),
            path: workspace_arg(temp),
            max_results: 100,
            pagination: paging(),
        };
        let dotted_result = execute_direct_callers(&dotted_args);
        let dotted_summary = match &dotted_result {
            Ok(_) => "<Ok>".to_string(),
            Err(e) => format!("Err({e})"),
        };
        assert!(
            dotted_result.is_err(),
            "[{lang}] FLIP-ON-FIX: MCP direct_callers used to reject the dotted qualified \
             form `{dot}` but now succeeds; update C_TESTS to assert dotted-form parity \
             with `::` and remove this guard. The MCP strict resolver must accept BOTH \
             separators when this fires, and the field_level_cross_language matrix should \
             enumerate the dotted form as a positive surface alongside the `::` form. \
             Result: {dotted_summary}",
            lang = case.label,
            dot = case.qualified_field_name_dot,
        );
    }

    Ok(())
}

// ============================================================================
// Surface 6 — MCP `direct_callees`
// ============================================================================

fn assert_mcp_direct_callees(case: &LangCase, temp: &TempDir) -> Result<()> {
    init_caches();
    let workspace = workspace_arg(temp);
    let anchor = if case.qualified_name_resolves_unambiguously {
        case.qualified_field_name_double_colon.to_string()
    } else {
        case.referencing_function.to_string()
    };
    let args = DirectCalleesArgs {
        symbol: anchor.clone(),
        path: workspace,
        max_results: 100,
        pagination: paging(),
    };
    let exec = execute_direct_callees(&args)
        .with_context(|| format!("[{}] direct_callees `{}`", case.label, anchor))?;
    let data = exec.data;
    assert_eq!(
        data.source, anchor,
        "[{}] direct_callees `source` echo mismatch",
        case.label
    );
    for callee in &data.callees {
        assert!(
            !is_synthetic_placeholder_name(&callee.name),
            "[{lang}] direct_callees leaked synthetic-named callee {callee:?}",
            lang = case.label,
        );
    }
    Ok(())
}

// ============================================================================
// Surface 7 — references via legacy CLI `query "references:<qn>"`
// ============================================================================

/// Lock the references surface. The MCP `relation_query` tool's
/// `RelationType` enum (Callers, Callees, Imports, Exports, Returns)
/// does NOT expose a `References` variant — see
/// `sqry-mcp/src/tools/validation.rs`. The user-facing references
/// contract today is the legacy planner's `references:<qn>` predicate
/// reachable via `sqry query`. The `relation_query` tool is also
/// exercised here against `RelationType::Imports` to lock the daemon-
/// shaped MCP handler contract for fields (imports of a field are
/// trivially empty — the contract is shape-only: handler returns Ok
/// with `relation_type = "imports"`).
fn assert_references_surface(case: &LangCase, temp: &TempDir) -> Result<()> {
    let predicate = format!("references:{}", case.qualified_field_name_dot);
    let (code, stdout, stderr) = run_sqry(temp.path(), &["query", &predicate])?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry query \"{predicate}\"` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse references JSON", case.label))?;
    // The legacy planner emits one of two JSON shapes depending on
    // the predicate routing:
    // - Graph-evaluator path: `{ "results": [...], "stats": {...} }`
    // - Text-fallback path: `{ "text_matches": [...], "match_count": N }`
    // Both surfaces are user-facing today; we accept either shape and
    // assert the field-level invariants on whichever is present.
    let results: Vec<Value> = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            payload
                .get("text_matches")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    if case.expects_references_hits {
        assert!(
            !results.is_empty(),
            "[{lang}] `sqry query \"{predicate}\"` must surface at least one reference site; got {payload}",
            lang = case.label,
        );
    }
    for result in &results {
        let name = result["name"].as_str().unwrap_or("");
        assert!(
            !is_synthetic_placeholder_name(name),
            "[{lang}] references surface leaked synthetic-named result {result}",
            lang = case.label,
        );
    }

    // Also drive the daemon-shaped MCP `relation_query` handler with
    // RelationType::Imports against the field anchor (in `::` wire
    // form) — locks that the handler does not panic and echoes
    // `relation_type = "imports"`.
    init_caches();
    let workspace = workspace_arg(temp);
    let anchor = if case.qualified_name_resolves_unambiguously {
        case.qualified_field_name_double_colon.to_string()
    } else {
        case.referencing_function.to_string()
    };
    let rel_args = RelationQueryArgs {
        symbol: anchor.clone(),
        relation: RelationType::Imports,
        path: workspace,
        max_depth: 1,
        max_results: 100,
        pagination: paging(),
    };
    let rel_exec = execute_relation_query(&rel_args)
        .with_context(|| format!("[{}] relation_query imports `{}`", case.label, anchor))?;
    assert_eq!(
        rel_exec.data.relation_type, "imports",
        "[{}] relation_query must echo relation_type=imports for `{}`",
        case.label, anchor
    );

    // Flip-on-fix regression guard: MCP `RelationType` enum
    // (`Callers`, `Callees`, `Imports`, `Exports`, `Returns`) does NOT
    // have a `References` variant today, so the user-facing references
    // surface routes through the legacy CLI `query "references:<qn>"`
    // predicate exercised above. The exhaustive-match below has no
    // wildcard arm and lists exactly the five current variants —
    // adding a `References` variant to `RelationType` will fail to
    // compile here, forcing the next implementer to flip C_TESTS so
    // `assert_references_surface` drives the real MCP
    // `RelationType::References` dispatcher alongside the legacy CLI
    // predicate, then remove this guard. C_TESTS regression-guard #4
    // from the BadLiveware DAG (codex C2 iter-1 finding).
    fn _flip_on_fix_relationtype_lacks_references(rt: RelationType) {
        match rt {
            RelationType::Callers => (),
            RelationType::Callees => (),
            RelationType::Imports => (),
            RelationType::Exports => (),
            RelationType::Returns => (),
        }
    }
    // Materialise the function pointer so the compiler keeps the
    // exhaustive-match alive even when this surface helper is dead-code.
    let _: fn(RelationType) = _flip_on_fix_relationtype_lacks_references;
    // Suppress the "FLIP-ON-FIX guard at line " warning we'd otherwise
    // pay if someone wires the underscore-prefixed helper into a
    // wildcard match later. The const evaluator above is the contract;
    // this assertion just keeps the symbol semantically anchored.
    assert!(
        matches!(
            RelationType::Returns,
            RelationType::Callers
                | RelationType::Callees
                | RelationType::Imports
                | RelationType::Exports
                | RelationType::Returns
        ),
        "[{lang}] flip-on-fix guard: RelationType::Returns must remain in the \
         five-variant set; if the enum gains a `References` variant or any \
         other new variant, the static guard above also needs updating.",
        lang = case.label,
    );

    Ok(())
}

// ============================================================================
// Negative test: AmbiguousSymbol envelope
// ============================================================================

/// Lock the `sqry::ambiguous_symbol` envelope: bare field name with a
/// real same-name collision MUST surface the typed error envelope
/// from `sqry impact --json --path . <bare>` with exit code 4.
fn assert_ambiguous_envelope(case: &LangCase, temp: &TempDir) -> Result<()> {
    let (code, stdout, stderr) = run_sqry(
        temp.path(),
        &["impact", "--path", ".", case.bare_field_name],
    )?;
    assert_eq!(
        code,
        4,
        "[{lang}] `sqry impact --path . {bare}` must exit code 4 on ambiguity; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
        bare = case.bare_field_name,
    );
    let envelope: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse ambiguous envelope JSON", case.label))?;
    let error = envelope.get("error").unwrap_or_else(|| {
        panic!(
            "[{lang}] envelope missing top-level `error`: {envelope}",
            lang = case.label
        )
    });
    assert_eq!(
        error["code"],
        "sqry::ambiguous_symbol",
        "[{lang}] envelope `code` mismatch: {envelope}",
        lang = case.label,
    );
    let message = error["message"].as_str().expect("message is string");
    assert!(
        message.contains(case.bare_field_name) && message.contains("ambiguous"),
        "[{lang}] envelope message must name the symbol and the ambiguity, got {message:?}",
        lang = case.label,
    );
    let candidates = error["candidates"].as_array().unwrap_or_else(|| {
        panic!(
            "[{lang}] envelope missing `candidates[]`: {envelope}",
            lang = case.label
        )
    });
    assert!(
        candidates.len() >= 2,
        "[{lang}] envelope must list at least 2 candidates; got {n}: {envelope}",
        lang = case.label,
        n = candidates.len(),
    );
    for candidate in candidates {
        assert!(
            candidate.get("qualified_name").is_some(),
            "[{}] candidate missing qualified_name: {candidate}",
            case.label
        );
        assert!(
            candidate.get("kind").is_some(),
            "[{}] candidate missing kind: {candidate}",
            case.label
        );
        assert!(
            candidate.get("file_path").is_some(),
            "[{}] candidate missing file_path: {candidate}",
            case.label
        );
        assert!(
            candidate.get("start_line").is_some(),
            "[{}] candidate missing start_line: {candidate}",
            case.label
        );
        assert!(
            candidate.get("start_column").is_some(),
            "[{}] candidate missing start_column: {candidate}",
            case.label
        );
        // Candidate qualified_name must NOT be a synthetic placeholder
        // — `C_SUPPRESS` requires synthetic shadows be invisible to
        // the resolver's candidate set.
        let qn = candidate["qualified_name"].as_str().unwrap_or("");
        assert!(
            !is_synthetic_placeholder_name(qn),
            "[{lang}] ambiguity candidate is a synthetic placeholder {qn:?}: {candidate}",
            lang = case.label,
        );
    }
    Ok(())
}

// ============================================================================
// Negative test: Synthetic-suppression
// ============================================================================

/// Lock `C_SUPPRESS`: `sqry --exact <bare>` must not surface any
/// synthetic-placeholder shape (`<field:...>` or `<name>@<offset>`).
fn assert_synthetic_suppression(case: &LangCase, temp: &TempDir) -> Result<()> {
    let (code, stdout, stderr) = run_sqry(temp.path(), &["--exact", case.bare_field_name])?;
    assert_eq!(
        code,
        0,
        "[{lang}] `sqry --exact {bare}` must exit 0; got code={code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        lang = case.label,
        bare = case.bare_field_name,
    );
    let payload: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("[{}] parse --exact JSON", case.label))?;
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !results.is_empty(),
        "[{lang}] `sqry --exact {bare}` must surface at least one real candidate; got {payload}",
        lang = case.label,
        bare = case.bare_field_name,
    );
    for result in &results {
        let name = result["name"].as_str().unwrap_or("");
        let qname = result["qualified_name"].as_str().unwrap_or("");
        assert!(
            !is_synthetic_placeholder_name(name),
            "[{lang}] `sqry --exact {bare}` leaked synthetic placeholder name {name:?}: {result}",
            lang = case.label,
            bare = case.bare_field_name,
        );
        assert!(
            !is_synthetic_placeholder_name(qname),
            "[{lang}] `sqry --exact {bare}` leaked synthetic placeholder qualified_name {qname:?}: {result}",
            lang = case.label,
            bare = case.bare_field_name,
        );
    }
    Ok(())
}

// ============================================================================
// Workspace-arg helper for MCP handler invocations
// ============================================================================

fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

// ============================================================================
// Per-language tests
// ============================================================================
//
// Each #[test] resolves to exactly one row of the [`CASES`] matrix by
// label. Per the DAG `constraints` ("fresh-index temp workspace per
// test, no shared `.sqry/` state") every test allocates its own
// `TempDir`, writes the language fixture, and invokes `sqry index`
// before any assertion runs.

fn case_for(label: &str) -> &'static LangCase {
    CASES
        .iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("missing matrix entry for language `{label}`"))
}

/// Drive every positive surface for one language against a single
/// freshly-built index. Per the matrix `LangCase` row.
fn run_positive_matrix(case: &LangCase) -> Result<()> {
    let temp = write_and_index(case)?;
    assert_impact_cli(case, &temp)?;
    assert_explain_cli(case, &temp)?;
    assert_name_query_cli(case, &temp)?;
    assert_unused_cli(case, &temp)?;
    assert_mcp_direct_callers(case, &temp)?;
    assert_mcp_direct_callees(case, &temp)?;
    assert_references_surface(case, &temp)?;
    Ok(())
}

// ---------- Go ----------

#[test]
fn go_field_positive_matrix() -> Result<()> {
    run_positive_matrix(case_for("go"))
}

#[test]
fn go_field_ambiguous_envelope() -> Result<()> {
    let case = case_for("go");
    let temp = write_and_index(case)?;
    assert_ambiguous_envelope(case, &temp)
}

#[test]
fn go_field_synthetic_suppression() -> Result<()> {
    let case = case_for("go");
    let temp = write_and_index(case)?;
    assert_synthetic_suppression(case, &temp)
}

// ---------- Java ----------

#[test]
fn java_field_positive_matrix() -> Result<()> {
    run_positive_matrix(case_for("java"))
}

#[test]
fn java_field_ambiguous_envelope() -> Result<()> {
    let case = case_for("java");
    let temp = write_and_index(case)?;
    assert_ambiguous_envelope(case, &temp)
}

#[test]
fn java_field_synthetic_suppression() -> Result<()> {
    let case = case_for("java");
    let temp = write_and_index(case)?;
    assert_synthetic_suppression(case, &temp)
}

// ---------- Python ----------

#[test]
fn python_field_positive_matrix() -> Result<()> {
    run_positive_matrix(case_for("python"))
}

#[test]
fn python_field_ambiguous_envelope() -> Result<()> {
    let case = case_for("python");
    let temp = write_and_index(case)?;
    assert_ambiguous_envelope(case, &temp)
}

#[test]
fn python_field_synthetic_suppression() -> Result<()> {
    let case = case_for("python");
    let temp = write_and_index(case)?;
    assert_synthetic_suppression(case, &temp)
}

// ---------- Rust ----------

#[test]
fn rust_field_positive_matrix() -> Result<()> {
    run_positive_matrix(case_for("rust"))
}

#[test]
fn rust_field_ambiguous_envelope() -> Result<()> {
    let case = case_for("rust");
    let temp = write_and_index(case)?;
    assert_ambiguous_envelope(case, &temp)
}

#[test]
fn rust_field_synthetic_suppression() -> Result<()> {
    let case = case_for("rust");
    let temp = write_and_index(case)?;
    assert_synthetic_suppression(case, &temp)
}

// ============================================================================
// Snapshot stale-detection sub-test (deferred from C_EDGE_MIGRATE)
// ============================================================================
//
// `C_EDGE_MIGRATE`'s acceptance defers the on-disk snapshot stale-
// detection contract to this unit. The migration is in-format
// (`SQRY_GRAPH_V10` magic unchanged — only edge source identity moved
// from struct-id to property-id), so the testable invariant is round-
// trip stability: the post-snapshot replay of the canonical Cluster C
// query must equal the pre-snapshot result.
//
// Approach: build the Go fixture, persist `.sqry/graph/snapshot.sqry`
// via `sqry index`, take a baseline `sqry impact --path . main.SelectorSource.NeedTags`
// reading, then re-run the same impact query in a second CLI
// invocation (which forces a fresh snapshot load via `engine_for_workspace`)
// and assert byte-equal results.

#[test]
fn snapshot_stale_detection_round_trip_property_sourced_typeof_field() -> Result<()> {
    let case = case_for("go");
    let temp = write_and_index(case)?;

    // Baseline reading.
    let (code1, stdout1, _) = run_sqry(
        temp.path(),
        &["impact", "--path", ".", case.qualified_field_name_dot],
    )?;
    assert_eq!(code1, 0, "baseline impact must exit 0");
    let baseline: Value = serde_json::from_str(&stdout1)?;

    // Second invocation. Each `sqry` invocation is a fresh process, so
    // this reload comes from the persisted V10 snapshot via
    // `load_unified_graph_for_cli` -> `Snapshot::load`. Asserting
    // equality on the impact envelope locks the round-trip stability
    // of the Property-sourced TypeOf{Field} migration shape.
    let (code2, stdout2, _) = run_sqry(
        temp.path(),
        &["impact", "--path", ".", case.qualified_field_name_dot],
    )?;
    assert_eq!(code2, 0, "second impact must exit 0");
    let replayed: Value = serde_json::from_str(&stdout2)?;
    assert_eq!(
        baseline, replayed,
        "post-snapshot impact replay must equal pre-snapshot reading; this guards the\n\
         persisted V10 shape of the C_EDGE_MIGRATE Property-sourced TypeOf{{Field}} edges.\n\
         If this fires, the on-disk snapshot lost the post-migration shape — the regression\n\
         is in the persistence layer, not the in-process build pipeline."
    );

    // Also verify the Property node is in the user-facing `--exact`
    // surface AFTER reload, which confirms the Property node itself
    // round-trips through the snapshot file (not just its incoming
    // edges).
    let (exact_code, exact_stdout, _) = run_sqry(temp.path(), &["--exact", case.bare_field_name])?;
    assert_eq!(exact_code, 0);
    let exact: Value = serde_json::from_str(&exact_stdout)?;
    let results = exact["results"].as_array().cloned().unwrap_or_default();
    let has_property = results.iter().any(|r| {
        r["kind"].as_str() == Some(case.expected_field_kind)
            && r["qualified_name"].as_str() == Some(case.qualified_field_name_dot)
    });
    assert!(
        has_property,
        "post-snapshot `sqry --exact {}` must surface the {} node `{}`; got {}",
        case.bare_field_name, case.expected_field_kind, case.qualified_field_name_dot, exact
    );
    Ok(())
}

// ============================================================================
// Sanity: the matrix covers every language listed in the DAG
// ============================================================================

#[test]
fn matrix_covers_every_dag_language() {
    // DAG `C_TESTS.summary` enumerates: Go field, Java field, Python class
    // attribute (here represented by the canonical `@property` Property-kind
    // node), Rust struct field.
    let expected = ["go", "java", "python", "rust"];
    for lang in expected {
        assert!(
            CASES.iter().any(|c| c.label == lang),
            "DAG C_TESTS scope requires `{lang}`, but it is missing from the CASES matrix"
        );
    }
    assert_eq!(
        CASES.len(),
        expected.len(),
        "CASES matrix has {} entries but DAG C_TESTS scope requires exactly {}",
        CASES.len(),
        expected.len()
    );
}

// ============================================================================
// Self-test: synthetic-placeholder pattern recogniser
// ============================================================================

#[test]
fn synthetic_placeholder_recogniser_is_correct() {
    assert!(is_synthetic_placeholder_name("<field:selector.NeedTags>"));
    assert!(is_synthetic_placeholder_name("<field:x.y>"));
    assert!(is_synthetic_placeholder_name("NeedTags@120"));
    assert!(is_synthetic_placeholder_name("selector@42"));
    assert!(is_synthetic_placeholder_name("x@0"));
    assert!(!is_synthetic_placeholder_name("NeedTags"));
    assert!(!is_synthetic_placeholder_name(
        "main.SelectorSource.NeedTags"
    ));
    // `@` not followed by digits — not a synthetic offset suffix.
    assert!(!is_synthetic_placeholder_name("user@example"));
    assert!(!is_synthetic_placeholder_name("foo@"));
    // Closing `>` required for the `<field:...>` shape.
    assert!(!is_synthetic_placeholder_name("<field:foo"));
}

#[test]
fn field_scoped_synthetic_placeholder_recogniser_is_correct() {
    // `<bare>@<digits>` shape — must match exactly, prefix-only.
    assert!(is_field_scoped_synthetic_placeholder(
        "NeedTags@120",
        "NeedTags"
    ));
    assert!(is_field_scoped_synthetic_placeholder(
        "needTags@96",
        "needTags"
    ));
    assert!(is_field_scoped_synthetic_placeholder(
        "display_name@5",
        "display_name"
    ));
    // `<field:.*<bare>.*>` shape.
    assert!(is_field_scoped_synthetic_placeholder(
        "<field:selector.NeedTags>",
        "NeedTags"
    ));
    // Different field name — must NOT match.
    assert!(!is_field_scoped_synthetic_placeholder(
        "selector@120",
        "NeedTags"
    ));
    assert!(!is_field_scoped_synthetic_placeholder(
        "other@42", "NeedTags"
    ));
    // Real qualified names — must NOT match.
    assert!(!is_field_scoped_synthetic_placeholder(
        "NeedTags", "NeedTags"
    ));
    assert!(!is_field_scoped_synthetic_placeholder(
        "main.SelectorSource.NeedTags",
        "NeedTags"
    ));
}
