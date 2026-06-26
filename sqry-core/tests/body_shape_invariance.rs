//! U15 — rename-invariance (AC-2), reformat/comment-invariance (AC-3), and
//! two-process determinism for the body-shape descriptor.
//!
//! These are whole-feature gates: they index hand-written fixture pairs through
//! the REAL pipeline (the real Rust and Python plugins plus the shared walker)
//! and compare committed descriptors.
//!
//! - **AC-2** (rename): renaming every identifier and changing every literal in a
//!   body leaves `cf_histogram` and `shape_hash` byte-identical, because the
//!   walker hashes tree-sitter grammar `kind_id`s, never source text.
//! - **AC-3** (reformat): injecting comments and reflowing whitespace leaves the
//!   descriptor unchanged while `body_hash` (computed from the raw body bytes)
//!   DIFFERS. The same fixture proves both halves.
//! - **determinism**: the walker uses no wall-clock and no RNG, so two
//!   independent builds of the same source (each a fresh `CodeGraph`, no shared
//!   mutable state, which is what distinct OS processes also see) yield identical
//!   `shape_hash` and `minhash`.

use std::fs;

use sqry_core::graph::body_hash::BodyHash128;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::shape::ShapeDescriptor;
use sqry_core::plugin::{LanguagePlugin, PluginManager};
use sqry_lang_python::PythonPlugin;
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

/// Index one fixture file alone (its own tempdir + single registered plugin), so
/// nothing unifies across files and each named function keeps its own descriptor.
fn index_one(plugin: Box<dyn LanguagePlugin>, file_name: &str, source: &str) -> CodeGraph {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join(file_name), source).expect("write fixture");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(plugin);
    build_unified_graph(tmp.path(), &plugins, &BuildConfig::default())
        .expect("build_unified_graph succeeds")
}

/// The committed descriptor and `body_hash` for the Function/Method node named
/// `name`. Selecting by name (rather than "the only descriptor") is deliberate:
/// the Rust and Python plugins model call expressions as their own Function nodes
/// (e.g. a `scale(..)` call becomes a Function node spanning the call), so a
/// fixture body carries more than one descriptor. The principal function is the
/// named one.
fn named_function(graph: &CodeGraph, name: &str) -> (ShapeDescriptor, BodyHash128) {
    let meta = graph.macro_metadata();
    let mut found: Option<(ShapeDescriptor, BodyHash128)> = None;
    for (id, entry) in graph.nodes().iter() {
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        let resolved = graph.strings().resolve(entry.name);
        if resolved.as_deref() != Some(name) {
            continue;
        }
        if let Some(desc) = meta.shape_descriptors().get(&id) {
            assert!(
                found.is_none(),
                "fixture must define exactly one function named {name}"
            );
            let body_hash = entry
                .body_hash
                .expect("a function carrying a shape descriptor must also carry a body_hash");
            found = Some((desc.clone(), body_hash));
        }
    }
    found.unwrap_or_else(|| panic!("fixture must yield a descriptor for function {name}"))
}

const RUST_ORIGINAL: &str = include_str!("../../test-fixtures/shape/rename-invariance/original.rs");
const RUST_RENAMED: &str = include_str!("../../test-fixtures/shape/rename-invariance/renamed.rs");
const RUST_REFORMATTED: &str =
    include_str!("../../test-fixtures/shape/rename-invariance/reformatted.rs");

const PY_ORIGINAL: &str = include_str!("../../test-fixtures/shape/rename-invariance/original.py");
const PY_RENAMED: &str = include_str!("../../test-fixtures/shape/rename-invariance/renamed.py");
const PY_REFORMATTED: &str =
    include_str!("../../test-fixtures/shape/rename-invariance/reformatted.py");

fn rust(source: &str, name: &str) -> (ShapeDescriptor, BodyHash128) {
    named_function(
        &index_one(Box::new(RustPlugin::default()), "lib.rs", source),
        name,
    )
}

fn python(source: &str, name: &str) -> (ShapeDescriptor, BodyHash128) {
    named_function(
        &index_one(Box::new(PythonPlugin::default()), "mod.py", source),
        name,
    )
}

#[test]
fn ac2_rename_invariance_rust() {
    let (orig, _) = rust(RUST_ORIGINAL, "transform");
    let (renamed, _) = rust(RUST_RENAMED, "convert");

    assert!(!orig.is_unhashable() && !renamed.is_unhashable());
    // The shared structure is real (branch + loop + call + return are populated).
    assert!(
        orig.cf_histogram.iter().sum::<u16>() >= 4,
        "non-trivial body"
    );
    assert_eq!(
        orig.cf_histogram, renamed.cf_histogram,
        "AC-2: renaming identifiers/literals must not change the cf_histogram"
    );
    assert_eq!(
        orig.shape_hash, renamed.shape_hash,
        "AC-2: renaming identifiers/literals must not change the shape_hash"
    );
}

#[test]
fn ac2_rename_invariance_python() {
    let (orig, _) = python(PY_ORIGINAL, "transform");
    let (renamed, _) = python(PY_RENAMED, "convert");

    assert!(!orig.is_unhashable() && !renamed.is_unhashable());
    assert!(
        orig.cf_histogram.iter().sum::<u16>() >= 4,
        "non-trivial body"
    );
    assert_eq!(orig.cf_histogram, renamed.cf_histogram, "AC-2 (python)");
    assert_eq!(orig.shape_hash, renamed.shape_hash, "AC-2 (python)");
}

#[test]
fn ac3_reformat_and_comment_invariance_rust() {
    let (orig, orig_body) = rust(RUST_ORIGINAL, "transform");
    let (reformatted, reformatted_body) = rust(RUST_REFORMATTED, "transform");

    assert_eq!(
        orig.cf_histogram, reformatted.cf_histogram,
        "AC-3: comments/whitespace must not change the cf_histogram"
    );
    assert_eq!(
        orig.shape_hash, reformatted.shape_hash,
        "AC-3: comments/whitespace must not change the shape_hash"
    );
    // The same fixture proves the other half: body_hash IS byte-sensitive, so the
    // reformat changes it. A descriptor that tracked raw bytes would fail here.
    assert_ne!(
        orig_body, reformatted_body,
        "AC-3: reformatting the body bytes must change body_hash"
    );
}

#[test]
fn ac3_reformat_and_comment_invariance_python() {
    let (orig, orig_body) = python(PY_ORIGINAL, "transform");
    let (reformatted, reformatted_body) = python(PY_REFORMATTED, "transform");

    assert_eq!(orig.cf_histogram, reformatted.cf_histogram, "AC-3 (python)");
    assert_eq!(orig.shape_hash, reformatted.shape_hash, "AC-3 (python)");
    assert_ne!(
        orig_body, reformatted_body,
        "AC-3 (python) body_hash differs"
    );
}

#[test]
fn determinism_two_independent_builds_match() {
    // Two fully independent builds (fresh PluginManager, fresh tempdir, fresh
    // CodeGraph; no shared mutable state) must produce a byte-identical
    // descriptor: the same shape_hash AND the same 64-lane minhash. With no
    // wall-clock and no RNG in the walker, this is observationally identical to
    // building the same workspace in two separate OS processes.
    let (first, _) = rust(RUST_ORIGINAL, "transform");
    let (second, _) = rust(RUST_ORIGINAL, "transform");
    assert_eq!(
        first.shape_hash, second.shape_hash,
        "determinism: shape_hash must be stable across builds"
    );
    assert_eq!(
        first.minhash, second.minhash,
        "determinism: minhash must be stable across builds"
    );
    // Whole-descriptor equality (cf_histogram, signature_shape, callee_shape,
    // shape_hash, minhash, flags) — nothing in a descriptor may vary run to run.
    assert_eq!(first, second, "determinism: full descriptor must be stable");
}
