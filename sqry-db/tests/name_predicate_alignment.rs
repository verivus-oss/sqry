//! B1_ALIGN — set-equality between the planner `name:` predicate and the
//! CLI `--exact` shorthand on a multi-language fixture.
//!
//! DAG unit `B1_TESTS` (`docs/development/public-issue-triage/2026-04-29_badliveware_go_batch_dag.toml`).
//! Closes verivus-oss/sqry#78 / verivus-oss/sqry#157 alongside the
//! `B1_ALIGN` implementation in `sqry-db/src/planner/parse.rs`,
//! `sqry-db/src/planner/execute.rs`, and
//! `sqry-core/src/graph/unified/concurrent/graph.rs`.
//!
//! Acceptance (DAG, exact wording):
//!
//!   - test asserts set-equality between `name:NeedTags` and
//!     `--exact NeedTags` for the BadLiveware fixture
//!   - test asserts the same for at least Rust function and Python
//!     class fixtures
//!   - test asserts `name:NeedTags` does not include synthetic-flagged
//!     nodes
//!   - fresh-index temp workspace per test
//!
//! Each test builds its own [`CodeGraph`] from scratch (the
//! "fresh-index temp workspace" requirement; we keep the fixtures
//! synthetic rather than driving real plugins so the test suite stays
//! hermetic and fast). Both surfaces under test route through
//! [`GraphSnapshot::find_by_exact_name`] (CLI) or the planner's
//! `entry_name_matches` / `scan_match` (which call
//! [`GraphSnapshot::is_node_synthetic`] in the same shape) — those two
//! paths are contract-bound to return identical sets and that is what
//! these tests verify.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{execute_plan, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn add_node(graph: &mut CodeGraph, entry: NodeEntry) -> NodeId {
    let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
    graph
        .indices_mut()
        .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
    id
}

/// Run `name:<value>` through the planner end-to-end and return the
/// sorted/deduplicated `NodeId` set.
fn planner_name_set(db: &QueryDb, name: &str) -> Vec<NodeId> {
    let plan = parse_query(&format!("name:{name}"))
        .unwrap_or_else(|err| panic!("parse_query(\"name:{name}\") failed: {err}"));
    execute_plan(&plan, db)
}

/// Mirror the CLI `--exact` path
/// (`sqry-cli/src/commands/search.rs::run_regular_search`): it calls
/// `snapshot.find_by_exact_name(pattern)` directly. Sort/dedup so the
/// result is comparable to the executor's canonical order.
fn cli_exact_set(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    let mut ids = snapshot.find_by_exact_name(name);
    ids.sort_unstable_by_key(|id| (id.index(), id.generation()));
    ids.dedup();
    ids
}

fn make_db(graph: CodeGraph) -> (Arc<GraphSnapshot>, QueryDb) {
    let snapshot = Arc::new(graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    (snapshot, db)
}

// ---------------------------------------------------------------------------
// Cluster B1 — Go field fixture (the BadLiveware regression itself)
// ---------------------------------------------------------------------------

/// BadLiveware fixture: a Go file with a struct field
/// `main.SelectorSource.NeedTags` (Property, qualified name carries the
/// package + receiver prefix), a local variable `NeedTags` (Variable,
/// simple name), the synthetic `<field:selector.NeedTags>` shadow the
/// Go plugin emits during ad-hoc field references, and an
/// offset-suffixed synthetic `NeedTags@469` flagged via the metadata
/// bit. Plus an unrelated `NeedTagsHelper` function so substring
/// behaviour can never leak through the exact-name surface.
#[test]
fn name_predicate_aligns_with_exact_for_go_badliveware_field_fixture() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("main.go"), Some(Language::Go))
        .expect("register go file");

    let property_qname = graph
        .strings_mut()
        .intern("main.SelectorSource.NeedTags")
        .expect("intern property qname");
    let property_simple = graph
        .strings_mut()
        .intern("NeedTags")
        .expect("intern property simple");
    let local_var_name = graph
        .strings_mut()
        .intern("NeedTags")
        .expect("intern local var");
    let synthetic_field_name = graph
        .strings_mut()
        .intern("<field:selector.NeedTags>")
        .expect("intern synthetic field");
    let synthetic_offset_name = graph
        .strings_mut()
        .intern("NeedTags@469")
        .expect("intern synthetic offset");
    let unrelated_name = graph
        .strings_mut()
        .intern("NeedTagsHelper")
        .expect("intern unrelated");

    // The Property carries `entry.name = "NeedTags"` and
    // `entry.qualified_name = "main.SelectorSource.NeedTags"` — this is
    // the C_PROPERTY_EMIT shape the Go plugin produces for struct
    // fields, and it is the only one of these five nodes that should
    // surface through `name:NeedTags`.
    let property_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Property, property_simple, file)
            .with_byte_range(10, 80)
            .with_qualified_name(property_qname),
    );
    let local_var_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, local_var_name, file).with_byte_range(100, 160),
    );
    let syn_field_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, synthetic_field_name, file).with_byte_range(200, 260),
    );
    let syn_offset_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, synthetic_offset_name, file).with_byte_range(300, 360),
    );
    let _unrelated_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, unrelated_name, file).with_byte_range(400, 460),
    );

    // Mark the offset-suffixed synthetic via the metadata bit. The
    // `<field:…>` synthetic is recognised by structural name shape; we
    // exercise both channels in this fixture.
    graph.macro_metadata_mut().mark_synthetic(syn_offset_id);

    let (snapshot, db) = make_db(graph);

    let planner = planner_name_set(&db, "NeedTags");
    let cli = cli_exact_set(&snapshot, "NeedTags");

    assert_eq!(
        planner, cli,
        "planner `name:NeedTags` and CLI `--exact NeedTags` must return the same set"
    );

    // Defence-in-depth: the set must contain the Property + the local
    // Variable, and must NOT contain either synthetic.
    assert!(
        planner.contains(&property_id),
        "Property `main.SelectorSource.NeedTags` must surface (qualified-name match)"
    );
    assert!(
        planner.contains(&local_var_id),
        "Variable `NeedTags` must surface (simple-name match)"
    );
    assert!(
        !planner.contains(&syn_field_id),
        "synthetic `<field:selector.NeedTags>` must not surface"
    );
    assert!(
        !planner.contains(&syn_offset_id),
        "synthetic `NeedTags@469` must not surface"
    );
}

// ---------------------------------------------------------------------------
// Rust function fixture
// ---------------------------------------------------------------------------

#[test]
fn name_predicate_aligns_with_exact_for_rust_function_fixture() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
        .expect("register rust file");

    let parse_expr_name = graph.strings_mut().intern("parse_expr").expect("intern");
    let parse_expr_qname = graph
        .strings_mut()
        .intern("my_crate::parser::parse_expr")
        .expect("intern qname");
    let parse_stmt_name = graph.strings_mut().intern("parse_stmt").expect("intern");
    let synthetic_at_offset = graph
        .strings_mut()
        .intern("parse_expr@128")
        .expect("intern synthetic");

    let target_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, parse_expr_name, file)
            .with_byte_range(10, 80)
            .with_qualified_name(parse_expr_qname),
    );
    let _other_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, parse_stmt_name, file).with_byte_range(100, 160),
    );
    let syn_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, synthetic_at_offset, file).with_byte_range(200, 260),
    );
    graph.macro_metadata_mut().mark_synthetic(syn_id);

    let (snapshot, db) = make_db(graph);

    // Simple-name lookup.
    assert_eq!(
        planner_name_set(&db, "parse_expr"),
        cli_exact_set(&snapshot, "parse_expr"),
        "Rust function: planner and CLI must agree on simple-name lookup"
    );
    assert_eq!(planner_name_set(&db, "parse_expr"), vec![target_id]);

    // Qualified-name lookup.
    assert_eq!(
        planner_name_set(&db, "my_crate::parser::parse_expr"),
        cli_exact_set(&snapshot, "my_crate::parser::parse_expr"),
        "Rust function: planner and CLI must agree on qualified-name lookup"
    );

    // Synthetic must not surface even though `parse_expr@128` shares a
    // prefix with the target.
    assert!(
        !planner_name_set(&db, "parse_expr").contains(&syn_id),
        "synthetic `parse_expr@128` must not surface for `name:parse_expr`"
    );
    assert!(planner_name_set(&db, "parse_expr@128").is_empty());
}

// ---------------------------------------------------------------------------
// Python class fixture
// ---------------------------------------------------------------------------

#[test]
fn name_predicate_aligns_with_exact_for_python_class_fixture() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("visitor.py"), Some(Language::Python))
        .expect("register python file");

    let visitor_name = graph.strings_mut().intern("Visitor").expect("intern");
    let visitor_qname = graph
        .strings_mut()
        .intern("ast.visitor.Visitor")
        .expect("intern qname");
    let other_name = graph.strings_mut().intern("AsyncVisitor").expect("intern");
    let synthetic_field_name = graph
        .strings_mut()
        .intern("<field:node.Visitor>")
        .expect("intern synthetic");

    let target_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Class, visitor_name, file)
            .with_byte_range(10, 80)
            .with_qualified_name(visitor_qname),
    );
    let _other_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Class, other_name, file).with_byte_range(100, 160),
    );
    let syn_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, synthetic_field_name, file).with_byte_range(200, 260),
    );

    let (snapshot, db) = make_db(graph);

    // Simple-name lookup.
    assert_eq!(
        planner_name_set(&db, "Visitor"),
        cli_exact_set(&snapshot, "Visitor"),
        "Python class: planner and CLI must agree on simple-name lookup"
    );
    assert_eq!(planner_name_set(&db, "Visitor"), vec![target_id]);

    // Qualified-name lookup.
    assert_eq!(
        planner_name_set(&db, "ast.visitor.Visitor"),
        cli_exact_set(&snapshot, "ast.visitor.Visitor"),
        "Python class: planner and CLI must agree on qualified-name lookup"
    );

    // Substring-style would have matched `AsyncVisitor`; exact must not.
    assert!(!planner_name_set(&db, "Visitor").contains(&_other_id));
    // Structural-shape synthetic (`<field:…>`) must not surface even
    // without an explicit metadata flip.
    assert!(!planner_name_set(&db, "Visitor").contains(&syn_id));
}

// ---------------------------------------------------------------------------
// Synthetic exclusion — both recognition channels
// ---------------------------------------------------------------------------

/// `name:Foo` must drop nodes flagged via [`NodeMetadataStore::mark_synthetic`]
/// even when their interned names look entirely natural (no `<…>` or
/// `…@<offset>` shape) — the metadata bit alone is sufficient.
#[test]
fn name_predicate_excludes_synthetic_nodes_via_metadata_bit_only() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("main.go"), Some(Language::Go))
        .expect("register go file");

    let foo_name = graph.strings_mut().intern("Foo").expect("intern");

    let real_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, foo_name, file).with_byte_range(10, 80),
    );
    let synthetic_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, foo_name, file).with_byte_range(100, 160),
    );
    graph.macro_metadata_mut().mark_synthetic(synthetic_id);

    let (snapshot, db) = make_db(graph);

    let planner = planner_name_set(&db, "Foo");
    let cli = cli_exact_set(&snapshot, "Foo");

    assert_eq!(
        planner, cli,
        "planner and CLI must agree on synthetic exclusion"
    );
    assert_eq!(
        planner,
        vec![real_id],
        "metadata-flagged synthetic must be invisible to `name:` even with a natural-shaped name"
    );
    assert!(!planner.contains(&synthetic_id));
}

/// `name:` must also drop nodes whose name shape itself is recognised
/// as synthetic (`<…>` and `…@<offset>`) when the metadata bit was
/// never flipped — defence-in-depth for V10 snapshots written before
/// the bit existed and unification losers that lost their metadata
/// entry.
#[test]
fn name_predicate_excludes_synthetic_nodes_via_structural_shape_only() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("main.go"), Some(Language::Go))
        .expect("register go file");

    let bar_name = graph.strings_mut().intern("Bar").expect("intern");
    let field_shape = graph
        .strings_mut()
        .intern("<field:s.Bar>")
        .expect("intern field shape");
    let offset_shape = graph
        .strings_mut()
        .intern("Bar@317")
        .expect("intern offset shape");

    let real_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, bar_name, file).with_byte_range(10, 80),
    );
    let _field_synthetic = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, field_shape, file).with_byte_range(100, 160),
    );
    let _offset_synthetic = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Variable, offset_shape, file).with_byte_range(200, 260),
    );

    let (snapshot, db) = make_db(graph);

    // Direct lookup of the synthetic-shaped names must come back empty
    // — the structural fallback is unconditional on the exact-name
    // surface.
    assert!(snapshot.find_by_exact_name("<field:s.Bar>").is_empty());
    assert!(snapshot.find_by_exact_name("Bar@317").is_empty());

    // The `name:Bar` lookup matches the real function and only the
    // real function on both surfaces.
    let planner = planner_name_set(&db, "Bar");
    let cli = cli_exact_set(&snapshot, "Bar");
    assert_eq!(planner, cli);
    assert_eq!(planner, vec![real_id]);
}

// ---------------------------------------------------------------------------
// Cross-language sanity: same fixture, three languages, single graph
// ---------------------------------------------------------------------------

/// One graph, three nodes that collectively assert: exact-name equality
/// across language boundaries does not get confused by qualified-name
/// formats. `name:Visitor` must match the Python class but NOT the
/// Rust trait `my_crate::ast::Visitor` *unless* its simple name is
/// also `"Visitor"` — and qualified-name lookup picks the right one.
#[test]
fn name_predicate_disambiguates_across_languages_in_a_shared_graph() {
    let mut graph = CodeGraph::new();
    let py_file = graph
        .files_mut()
        .register_with_language(Path::new("a.py"), Some(Language::Python))
        .expect("register py file");
    let rs_file = graph
        .files_mut()
        .register_with_language(Path::new("b.rs"), Some(Language::Rust))
        .expect("register rs file");
    let go_file = graph
        .files_mut()
        .register_with_language(Path::new("c.go"), Some(Language::Go))
        .expect("register go file");

    let visitor_name = graph.strings_mut().intern("Visitor").expect("intern");
    let py_visitor_qname = graph
        .strings_mut()
        .intern("ast.visitor.Visitor")
        .expect("intern py qname");
    let rs_visitor_qname = graph
        .strings_mut()
        .intern("my_crate::ast::Visitor")
        .expect("intern rs qname");
    let go_visitor_qname = graph
        .strings_mut()
        .intern("ast.Visitor")
        .expect("intern go qname");

    let py_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Class, visitor_name, py_file)
            .with_byte_range(10, 80)
            .with_qualified_name(py_visitor_qname),
    );
    let rs_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Trait, visitor_name, rs_file)
            .with_byte_range(10, 80)
            .with_qualified_name(rs_visitor_qname),
    );
    let go_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Interface, visitor_name, go_file)
            .with_byte_range(10, 80)
            .with_qualified_name(go_visitor_qname),
    );

    let (snapshot, db) = make_db(graph);

    // Simple-name `name:Visitor` matches all three (each has `entry.name = "Visitor"`).
    let planner_simple = planner_name_set(&db, "Visitor");
    let cli_simple = cli_exact_set(&snapshot, "Visitor");
    assert_eq!(planner_simple, cli_simple);
    let mut expected = vec![py_id, rs_id, go_id];
    expected.sort_unstable_by_key(|id| (id.index(), id.generation()));
    assert_eq!(planner_simple, expected);

    // Qualified-name `name:my_crate::ast::Visitor` matches the Rust
    // trait alone — the Python and Go nodes have different qualified
    // names.
    assert_eq!(
        planner_name_set(&db, "my_crate::ast::Visitor"),
        cli_exact_set(&snapshot, "my_crate::ast::Visitor")
    );
    assert_eq!(planner_name_set(&db, "my_crate::ast::Visitor"), vec![rs_id]);
    assert_eq!(
        planner_name_set(&db, "ast.visitor.Visitor"),
        vec![py_id],
        "Python qualified name must match Python only"
    );
    assert_eq!(
        planner_name_set(&db, "ast.Visitor"),
        vec![go_id],
        "Go qualified name must match Go only"
    );
}

// ---------------------------------------------------------------------------
// Glob-promotion contract: literal vs glob behavioural split
// ---------------------------------------------------------------------------

/// `name:` accepts glob meta (`*`, `?`, `[`) and promotes the value to
/// a glob match (still synthetic-filtered). The CLI `--exact` shorthand
/// does **not** accept glob meta — it treats every character as a
/// literal — so the exact-set-equality contract holds **only** for
/// literal values. This test locks that split:
///
/// 1. **Literal value:** `name:parse_expr` ≡ `--exact parse_expr` (the
///    contract that Layer-3 B1_ALIGN ships).
/// 2. **Glob value:** `name:parse_*` matches multiple nodes via glob;
///    `--exact parse_*` matches at most a single node whose interned
///    simple or qualified name is the literal string `parse_*`.
///
/// Without this regression guard the two surfaces could silently drift
/// — for example if a future change made `parse_string_pattern`
/// promote `parse_*` to a regex while `--exact` continued to treat
/// `parse_*` as a literal, the README contract would be wrong.
#[test]
fn name_predicate_glob_meta_promotes_to_glob_match_in_planner_only() {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
        .expect("register rust file");

    let parse_expr_name = graph.strings_mut().intern("parse_expr").expect("intern");
    let parse_stmt_name = graph.strings_mut().intern("parse_stmt").expect("intern");
    let parse_block_name = graph.strings_mut().intern("parse_block").expect("intern");
    // A genuine name containing the literal "*" character — used so the
    // CLI `--exact 'parse_*'` lookup has at least one node it can hit
    // and we can prove it does NOT hit the glob-shape matches.
    let literal_glob_name = graph.strings_mut().intern("parse_*").expect("intern");

    let parse_expr_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, parse_expr_name, file).with_byte_range(10, 80),
    );
    let parse_stmt_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, parse_stmt_name, file).with_byte_range(100, 160),
    );
    let parse_block_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, parse_block_name, file).with_byte_range(200, 260),
    );
    let literal_glob_id = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, literal_glob_name, file).with_byte_range(300, 360),
    );

    let (snapshot, db) = make_db(graph);

    // (1) Literal contract: planner ≡ CLI exact.
    let planner_literal = planner_name_set(&db, "parse_expr");
    let cli_literal = cli_exact_set(&snapshot, "parse_expr");
    assert_eq!(
        planner_literal, cli_literal,
        "literal `name:parse_expr` must equal `--exact parse_expr`"
    );
    assert_eq!(planner_literal, vec![parse_expr_id]);

    // (2) Glob path in the planner. `name:parse_*` matches every
    // function whose simple name has the `parse_` prefix — that's
    // `parse_expr`, `parse_stmt`, `parse_block`, AND the literal
    // `parse_*` node itself (its simple name also matches the glob).
    let planner_glob = planner_name_set(&db, "parse_*");
    let mut expected_glob = vec![
        parse_expr_id,
        parse_stmt_id,
        parse_block_id,
        literal_glob_id,
    ];
    expected_glob.sort_unstable_by_key(|id| (id.index(), id.generation()));
    assert_eq!(
        planner_glob, expected_glob,
        "planner glob `name:parse_*` must match every `parse_`-prefixed name"
    );

    // (3) `--exact parse_*` is a literal lookup — it matches only the
    // node whose interned name is the literal three-byte string
    // `parse_*`. The glob-shape matches MUST NOT surface here.
    let cli_glob_value = cli_exact_set(&snapshot, "parse_*");
    assert_eq!(
        cli_glob_value,
        vec![literal_glob_id],
        "`--exact parse_*` is literal-only — must match only the literal `parse_*` node, never `parse_expr` etc."
    );

    // (4) Confirm the two surfaces are **not** set-equal for glob
    // values. This is the explicit complement of the literal contract
    // and the reason the README scopes the set-equality claim to
    // literal values only.
    assert_ne!(
        planner_glob, cli_glob_value,
        "glob and exact paths intentionally diverge — the contract holds for literals only"
    );

    // (5) Synthetic filter still applies on the glob path.
    //
    // Subtlety (codex iter-2): the synthetic node's `entry.name` (or
    // `entry.qualified_name`) MUST match the glob, otherwise
    // `entry_name_matches` returns early on the simple/qualified-name
    // check and never reaches the `is_node_synthetic` branch — and
    // the test would pass even if synthetic suppression on the glob
    // path were removed. Both channels exercised below use a
    // glob-matching `entry.name` so the assertion genuinely fails if
    // `is_node_synthetic` is ever pulled out of the planner's
    // name-match path:
    //
    //   (a) Metadata-bit channel. Simple name `parse_inner` matches
    //       `parse_*`. Flagged via `NodeMetadataStore::mark_synthetic`.
    //
    //   (b) Structural-shape channel. Simple name `parse_helper@921`
    //       is itself the `<ident>@<offset>` synthetic shape that
    //       `is_synthetic_placeholder_name` recognises, AND its
    //       `parse_` prefix matches the glob `parse_*`. No metadata
    //       bit is flipped — the structural fallback is what gates it
    //       out.
    //
    // (Built as a fresh fixture so we can mutate the graph after the
    // snapshot has been taken above.)
    let mut graph2 = CodeGraph::new();
    let file2 = graph2
        .files_mut()
        .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
        .expect("register rust file");
    let parse_expr_name2 = graph2.strings_mut().intern("parse_expr").expect("intern");
    let bit_synthetic_name = graph2.strings_mut().intern("parse_inner").expect("intern");
    let shape_synthetic_name = graph2
        .strings_mut()
        .intern("parse_helper@921")
        .expect("intern structural-shape simple name");

    let parse_id2 = add_node(
        &mut graph2,
        NodeEntry::new(NodeKind::Function, parse_expr_name2, file2).with_byte_range(10, 80),
    );
    let bit_synthetic_id = add_node(
        &mut graph2,
        NodeEntry::new(NodeKind::Variable, bit_synthetic_name, file2).with_byte_range(100, 160),
    );
    let _shape_synthetic_id = add_node(
        &mut graph2,
        NodeEntry::new(NodeKind::Variable, shape_synthetic_name, file2).with_byte_range(200, 260),
    );

    // Flag (a) via the metadata bit. (b) is gated by the structural
    // recognition fallback inside `is_node_synthetic`.
    graph2.macro_metadata_mut().mark_synthetic(bit_synthetic_id);

    let (_snap2, db2) = make_db(graph2);

    // The glob `parse_*` matches every one of the three nodes by
    // simple name. Synthetic suppression is the only thing keeping
    // (a) and (b) out of the result; without `is_node_synthetic`
    // gating the name match, the result would be three NodeIds
    // instead of one.
    assert_eq!(
        planner_name_set(&db2, "parse_*"),
        vec![parse_id2],
        "synthetic placeholders must not surface through the glob path \
         (would fail without `is_node_synthetic` because both synthetic \
         simple names match the glob)"
    );
}
