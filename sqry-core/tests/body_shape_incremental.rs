//! U15 — AC-7 incremental gate: editing one file's body recomputes only that
//! function's descriptor; an untouched file's descriptor stays byte-stable.
//!
//! This drives the REAL incremental engine (`incremental_rebuild`, the same path
//! the daemon's rebuild dispatcher uses), proving that shape descriptors flow
//! through reparse-on-change exactly like `body_hash`. The structural index that
//! consumes these descriptors (`sqry_db::StructuralNeighborsQuery`) declares a
//! Tier-1 (file-revision) dependency ONLY — frozen at compile time in
//! `structural_neighbors.rs` (`const _: () = assert!(!..TRACKS_EDGE_REVISION)`),
//! so a changed descriptor reshapes only the changed node's band membership while
//! edge/metadata-only churn never invalidates it. Here we prove the upstream half
//! the index depends on: the per-node descriptor recompute is correctly isolated
//! to the edited file.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use sqry_core::graph::unified::build::incremental::{
    compute_reverse_dep_closure, incremental_rebuild,
};
use sqry_core::graph::unified::build::{BuildConfig, CancellationToken, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::shape::ShapeHash128;
use sqry_core::plugin::PluginManager;
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

fn plugins() -> PluginManager {
    let mut pm = PluginManager::new();
    pm.register_builtin(Box::new(RustPlugin::default()));
    pm
}

/// The `shape_hash` of the Function named `name`, or `None` if no such function
/// carries a descriptor.
fn shape_hash_of(graph: &CodeGraph, name: &str) -> Option<ShapeHash128> {
    let meta = graph.macro_metadata();
    for (id, entry) in graph.nodes().iter() {
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        if graph.strings().resolve(entry.name).as_deref() != Some(name) {
            continue;
        }
        if let Some(desc) = meta.shape_descriptors().get(&id) {
            return Some(desc.shape_hash);
        }
    }
    None
}

#[test]
fn ac7_edit_one_body_recomputes_only_that_descriptor() {
    let tmp = TempDir::new().expect("tempdir");
    let a_path = tmp.path().join("a.rs");
    let b_path = tmp.path().join("b.rs");

    // `alpha` (file a) starts branch-only; `beta` (file b) is loop+branch and
    // never changes. The names are distinct across files so nothing unifies.
    fs::write(
        &a_path,
        r#"
pub fn alpha(x: i32) -> i32 {
    if x > 0 {
        return x;
    }
    x
}
"#,
    )
    .expect("write a.rs");
    fs::write(
        &b_path,
        r#"
pub fn beta(items: &[i32]) -> i32 {
    let mut acc = 0;
    for item in items {
        if *item > 0 {
            acc += *item;
        }
    }
    acc
}
"#,
    )
    .expect("write b.rs");

    let config = BuildConfig::default();
    let graph = build_unified_graph(tmp.path(), &plugins(), &config).expect("initial build");

    let alpha_before = shape_hash_of(&graph, "alpha").expect("alpha descriptor before edit");
    let beta_before = shape_hash_of(&graph, "beta").expect("beta descriptor before edit");

    // Edit ONLY file a: give `alpha` a loop, materially changing its structure
    // (a new Loop bucket) so its shape_hash must change. File b is untouched.
    fs::write(
        &a_path,
        r#"
pub fn alpha(x: i32) -> i32 {
    let mut total = 0;
    for step in 0..x {
        if step > 0 {
            total += step;
        }
    }
    total
}
"#,
    )
    .expect("rewrite a.rs");

    let changed: Vec<PathBuf> = vec![a_path.clone()];
    let a_file_id = graph.files().get(&a_path).expect("a.rs has a FileId");
    let closure: HashSet<_> = compute_reverse_dep_closure(&[a_file_id], &graph);
    let cancellation = CancellationToken::new();
    let rebuilt = incremental_rebuild(
        &graph,
        &changed,
        &closure,
        &plugins(),
        &config,
        &cancellation,
    )
    .expect("incremental rebuild succeeds");

    let alpha_after = shape_hash_of(&rebuilt, "alpha").expect("alpha descriptor after edit");
    let beta_after = shape_hash_of(&rebuilt, "beta").expect("beta descriptor after edit");

    // The edited function's descriptor recomputed: a new structure, a new hash.
    assert_ne!(
        alpha_before, alpha_after,
        "AC-7: editing alpha's body must recompute its shape descriptor"
    );
    // The untouched file's descriptor is byte-stable (cache-warm isolation): file
    // b was not in the closure, so beta is never reparsed and its hash is identical.
    assert_eq!(
        beta_before, beta_after,
        "AC-7: an unchanged file's descriptor must stay byte-identical (Tier-1 isolation)"
    );
}
