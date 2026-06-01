//! T3 Cluster B + Cluster D — full-pipeline staging-metadata propagation.
//!
//! Exercises Phase 4d-prime end-to-end with the real Go plugin:
//!
//! 1. Build a temporary workspace with a `//go:build linux` Go file
//!    declaring three top-level functions.
//! 2. Run `build_unified_graph` through `PluginManager` with `GoPlugin`.
//! 3. Assert the resulting `CodeGraph::macro_metadata()` carries
//!    `cfg_condition = Some("linux")` for every non-synthetic NodeId
//!    the file produced.
//! 4. Round-trip the graph through `save_to_path` / `load_from_path`
//!    and re-assert the metadata survives V11 snapshot serialisation
//!    (V11 is the current writer; wire-shape-identical to V10 per
//!    `sqry-core/src/graph/unified/persistence/format.rs`).
//!
//! This is the integration counterpart to the unit tests in
//! `sqry-core/src/graph/unified/build/parallel_commit.rs` (which
//! exercise the Phase 4d-prime helpers in isolation with synthetic
//! `NodeMetadataStore` inputs).

use std::fs;
use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_core::plugin::PluginManager;
use sqry_lang_go::GoPlugin;
use tempfile::TempDir;

fn plugin_manager() -> PluginManager {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(GoPlugin::default()));
    manager
}

fn write_go_file(root: &Path, name: &str, body: &str) {
    fs::write(root.join(name), body).expect("write Go fixture");
}

#[test]
fn gobuild_cfg_condition_reaches_snapshot_via_phase4d_prime() {
    let tmp = TempDir::new().expect("tempdir");
    let body = "//go:build linux\n\npackage cache\nfunc one() {}\nfunc two() {}\nfunc three() {}\n";
    write_go_file(tmp.path(), "cache.go", body);

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");

    // Walk every Macro-metadata entry. Phase 4d-prime+ Cluster D's
    // stamper guarantees cfg_condition == Some("linux") for every
    // non-synthetic NodeId emitted from `cache.go`.
    let metadata = graph.macro_metadata();
    let mut linux_count = 0usize;
    for (_, m) in metadata.iter() {
        if m.cfg_condition.as_deref() == Some("linux") {
            linux_count += 1;
        }
    }
    assert!(
        linux_count >= 3,
        "expected >=3 cfg_condition=Some(linux) entries (one per decl), got {linux_count}",
    );
}

#[test]
fn cfg_condition_persists_through_index_save_and_load() {
    let tmp = TempDir::new().expect("tempdir");
    let body = "//go:build linux && amd64\n\npackage cache\nfunc flush() {}\n";
    write_go_file(tmp.path(), "cache.go", body);

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");

    let snap_path = tmp.path().join("snapshot.sqry");
    save_to_path(&graph, &snap_path).expect("save_to_path");
    let reloaded = load_from_path(&snap_path, Some(&plugins)).expect("load_from_path");

    let metadata = reloaded.macro_metadata();
    let mut found = false;
    for (_, m) in metadata.iter() {
        if m.cfg_condition.as_deref() == Some("linux && amd64") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "cfg_condition must survive V11 snapshot save+load round trip",
    );
}

#[test]
fn synthetic_marker_preserved_through_phase_4d_prime() {
    // The Go plugin's local-scope resolver stamps synthetic Variable
    // nodes (`<ident>@<offset>` shape) at usage sites of a local
    // binding. With a `//go:build linux` header at the top of the
    // file, those synthetic NodeIds MUST NOT receive a
    // `cfg_condition` entry (per 02_DESIGN §4.3.d: the stamper skips
    // every NodeId already marked synthetic), AND the Synthetic
    // marker MUST survive Phase 4d-prime's metadata wire-through into
    // the final snapshot.
    //
    // Closes codex iter-5 finding 2: the previous body inspected
    // `graph.macro_metadata()` directly and used a vacuous duplicate-
    // key assertion (re-filtering the same store by the same key).
    // This rewrite goes through the snapshot's NodeId-keyed
    // `is_node_synthetic` + `macro_metadata().get(id).cfg_condition`
    // API for a specific synthetic NodeId.
    let tmp = TempDir::new().expect("tempdir");
    let body =
        "//go:build linux\n\npackage cache\nfunc touch() int {\n    x := 1\n    return x\n}\n";
    write_go_file(tmp.path(), "cache.go", body);

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");

    let snap = graph.snapshot();

    // Walk every committed NodeId. For each one that the live
    // `CodeGraph::is_node_synthetic` API reports as synthetic, the
    // snapshot's macro_metadata MUST NOT carry a `cfg_condition`
    // entry — that is the production contract from 02_DESIGN §4.3.d:
    // the stamper skips synthetic NodeIds, and Phase 4d-prime never
    // resurrects them.
    let mut synthetic_seen = 0usize;
    let mut macro_with_linux = 0usize;
    let mut synthetic_examples: Vec<String> = Vec::new();
    for (node_id, entry) in snap.nodes().iter() {
        let name = snap
            .strings()
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        if snap.is_node_synthetic(node_id) {
            synthetic_seen += 1;
            if synthetic_examples.len() < 3 {
                synthetic_examples.push(name.clone());
            }
            let macro_cfg = snap
                .macro_metadata()
                .get_macro(node_id)
                .and_then(|m| m.cfg_condition.clone());
            assert!(
                macro_cfg.is_none(),
                "synthetic NodeId {node_id:?} (name={name:?}) erroneously \
                 carries cfg_condition={macro_cfg:?} — the stamper failed \
                 to skip it OR Phase 4d-prime resurrected dropped metadata",
            );
        } else if let Some(macro_meta) = snap.macro_metadata().get_macro(node_id)
            && macro_meta.cfg_condition.as_deref() == Some("linux")
        {
            macro_with_linux += 1;
        }
    }
    assert!(
        synthetic_seen > 0,
        "fixture must produce at least one synthetic NodeId (so the \
         skip-branch is exercised); got synthetic_seen={synthetic_seen}. \
         If this regresses, the Go local-scope resolver no longer emits \
         synthetic `<ident>@<offset>` placeholders for the `x := 1` + \
         `return x` shape — refresh the fixture before silencing.",
    );
    assert!(
        macro_with_linux > 0,
        "expected at least one non-synthetic NodeId to carry \
         cfg_condition=Some(linux); got macro_with_linux={macro_with_linux} \
         (synthetic_seen={synthetic_seen}, examples={synthetic_examples:?})",
    );
}

/// Closes codex iter-5 finding 1: drive the full
/// `build_unified_graph` pipeline against two Go files that declare
/// `cache::flush` under different `//go:build` constraints. Phase
/// 4c-prime's cross-file unifier picks ONE winner (widest span;
/// fixtures keep spans equal so the tiebreaker is implementation-
/// defined but stable). Per 01_SPEC §5.3.f / 02_DESIGN §4.3.d, the
/// snapshot's macro_metadata MUST surface exactly one cfg_condition
/// for the surviving NodeId, drawn from {"linux", "darwin"}; the
/// loser's metadata is dropped (NOT merged into a disjunction — that
/// is the deliberately-deferred Phase-2 enhancement at 01_SPEC §9.7).
#[test]
fn cross_file_unification_picks_winner_cfg_condition() {
    let tmp = TempDir::new().expect("tempdir");
    // Filenames intentionally use no GOOS suffix — `parse_filename_suffix`
    // parses `_linux.go` / `_darwin.go` and would conjoin into the
    // header, producing "darwin && darwin" / "linux && linux". The
    // header alone gives a clean stored cfg_condition for the
    // winner/loser comparison.
    write_go_file(
        tmp.path(),
        "alpha.go",
        "//go:build linux\n\npackage cache\nfunc flush() {}\n",
    );
    write_go_file(
        tmp.path(),
        "beta.go",
        "//go:build darwin\n\npackage cache\nfunc flush() {}\n",
    );

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");

    let snap = graph.snapshot();
    let mut flush_nodes: Vec<(sqry_core::graph::unified::node::NodeId, Option<String>)> =
        Vec::new();
    for (node_id, entry) in snap.nodes().iter() {
        let qn = entry
            .qualified_name
            .and_then(|sid| snap.strings().resolve(sid))
            .map(|s| s.to_string())
            .unwrap_or_default();
        if qn != "cache::flush" {
            continue;
        }
        // Phase 4c-prime tombstones the loser via `merge_node_into`.
        // The live `CodeGraph::is_node_synthetic` check also catches
        // unification losers indirectly via `is_unified_loser`, so we
        // filter both via the snapshot's `nodes().iter()` plus the
        // graph's name-resolution path.
        if entry.is_unified_loser() {
            continue;
        }
        let cfg = snap
            .macro_metadata()
            .get_macro(node_id)
            .and_then(|m| m.cfg_condition.clone());
        flush_nodes.push((node_id, cfg));
    }
    assert_eq!(
        flush_nodes.len(),
        1,
        "Phase 4c-prime must pick exactly one winner for `cache::flush` \
         (loser tombstoned via is_unified_loser); got {flush_nodes:#?}",
    );
    let (_winner, winner_cfg) = &flush_nodes[0];
    let winner_cfg_str = winner_cfg
        .as_deref()
        .expect("winning NodeId must carry the winner's cfg_condition through Phase 4d-prime");
    assert!(
        winner_cfg_str == "linux" || winner_cfg_str == "darwin",
        "winner cfg_condition must be exactly one of {{linux,darwin}} \
         (NOT a disjunction — 01_SPEC §9.7 defers that to Phase 2); got {winner_cfg_str:?}",
    );
}

#[test]
fn no_buildline_plain_go_has_no_cfg_condition_in_snapshot() {
    let tmp = TempDir::new().expect("tempdir");
    let body = "package cache\nfunc flush() {}\n";
    write_go_file(tmp.path(), "cache.go", body);

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");

    let metadata = graph.macro_metadata();
    for (_, m) in metadata.iter() {
        assert!(
            m.cfg_condition.is_none(),
            "plain Go file should not stamp cfg_condition (got {:?})",
            m.cfg_condition,
        );
    }
}
