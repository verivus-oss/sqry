//! U04 build-seam end-to-end (M1 boundary).
//!
//! Proves two things through the REAL index pipeline with the REAL Rust plugin:
//!
//! 1. Identifier-blind shape descriptors are computed in the staging seam and
//!    flow through `take_macro_metadata -> rekey_staging_metadata_to_arena ->
//!    merge` into the committed `CodeGraph::macro_metadata()` (a non-empty
//!    `shape_descriptors` side table with the expected control-flow histogram).
//!
//! 2. AC-5 no-regression guard: the shape feature running alongside `body_hash`
//!    does not perturb the find_duplicates Body ladder. `body_hash` stays
//!    byte-sensitive (two byte-identical bodies group; a structurally-identical
//!    but textually-different body does NOT), exactly as before this feature.
//!    This is the integration counterpart to the `body_hash` unit test
//!    `test_body_hash_different_content` (return 42 != return 43), which is left
//!    untouched.

use std::fs;

use sqry_core::graph::unified::build::shape::CfBucket;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::storage::shape::ShapeDescriptor;
use sqry_core::plugin::LanguagePlugin;
use sqry_core::plugin::PluginManager;
use sqry_core::query::{DuplicateConfig, DuplicateType, build_duplicate_groups_graph};
use sqry_lang_cpp::CppPlugin;
use sqry_lang_python::PythonPlugin;
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

fn plugin_manager() -> PluginManager {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));
    manager
}

fn index_rust(source: &str) -> CodeGraph {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join("lib.rs"), source).expect("write fixture");
    let plugins = plugin_manager();
    let config = BuildConfig::default();
    build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds")
}

/// Index a single file through the REAL pipeline with one registered plugin.
fn index_one(plugin: Box<dyn LanguagePlugin>, file_name: &str, source: &str) -> CodeGraph {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join(file_name), source).expect("write fixture");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(plugin);
    let config = BuildConfig::default();
    build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds")
}

/// The one committed descriptor whose histogram satisfies `pred`. Panics unless
/// exactly one matches, so a fixture that accidentally grows a second matching
/// body is caught rather than silently picking one.
fn only_descriptor<'g>(
    graph: &'g CodeGraph,
    what: &str,
    pred: impl Fn(&ShapeDescriptor) -> bool,
) -> &'g ShapeDescriptor {
    let metadata = graph.macro_metadata();
    let mut hits = metadata.shape_descriptors().values().filter(|d| pred(d));
    let found = hits
        .next()
        .unwrap_or_else(|| panic!("no descriptor for {what}"));
    assert!(
        hits.next().is_none(),
        "expected exactly one descriptor for {what}"
    );
    found
}

#[test]
fn descriptors_computed_and_stored_end_to_end_for_rust() {
    // `classify` exercises branch + loop + match + return + call; `helper` is a
    // small function. Both should carry descriptors after a real index.
    let source = r#"
pub fn classify(n: i32) -> i32 {
    let mut total = 0;
    for i in 0..n {
        if i % 2 == 0 {
            total += helper(i);
        }
    }
    match total {
        0 => return 0,
        _ => {}
    }
    total
}

fn helper(x: i32) -> i32 {
    x + 1
}
"#;
    let graph = index_rust(source);
    let metadata = graph.macro_metadata();
    let descriptors = metadata.shape_descriptors();

    assert!(
        !descriptors.is_empty(),
        "shape descriptors must reach the committed graph end-to-end"
    );

    // Find the `classify` descriptor by its control-flow signature (branch + loop +
    // match together). Identifier-blind, so we match on structure, not names.
    let classify = descriptors
        .values()
        .find(|d| {
            d.cf_histogram[CfBucket::Branch.index()] >= 1
                && d.cf_histogram[CfBucket::Loop.index()] >= 1
                && d.cf_histogram[CfBucket::Match.index()] >= 1
        })
        .expect("classify descriptor with branch+loop+match present");

    assert!(
        classify.cf_histogram[CfBucket::Return.index()] >= 1,
        "classify has a return"
    );
    assert!(
        classify.cf_histogram[CfBucket::Call.index()] >= 1,
        "classify calls helper"
    );
    assert!(!classify.is_unhashable(), "classify body is hashable");
    assert_eq!(
        classify.signature_shape.arity_positional, 1,
        "classify takes one positional parameter"
    );
    assert!(
        classify.signature_shape.has_return_annotation,
        "classify declares a return type"
    );
}

#[test]
fn ac5_body_hash_duplicates_unaffected_by_shape_feature() {
    // `a::run` and `b::run` are byte-identical function items (exact body-hash
    // duplicates). `c::run` is STRUCTURALLY identical but textually different
    // (renamed binding, different literal), so it shares the shape but NOT the
    // body bytes. Body-mode duplicate detection (driven by `body_hash`) must group
    // ONLY the exact pair, proving the Body ladder is untouched by the shape
    // feature running in the same seam (AC-5).
    let source = r#"
mod a {
    pub fn run(x: i32) -> i32 {
        let y = x + 1;
        if y > 0 {
            return y;
        }
        y
    }
}

mod b {
    pub fn run(x: i32) -> i32 {
        let y = x + 1;
        if y > 0 {
            return y;
        }
        y
    }
}

mod c {
    pub fn run(input: i32) -> i32 {
        let total = input + 99;
        if total > 0 {
            return total;
        }
        total
    }
}
"#;
    let graph = index_rust(source);

    // The shape feature IS active: descriptors were produced for these bodies.
    assert!(
        !graph.macro_metadata().shape_descriptors().is_empty(),
        "precondition: shape descriptors are being computed"
    );

    // All three `run` functions are structurally identical, so their shape_hash
    // must match (sanity check that shape is rename/relocate-invariant), while
    // body_hash below distinguishes them.
    let shape_hashes: Vec<_> = graph
        .macro_metadata()
        .shape_descriptors()
        .values()
        .filter(|d| {
            d.cf_histogram[CfBucket::Branch.index()] >= 1
                && d.cf_histogram[CfBucket::Return.index()] >= 1
        })
        .map(|d| d.shape_hash)
        .collect();
    assert!(
        shape_hashes.len() >= 3,
        "all three run bodies carry a descriptor, got {}",
        shape_hashes.len()
    );
    assert!(
        shape_hashes.windows(2).all(|w| w[0] == w[1]),
        "structurally-identical bodies must share one shape_hash"
    );

    // Body-mode duplicate detection: exactly one group, the exact-bytes pair.
    let config = DuplicateConfig::default();
    let groups = build_duplicate_groups_graph(DuplicateType::Body, &graph, &config);
    assert_eq!(
        groups.len(),
        1,
        "only the byte-identical pair forms a body-hash duplicate group, got {groups:?}"
    );
    assert_eq!(
        groups[0].node_ids.len(),
        2,
        "the body-hash group is exactly {{a::run, b::run}}"
    );
}

#[test]
fn ac5_body_hash_unit_invariant_still_holds() {
    // Mirror of `graph::body_hash::tests::test_body_hash_different_content` at the
    // integration boundary: `return 42` and `return 43` must differ under
    // body_hash even though their shapes are identical. Guards against a future
    // unit accidentally normalising literals into the Body ladder.
    use sqry_core::graph::body_hash::BodyHash128;
    let h42 = BodyHash128::compute(b"fn example() { return 42; }");
    let h43 = BodyHash128::compute(b"fn example() { return 43; }");
    assert_ne!(h42, h43, "body_hash must stay literal-sensitive (AC-5)");
}

/// AC-6: a structurally-equivalent C++ and Python function produce comparable
/// descriptors under the one canonical [`CfBucket`] schema.
///
/// This indexes both fixtures through the REAL pipeline (the actual `CppPlugin`
/// and `PythonPlugin` ShapeMappings + the shared walker) and COMPARES the two
/// committed histograms directly. It is not two separate per-language
/// well-formedness checks: the cross-language claim is only meaningful if the
/// C++ and Python descriptors of the same structure are put side by side.
///
/// Note on scope: `cf_histogram` is the language-neutral surface (its indices are
/// canonical buckets shared by every plugin), so it is the field that can be
/// compared across languages. `shape_hash` and `minhash` are computed over
/// tree-sitter grammar `kind_id`s, which differ per grammar, so they are
/// deliberately NOT cross-compared (a Python `if_statement` and a C++
/// `if_statement` carry different grammar ids). That is exactly why AC-6 speaks
/// of "comparable", not "byte-identical", descriptors.
#[test]
fn ac6_cpp_and_python_equivalent_functions_share_canonical_histogram() {
    const PY: &str = include_str!("../../test-fixtures/shape/cross-language/equiv.py");
    const CPP: &str = include_str!("../../test-fixtures/shape/cross-language/equiv.cpp");

    let py_graph = index_one(Box::new(PythonPlugin::default()), "equiv.py", PY);
    let cpp_graph = index_one(Box::new(CppPlugin::default()), "equiv.cpp", CPP);

    let b = CfBucket::Branch.index();
    let l = CfBucket::Loop.index();
    let r = CfBucket::Return.index();
    let c = CfBucket::Call.index();
    let assign = CfBucket::Assign.index();

    // `branchy`: branches/calls/returns only, no loop, no binding. Pick it by its
    // structural signature (two branches, no loop) in each graph.
    let branchy_pred = |d: &ShapeDescriptor| {
        d.cf_histogram[b] == 2 && d.cf_histogram[l] == 0 && d.cf_histogram[r] == 3
    };
    let branchy_py = only_descriptor(&py_graph, "python branchy", branchy_pred);
    let branchy_cpp = only_descriptor(&cpp_graph, "cpp branchy", branchy_pred);

    // The whole point: a FULL canonical-histogram identity across two languages
    // for a branch/call/return-only body (no loop-variable binding to perturb the
    // Assign bucket). Every one of the 15 buckets is equal.
    assert_eq!(
        branchy_py.cf_histogram, branchy_cpp.cf_histogram,
        "AC-6: branch-only equivalent functions must share the full canonical histogram across C++/Python"
    );
    assert!(!branchy_py.is_unhashable() && !branchy_cpp.is_unhashable());
    // Non-trivial: the shared histogram is actually populated, not all zeros.
    assert!(branchy_py.cf_histogram[b] >= 1 && branchy_py.cf_histogram[c] >= 1);

    // `classify`: adds a loop. The two agree on EVERY control-flow bucket; they
    // differ only in Assign, because the C++ range-for `for (int item : items)`
    // declares a loop variable (an Assign) while Python's `for item in items`
    // binds without an assignment node. That single-bucket idiom difference is
    // the honest "comparable, not identical" boundary AC-6 draws.
    let classify_pred = |d: &ShapeDescriptor| d.cf_histogram[b] == 1 && d.cf_histogram[l] == 1;
    let classify_py = only_descriptor(&py_graph, "python classify", classify_pred);
    let classify_cpp = only_descriptor(&cpp_graph, "cpp classify", classify_pred);

    for (bucket, (py_n, cpp_n)) in classify_py
        .cf_histogram
        .iter()
        .zip(classify_cpp.cf_histogram.iter())
        .enumerate()
    {
        if bucket == assign {
            continue; // documented language-idiom bucket; checked separately below
        }
        assert_eq!(
            py_n, cpp_n,
            "AC-6: classify must agree on canonical bucket {bucket} across C++/Python"
        );
    }
    // The shared control-flow structure is real (each of these buckets > 0 in both).
    for idx in [b, l, r, c] {
        assert!(classify_py.cf_histogram[idx] >= 1 && classify_cpp.cf_histogram[idx] >= 1);
    }
    // And the one documented divergence is exactly the expected direction: the C++
    // range-for binds a loop variable that Python does not.
    assert!(
        classify_cpp.cf_histogram[assign] >= classify_py.cf_histogram[assign],
        "the C++ range-for loop-variable declaration is the only added Assign"
    );
    assert!(!classify_py.is_unhashable() && !classify_cpp.is_unhashable());
}
