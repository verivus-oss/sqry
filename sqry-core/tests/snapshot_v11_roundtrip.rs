//! V11 snapshot end-to-end round-trip integration test (IMP:c-icall-precision-006).
//!
//! Confirms that the Phase A V11 wire surfaces land coherently:
//!
//! - U02 — `TypedMetadata` payload channel + `NodeFlags` marker-flag channel
//!   (`Macro` + `ADDRESS_TAKEN` co-occur on the same node).
//! - U03 — `SQRY_GRAPH_V11` magic + `GraphSnapshotDataV11` envelope.
//! - U04 — `EdgeKind::Calls.resolved_via: ResolvedVia` (`Direct` /
//!   `TypeMatch` / `BindingPlane`).
//!
//! # Test design
//!
//! 1. Build a synthetic `CodeGraph` carrying every V11 wire surface:
//!    - One `Function` node with `TypedMetadata::Macro(...)`.
//!    - One `Function` node with `NodeFlags::SYNTHETIC | ADDRESS_TAKEN`.
//!    - One `Method` node with both `TypedMetadata::Macro` AND
//!      `NodeFlags::ADDRESS_TAKEN` (co-occurrence: the design's whole point).
//!    - One `EdgeKind::Calls` edge for each `ResolvedVia` variant
//!      (`Direct`, `TypeMatch`, `BindingPlane`).
//! 2. Save the graph through the public `save_to_path` API. Load it back.
//!    Verify every wire surface survives the round-trip via public
//!    accessors (`get_typed`, `is_address_taken`, `is_synthetic`,
//!    `Calls.resolved_via`).
//! 3. Save the same constructed graph twice in quick succession to two
//!    fresh paths and assert byte-identical re-save. This proves the
//!    encoder is deterministic given the same input — the round-trip
//!    integrity claim above demonstrates that the load → save composition
//!    preserves all V11 channels.
//!
//! # On "byte-identical" and the documented nondeterministic surface
//!
//! `save_to_path` stamps a monotonic `fact_epoch` (see
//! `sqry-core/src/graph/unified/persistence/snapshot.rs::next_fact_epoch`,
//! introduced by Phase 1 fact layer P1U06). The epoch is computed as
//! `max(prev_epoch + 1, SystemTime::now().as_secs())` so a save → load →
//! save sequence ALWAYS advances the epoch by at least one tick. That
//! advance propagates into the header's `timestamp` + `fact_epoch`
//! fields, the per-file `indexed_at` slot (`stamp_file_indexed_at`), and
//! the `last_seen_epoch` field of every `NodeProvenance` /
//! `EdgeProvenance` record (`merge_provenance_from_snapshot`). Those
//! propagations are documented monotone behaviour — not "ordering
//! nondeterminism" — so we MUST NOT mask them by zeroing the affected
//! bytes; doing so would silently hide a real wire-format regression.
//!
//! Instead this test uses two complementary assertions:
//!
//! - **Structural round-trip** — load-then-compare-by-accessor proves
//!   every V11 channel survives serialization. This is the binding
//!   correctness claim for U06.
//! - **Deterministic encode** — saving the SAME constructed graph twice
//!   within a single wall-clock second to two fresh paths must produce
//!   byte-identical files (prev_epoch is `0` on both fresh paths,
//!   `now_secs` is identical inside one second, every stamped field
//!   converges on the same value, and postcard encoding of identical
//!   input is deterministic). This is the wire-format byte-identity
//!   claim. If the two saves straddle a second boundary the test retries
//!   inside the same loop — the retry budget is small but sufficient
//!   given each save completes in well under a second on the synthetic
//!   graph.
//!
//! # Out of scope
//!
//! - `derived.sqry` (sqry-db companion cache) soft-miss behaviour on the
//!   SHA-256 change after the V11 bump is a daemon-owned concern: only
//!   `sqryd` writes that file via `QueryDbHook`, and this integration
//!   test does NOT spin up a daemon. The contract is described in
//!   `CLAUDE.md` §"Derived Analysis DB" — the V11 magic change forces a
//!   one-off soft-miss + rebuild on the first daemon publish after
//!   upgrade. Verifying that handoff lives in the daemon test suite, not
//!   in a sqry-core integration test.
//!
//! Spec / design references:
//! - `docs/development/c-semantic-phase-a-icall-precision/03_IMPLEMENTATION_PLAN-...md`
//!   §"U6 — sqry-core wire changes integration (round-trip)" (line 360).
//! - `docs/superpowers/plans/2026-05-14-c-semantic-phase-a-icall-precision-dag.toml`
//!   `[units.U06_WIRE_ROUNDTRIP]`.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqry_core::graph::CodeGraph;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::storage::{MacroNodeMetadata, TypedMetadata};
use tempfile::TempDir;

/// Tags applied to each constructed node so the test can re-resolve them by
/// name after a save → load round-trip.
const MACRO_FN_NAME: &str = "macro_generated_fn";
const SYNTHETIC_ADDR_FN_NAME: &str = "synthetic_address_taken_fn";
const COOCCUR_METHOD_NAME: &str = "macro_and_address_taken_method";

/// Build the synthetic graph carrying every Phase A V11 wire surface.
///
/// Returns the populated graph plus the three caller / callee NodeIds we
/// allocated, so the test can index into the metadata store + edge store
/// without re-walking the arena.
fn build_synthetic_v11_graph() -> SyntheticGraph {
    let mut graph = CodeGraph::new();

    // File registry: register a single C source file. The file slot is what
    // every edge attaches to, so all three edges live in the same file
    // partition — keeping the test deterministic across delta-buffer layout
    // changes.
    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/icall.c"), Some(Language::C))
        .expect("file registration succeeds");

    // ---- Nodes ----
    //
    // Three "callee" functions, one per ResolvedVia variant, plus the
    // three nodes that exercise the U02 metadata channels. The same
    // node can serve both roles for the metadata-channel checks (the
    // Macro + ADDRESS_TAKEN co-occurrence node is also the BindingPlane
    // callee), but to keep the test legible we allocate dedicated slots.

    let macro_fn_id = alloc_node(
        &mut graph,
        MACRO_FN_NAME,
        "test::icall::macro_generated_fn",
        NodeKind::Function,
        file_id,
        1,
        5,
    );

    let synthetic_addr_fn_id = alloc_node(
        &mut graph,
        SYNTHETIC_ADDR_FN_NAME,
        "test::icall::synthetic_address_taken_fn",
        NodeKind::Function,
        file_id,
        7,
        12,
    );

    let cooccur_method_id = alloc_node(
        &mut graph,
        COOCCUR_METHOD_NAME,
        "test::icall::macro_and_address_taken_method",
        NodeKind::Method,
        file_id,
        14,
        20,
    );

    // A caller node — source of all three Calls edges. Carries no
    // metadata; we only need a valid NodeId pointing at a Function-kind
    // entry so the edge store accepts the edge.
    let caller_id = alloc_node(
        &mut graph,
        "caller_fn",
        "test::icall::caller_fn",
        NodeKind::Function,
        file_id,
        22,
        40,
    );

    // ---- Metadata ----
    //
    // U02 surfaces: TypedMetadata::Macro + NodeFlags bits.

    let macro_payload = MacroNodeMetadata {
        macro_generated: Some(true),
        macro_source: Some("DEFINE_HANDLER".to_string()),
        cfg_condition: Some("CONFIG_TEST".to_string()),
        cfg_active: Some(true),
        proc_macro_kind: None,
        expansion_cached: Some(false),
        unresolved_attributes: vec!["__weak".to_string()],
    };

    // macro_fn: Macro typed payload, no flags.
    graph
        .macro_metadata_mut()
        .insert_typed(macro_fn_id, TypedMetadata::Macro(macro_payload.clone()));

    // synthetic_addr_fn: SYNTHETIC | ADDRESS_TAKEN flags, no typed payload.
    graph
        .macro_metadata_mut()
        .mark_synthetic(synthetic_addr_fn_id);
    graph
        .macro_metadata_mut()
        .mark_address_taken(synthetic_addr_fn_id);

    // cooccur_method: BOTH channels — Macro typed payload AND ADDRESS_TAKEN
    // flag. This is the DEC:c-icall-precision-001 co-occurrence case.
    graph.macro_metadata_mut().insert_typed(
        cooccur_method_id,
        TypedMetadata::Macro(macro_payload.clone()),
    );
    graph
        .macro_metadata_mut()
        .mark_address_taken(cooccur_method_id);

    // ---- Edges ----
    //
    // U04 surface: one Calls edge per ResolvedVia variant. We assert in
    // the round-trip path that exactly one edge of each variant survives,
    // so the targets are distinct.

    graph.edges_mut().add_edge(
        caller_id,
        macro_fn_id,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_id,
    );

    graph.edges_mut().add_edge(
        caller_id,
        synthetic_addr_fn_id,
        EdgeKind::Calls {
            argument_count: 2,
            is_async: false,
            resolved_via: ResolvedVia::TypeMatch,
        },
        file_id,
    );

    graph.edges_mut().add_edge(
        caller_id,
        cooccur_method_id,
        EdgeKind::Calls {
            argument_count: 1,
            is_async: true,
            resolved_via: ResolvedVia::BindingPlane,
        },
        file_id,
    );

    SyntheticGraph { graph }
}

struct SyntheticGraph {
    graph: CodeGraph,
}

/// Allocate a node and register it in the auxiliary indices.
///
/// Mirrors the helper pattern used by
/// `sqry-core/tests/unified_graph_indices_persistence_test.rs::create_test_graph`.
fn alloc_node(
    graph: &mut CodeGraph,
    name: &str,
    qname: &str,
    kind: NodeKind,
    file_id: sqry_core::graph::unified::FileId,
    start_line: u32,
    end_line: u32,
) -> sqry_core::graph::unified::NodeId {
    let name_id = graph.strings_mut().intern(name).expect("intern node name");
    let qname_id = graph
        .strings_mut()
        .intern(qname)
        .expect("intern node qualified name");

    let entry = NodeEntry::new(kind, name_id, file_id)
        .with_location(start_line, 0, end_line, 0)
        .with_qualified_name(qname_id);

    let node_id = graph
        .nodes_mut()
        .alloc(entry.clone())
        .expect("alloc node into arena");

    graph.indices_mut().add(
        node_id,
        entry.kind,
        entry.name,
        entry.qualified_name,
        entry.file,
    );

    node_id
}

/// Re-resolve a node by its interned name string, returning the live
/// NodeId from the loaded graph's arena.
fn resolve_by_name(graph: &CodeGraph, name: &str) -> sqry_core::graph::unified::NodeId {
    let strings = graph.strings();
    let name_id = strings
        .get(name)
        .unwrap_or_else(|| panic!("expected interned name {name:?} present in loaded graph"));
    for (node_id, entry) in graph.nodes().iter() {
        if entry.name == name_id {
            return node_id;
        }
    }
    panic!("no live node found for name {name:?} after load");
}

/// Verify that every V11 wire surface in `graph` matches the synthetic
/// fixture we constructed. Called twice: once on the original constructed
/// graph (sanity check) and once on the loaded-from-disk graph (the actual
/// round-trip claim).
fn assert_v11_surfaces_present(graph: &CodeGraph, context: &str) {
    let metadata = graph.macro_metadata();

    // U02 — Macro typed payload alone.
    let macro_fn = resolve_by_name(graph, MACRO_FN_NAME);
    let typed = metadata
        .get_typed(macro_fn)
        .unwrap_or_else(|| panic!("{context}: macro_fn missing TypedMetadata"));
    match typed {
        TypedMetadata::Macro(m) => {
            assert_eq!(
                m.macro_source.as_deref(),
                Some("DEFINE_HANDLER"),
                "{context}: macro_fn macro_source"
            );
            assert_eq!(
                m.cfg_condition.as_deref(),
                Some("CONFIG_TEST"),
                "{context}: macro_fn cfg_condition"
            );
            assert_eq!(
                m.unresolved_attributes,
                vec!["__weak".to_string()],
                "{context}: macro_fn unresolved_attributes"
            );
        }
        TypedMetadata::Classpath(_) => {
            panic!("{context}: macro_fn typed payload is Classpath, expected Macro")
        }
    }
    assert!(
        metadata.get_flags(macro_fn).is_empty(),
        "{context}: macro_fn must carry no flags"
    );

    // U02 — Flag bits alone (SYNTHETIC | ADDRESS_TAKEN).
    let synthetic_addr_fn = resolve_by_name(graph, SYNTHETIC_ADDR_FN_NAME);
    assert!(
        metadata.is_synthetic(synthetic_addr_fn),
        "{context}: synthetic_addr_fn must carry SYNTHETIC flag"
    );
    assert!(
        metadata.is_address_taken(synthetic_addr_fn),
        "{context}: synthetic_addr_fn must carry ADDRESS_TAKEN flag"
    );
    assert!(
        metadata.get_typed(synthetic_addr_fn).is_none(),
        "{context}: synthetic_addr_fn must carry NO typed payload"
    );

    // U02 — Co-occurrence: Macro typed payload AND ADDRESS_TAKEN flag.
    let cooccur_method = resolve_by_name(graph, COOCCUR_METHOD_NAME);
    let typed = metadata
        .get_typed(cooccur_method)
        .unwrap_or_else(|| panic!("{context}: cooccur_method missing typed payload"));
    assert!(
        matches!(typed, TypedMetadata::Macro(_)),
        "{context}: cooccur_method typed payload must be Macro"
    );
    assert!(
        metadata.is_address_taken(cooccur_method),
        "{context}: cooccur_method must carry ADDRESS_TAKEN flag"
    );
    // Co-occurrence specifically excludes SYNTHETIC for this node.
    assert!(
        !metadata.is_synthetic(cooccur_method),
        "{context}: cooccur_method must NOT carry SYNTHETIC"
    );

    // U04 — every ResolvedVia variant exercised on a live Calls edge.
    let edges = graph.edges().all_live_forward_edges();
    let calls_edges: Vec<&EdgeKind> = edges
        .iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, EdgeKind::Calls { .. }))
        .collect();
    assert_eq!(
        calls_edges.len(),
        3,
        "{context}: expected exactly 3 Calls edges (one per ResolvedVia), found {}",
        calls_edges.len()
    );

    let mut have_direct = false;
    let mut have_typematch = false;
    let mut have_bindingplane = false;
    for k in calls_edges {
        if let EdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via,
        } = k
        {
            match resolved_via {
                ResolvedVia::Direct => {
                    assert!(!have_direct, "{context}: duplicate ResolvedVia::Direct");
                    have_direct = true;
                    assert_eq!(*argument_count, 0, "{context}: Direct edge argument_count");
                    assert!(!*is_async, "{context}: Direct edge is_async");
                }
                ResolvedVia::TypeMatch => {
                    assert!(
                        !have_typematch,
                        "{context}: duplicate ResolvedVia::TypeMatch"
                    );
                    have_typematch = true;
                    assert_eq!(
                        *argument_count, 2,
                        "{context}: TypeMatch edge argument_count"
                    );
                    assert!(!*is_async, "{context}: TypeMatch edge is_async");
                }
                ResolvedVia::BindingPlane => {
                    assert!(
                        !have_bindingplane,
                        "{context}: duplicate ResolvedVia::BindingPlane"
                    );
                    have_bindingplane = true;
                    assert_eq!(
                        *argument_count, 1,
                        "{context}: BindingPlane edge argument_count"
                    );
                    assert!(*is_async, "{context}: BindingPlane edge is_async");
                }
                // V12 dispatch-resolver provenances. This fixture
                // exercises V11 wire compatibility (V11 → V12 upconvert
                // preserves the 3-variant V11 set untouched); the new
                // variants are unreachable in the fixture itself but
                // must be matched exhaustively per V12 contract.
                ResolvedVia::VirtualDispatch
                | ResolvedVia::InterfaceDispatch
                | ResolvedVia::DuckTyped
                | ResolvedVia::Structural
                | ResolvedVia::PromiscuousElided => {
                    panic!(
                        "{context}: V12 dispatch-resolver provenance unexpectedly present in V11 round-trip fixture: {resolved_via:?}"
                    );
                }
            }
        }
    }
    assert!(have_direct, "{context}: ResolvedVia::Direct missing");
    assert!(have_typematch, "{context}: ResolvedVia::TypeMatch missing");
    assert!(
        have_bindingplane,
        "{context}: ResolvedVia::BindingPlane missing"
    );

    // Defensive: confirm the metadata store actually exposes entries.
    // (NodeMetadataStore default ctor is empty; an empty store would
    // silently pass every accessor check above on a missing entry.)
    let entry_count = metadata.iter_entries().count();
    assert!(
        entry_count >= 3,
        "{context}: expected ≥3 metadata entries (macro_fn, synthetic_addr_fn, cooccur_method), \
         found {entry_count}"
    );
}

/// Returns the current wall-clock second.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Acceptance #1 / #2 / #4 — load-then-save preserves every V11 channel.
///
/// Save → load → verify-by-accessor → re-save → reload → verify-by-accessor.
/// This is the binding correctness claim: the V11 wire format faithfully
/// represents every Phase A surface (U02 + U03 + U04). The deterministic
/// byte-identity claim is exercised separately in
/// `byte_identical_resave_within_one_second`.
#[test]
fn v11_round_trip_preserves_all_wire_surfaces() {
    let SyntheticGraph { graph, .. } = build_synthetic_v11_graph();

    // Sanity: surfaces are present in the freshly constructed graph
    // before we hand it to persistence.
    assert_v11_surfaces_present(&graph, "before-save");

    let tmp = TempDir::new().expect("create tmpdir");
    let path1 = tmp.path().join("snapshot1.sqry");

    save_to_path(&graph, &path1).expect("save_to_path #1 succeeds");
    let loaded = load_from_path(&path1, None).expect("load_from_path #1 succeeds");

    assert_v11_surfaces_present(&loaded, "after-first-load");

    // Re-save the loaded graph to a separate path and reload — proves the
    // load → save → load composition is closed under V11.
    let path2 = tmp.path().join("snapshot2.sqry");
    save_to_path(&loaded, &path2).expect("save_to_path #2 succeeds");
    let reloaded = load_from_path(&path2, None).expect("load_from_path #2 succeeds");

    assert_v11_surfaces_present(&reloaded, "after-second-load");
}

/// Acceptance #1 (byte-identity sub-claim) — the encoder is deterministic.
///
/// Save the SAME constructed graph twice to two fresh paths in quick
/// succession. The two on-disk files must be byte-identical.
///
/// Why this proves what we need:
///
/// - Both saves target fresh paths, so `next_fact_epoch` reads no prior
///   epoch (`prev_epoch == 0`) and resolves to
///   `max(0 + 1, now_secs()) == now_secs()` in BOTH calls.
/// - `header.timestamp = SystemTime::now().as_secs()` is sampled inside
///   `GraphHeader::new` and converges on the same value within a single
///   wall-clock second.
/// - `stamp_file_indexed_at` writes the same `epoch` into every file
///   slot in both saves.
/// - `build_provenance_from_snapshot` is called for both saves
///   (`snapshot.fact_epoch() == 0` on the freshly constructed graph,
///   selecting the build-fresh branch in `resolve_provenance`), and
///   stamps the same `epoch` into every provenance record.
/// - `postcard::to_allocvec` is deterministic given identical input.
///
/// The only failure mode is a second boundary crossed between the two
/// saves — we retry the loop up to a small budget to absorb that.
///
/// If, in a future change, a HashMap or other source of iteration-order
/// nondeterminism were to bleed into the encoded bytes, this test would
/// fail on the first iteration and direct the reviewer to the real
/// regression (instead of being papered over with a normalisation pass).
#[test]
fn byte_identical_resave_within_one_second() {
    const RETRY_BUDGET: u32 = 5;

    for attempt in 0..RETRY_BUDGET {
        let SyntheticGraph { graph, .. } = build_synthetic_v11_graph();

        let tmp = TempDir::new().expect("create tmpdir");
        let path_a = tmp.path().join("attempt_a.sqry");
        let path_b = tmp.path().join("attempt_b.sqry");

        let before = now_secs();
        save_to_path(&graph, &path_a).expect("save #A succeeds");
        save_to_path(&graph, &path_b).expect("save #B succeeds");
        let after = now_secs();

        // If we straddled a wall-clock second, the two saves can land on
        // different `now_secs()` values inside GraphHeader::new and
        // next_fact_epoch. Retry without flagging the run as a failure —
        // each save completes in well under a second on this synthetic
        // graph, so the retry budget is overkill for the actual race
        // window.
        if before != after {
            // Backoff far less than one second so we comfortably fit
            // RETRY_BUDGET attempts inside any reasonable CI step.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let bytes_a = std::fs::read(&path_a).expect("read attempt_a.sqry");
        let bytes_b = std::fs::read(&path_b).expect("read attempt_b.sqry");

        assert_eq!(
            bytes_a.len(),
            bytes_b.len(),
            "attempt {attempt}: V11 encoder produced different-length outputs from identical \
             input graphs (within one wall-clock second, fresh paths) — encoder is \
             nondeterministic; this is a wire-format regression",
        );
        assert_eq!(
            bytes_a, bytes_b,
            "attempt {attempt}: V11 encoder produced different bytes from identical input \
             graphs (within one wall-clock second, fresh paths) — encoder is \
             nondeterministic; this is a wire-format regression",
        );

        return;
    }

    panic!(
        "byte_identical_resave_within_one_second: exhausted {RETRY_BUDGET} retries crossing \
         wall-clock second boundaries; either CI is starved or there is a real timing bug",
    );
}
