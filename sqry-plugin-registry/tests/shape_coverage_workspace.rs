//! U15 — AC-1 workspace-wide coverage gate.
//!
//! The per-plugin coverage fixtures (M3, in-crate) prove each of the 37 language
//! plugins ships a `ShapeMapping`. This test proves the *integration invariant*
//! over a real multi-language index built with the full built-in roster
//! (`create_plugin_manager_all`): in a committed graph,
//!
//!   1. every descriptor carries `callee_shape == Unresolved` (the dependent
//!      extension is present-but-unresolved this effort, never silently zeroed);
//!   2. every descriptor attaches to a Function/Method node (no stray descriptors
//!      on the wrong kind or on a tombstone); and
//!   3. every NAMED function/method definition (non-synthetic, valid body span,
//!      a resolvable name) carries a committed descriptor — i.e. the seam never
//!      silently drops a real body.
//!
//! Scope note (3): "named definition" deliberately excludes two node classes that
//! carry a Function/Method kind but are not real definitions the feature targets:
//!   - anonymous/lambda nodes (empty name) and the call-expression pseudo-function
//!     nodes several plugins emit (a `foo()` call modelled as a Function named
//!     `foo`). Cross-file unification collapses common call names (`map`, `filter`,
//!     `get`, ...), and an anonymous node's recorded span often does not correspond
//!     to a clean tree-sitter subtree, so these legitimately fall under the
//!     conservative "skip rather than fingerprint the wrong node" contract.
//!     Since issue #748 the call-expression class is excluded by asking the build
//!     rather than by name shape: such a node holds a CALL SITE's extent, is
//!     denied a `body_hash`, and the shape seam declines it in the same breath,
//!     so eligibility here keys on `body_hash` being present. That makes this
//!     assertion the lock-step check between the two halves of the body plane.
//!   - the R plugin records function-node spans off-by-one: `span_from_points`
//!     (sqry-lang-r/src/relations/graph_builder.rs:433) already converts the
//!     tree-sitter row to 1-indexed (`start.row + 1`), and the shared span->location
//!     conversion in `add_node_internal` (sqry-core/.../build/helper.rs:864) then
//!     adds `+1` again. The double increment shifts a real function's recorded start
//!     line down by one (e.g. `classify` lands on L4, the body's first statement,
//!     instead of L3, the `name <- function(...)` definition), so the recorded span
//!     matches no single tree-sitter node and `descriptor_for`'s exact-span guard
//!     conservatively skips it. This is a pre-existing R-plugin span bug (it equally
//!     skews body_hash's R byte extraction), independent of the body-shape feature,
//!     filed as an R-plugin follow-up. It is the one allowlisted named exception
//!     (`SPAN_QUIRK_EXTENSIONS`); any named miss in any other language fails the
//!     test, so a real coverage regression is still caught.
//!
//! The active roster depends on Cargo features: under the default build the seven
//! specialty plugins (apex, abap, servicenow, terraform, pulumi, puppet) are not
//! compiled in, so their fixtures are skipped; with `--features specialty-plugins`
//! the workspace spans all 37. The invariant is identical either way; the test
//! adapts to whichever roster is active and asserts a meaningful coverage floor so
//! a silent "no descriptors produced" regression fails loudly.

use std::path::PathBuf;

use sqry_core::graph::unified::build::body_hash::has_valid_body_span;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::shape::CalleeShape;
use sqry_plugin_registry::create_plugin_manager_all;

/// Source-tree `test-fixtures/shape` directory (read-only; `build_unified_graph`
/// is the pure in-memory builder and writes nothing).
fn shape_fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root is the parent of sqry-plugin-registry")
        .join("test-fixtures/shape")
}

#[test]
fn ac1_every_eligible_function_carries_a_descriptor_across_the_roster() {
    let root = shape_fixtures_root();
    assert!(root.is_dir(), "fixture tree missing at {}", root.display());

    let plugins = create_plugin_manager_all();
    let graph = build_unified_graph(&root, &plugins, &BuildConfig::default())
        .expect("multi-language fixture workspace builds");
    let snapshot = graph.snapshot();
    let descriptors = snapshot.macro_metadata().shape_descriptors();

    // (1) + (2): every descriptor is Unresolved and lives on a Function/Method
    // node (descriptors never strand on the wrong kind or a tombstone).
    for (node_id, descriptor) in descriptors {
        assert_eq!(
            descriptor.callee_shape,
            CalleeShape::Unresolved,
            "AC-1: callee_shape must be Unresolved everywhere this effort (node {node_id:?})"
        );
        let entry = snapshot
            .nodes()
            .get(*node_id)
            .expect("a descriptor's node must be live (no descriptor on a tombstone)");
        assert!(
            matches!(entry.kind, NodeKind::Function | NodeKind::Method),
            "AC-1: descriptors attach only to Function/Method nodes, found {:?}",
            entry.kind
        );
    }

    // (3): every NAMED function/method definition has a committed descriptor.
    // R's off-by-one function-span recording (span_from_points + add_node_internal
    // both increment the start line) is the single allowlisted quirk.
    let mut named_definitions = 0usize;
    let mut covered = 0usize;
    let mut files_with_descriptors = std::collections::HashSet::new();
    let mut misses: Vec<String> = Vec::new();
    let mut skipped_without_body_hash = 0usize;
    let mut stranded_descriptors: Vec<String> = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        // Synthetic nodes have no real tree-sitter span to fingerprint; the seam
        // (like body_hash) only fingerprints nodes with a concrete body span.
        if snapshot.is_node_synthetic(node_id) || !has_valid_body_span(entry) {
            continue;
        }
        let name = snapshot
            .strings()
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        // Anonymous nodes are out of scope too: an anonymous node's recorded span
        // often does not correspond to a clean tree-sitter subtree.
        if name.is_empty() {
            continue;
        }
        let file = snapshot
            .files()
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if has_span_quirk(&file) {
            continue;
        }
        // Call-expression pseudo-functions are out of scope (see the module
        // docs). Since issue #748 the build answers this directly: a node whose
        // extent came from a call site is denied a body hash, and the shape seam
        // declines it in the same breath.
        //
        // The skip is COUNTED, not silent. Keying eligibility on the value under
        // test would let a future bug that suppresses BOTH halves for a real
        // declaration pass unnoticed, so the skipped population is held to two
        // separate assertions below: none of it may carry a descriptor (the
        // lock-step half), and it may not grow without someone looking.
        if entry.body_hash.is_none() {
            skipped_without_body_hash += 1;
            if descriptors.contains_key(&node_id) {
                stranded_descriptors.push(format!("{name} ({file}) {:?}", entry.kind));
            }
            continue;
        }
        named_definitions += 1;
        if descriptors.contains_key(&node_id) {
            covered += 1;
            files_with_descriptors.insert(entry.file);
        } else if misses.len() < 20 {
            misses.push(format!("{name} ({file}) {:?}", entry.kind));
        }
    }

    assert!(
        misses.is_empty(),
        "AC-1: {} of {named_definitions} named Function/Method definitions lack a committed \
         descriptor (outside the allowlisted span-quirk languages); misses: {misses:?}",
        named_definitions - covered
    );

    // The two guards on the skipped population, so eligibility keyed on
    // `body_hash` cannot quietly absorb a real regression.
    assert!(
        stranded_descriptors.is_empty(),
        "AC-1 lock-step: a node the body plane declined still carries a shape \
         descriptor: {stranded_descriptors:?}"
    );
    assert!(
        SKIPPED_WITHOUT_BODY_HASH_BAND.contains(&skipped_without_body_hash),
        "AC-1: {skipped_without_body_hash} named Function/Method nodes with a valid \
         body extent were skipped for lack of a body hash, outside the expected \
         band {SKIPPED_WITHOUT_BODY_HASH_BAND:?}. That population is \
         call-expression pseudo-functions. Growth means either a new stub family \
         or a real declaration losing both halves of the body plane; a collapse \
         means the build stopped minting those stubs. Investigate before moving \
         the band."
    );

    // Coverage floor: the multi-language workspace must actually produce a
    // substantial body of descriptors spanning several source files, so a silent
    // regression that stops computing descriptors fails here, not quietly.
    assert!(
        named_definitions >= 20,
        "expected a non-trivial named-definition population, got {named_definitions}"
    );
    assert_eq!(
        covered, named_definitions,
        "every named definition is covered"
    );
    assert!(
        files_with_descriptors.len() >= 8,
        "descriptors must span many source files (multi-language coverage), got {}",
        files_with_descriptors.len()
    );
}

/// File extensions whose plugin records a function's span in a way that does not
/// correspond to a single tree-sitter node, so the seam conservatively skips them
/// (see the module docs). Today this is only R: its `span_from_points`
/// (graph_builder.rs:433) double-increments the start line versus the shared
/// `add_node_internal` conversion (helper.rs:864), shifting every R function span
/// one line off the definition node. Kept as an explicit allowlist so any
/// regression in another language surfaces as a hard failure.
const SPAN_QUIRK_EXTENSIONS: &[&str] = &[".R", ".r"];

/// Band on named Function/Method nodes with a valid body extent that the body
/// plane declines. That population is call-expression pseudo-functions, which
/// hold a CALL SITE's extent and are denied a body hash since issue #748.
///
/// A two-sided band, not a loose ceiling. A ceiling of 100 over a measured 63
/// tolerated ~35 real declarations silently losing both halves before failing,
/// which is not an assertion. The lower bound matters too: a collapse means the
/// build stopped minting those stubs at all, which would be a different
/// regression wearing this test's green tick.
///
/// Re-measure and update deliberately when a fixture is added.
const SKIPPED_WITHOUT_BODY_HASH_BAND: std::ops::RangeInclusive<usize> = 55..=75;

fn has_span_quirk(file: &str) -> bool {
    SPAN_QUIRK_EXTENSIONS.iter().any(|ext| file.ends_with(ext))
}
