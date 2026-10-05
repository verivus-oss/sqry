//! T1 and T4 (surface parity, unit W1): the production daemon builder
//! builds a workspace with the roster its manifest records, and falls back
//! to the fast path for a workspace with no index.
//!
//! T1 plants a workspace indexed with `include_all` (one `.rs` and one
//! `.json` file) and requires the daemon build to contain the json nodes
//! the CLI build contains; on the pre-change head the daemon built every
//! workspace with the fast-path manager and the json count was 0.
//!
//! T4 is the control the repair must not break: a workspace with no
//! manifest still builds with the fast-path default (`json` absent,
//! `source == Fallback`). It is green on both heads by design.
//!
//! Round 2 adds T17 (design D10): `load_persisted` classifies the
//! manifest's ids against the load roster before reading the snapshot and
//! refuses an id this binary did not compile with
//! `WorkspaceIncompatibleGraph`, writing nothing; on `abefdd8e3` it
//! returned `Ok` with the planted id in the record. And the resolver leg of
//! T22 (design D9, D12): an unreadable manifest is refused with `-32001`
//! naming the file and `sqry index --force <root>`; on `abefdd8e3` it
//! resolved to the fallback.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_core::query::QueryExecutor;
use sqry_daemon::{DaemonError, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceRosterResolver};
use sqry_plugin_registry::{RosterSource, create_plugin_manager, create_plugin_manager_all};
use tempfile::TempDir;

/// The id no build of sqry compiles, planted by the round 2 controls.
const PLANTED_ID: &str = "w1-r2-planted-plugin";

/// One Rust file and one JSON file, so the fast path and the full roster
/// build different graphs.
pub fn write_mixed_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "nested": {"enabled": true, "count": 3}, "items": [1, 2]}"#,
    )
    .expect("write config.json");
}

/// Index `root` the way `sqry index --include-high-cost` does: every
/// compiled plugin, `high_cost_mode: include_all` recorded in the manifest.
pub fn index_with_include_all(root: &Path) {
    let plugins = create_plugin_manager_all();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &BuildConfig::default(),
        "test:include_all",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("include_all".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("include_all index persists");
    let manifest = GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable");
    let selection = manifest.plugin_selection.expect("selection recorded");
    assert!(
        selection.active_plugin_ids.iter().any(|id| id == "json"),
        "fixture precondition: the manifest must record json"
    );
    assert_eq!(selection.high_cost_mode.as_deref(), Some("include_all"));
}

/// Count the nodes `lang:json` matches in `graph`, the same way every
/// query surface counts them.
pub fn json_node_count(graph: Arc<CodeGraph>, root: &Path) -> usize {
    let executor = QueryExecutor::with_plugin_manager(create_plugin_manager_all());
    executor
        .execute_on_preloaded_graph(graph, "lang:json", root, None)
        .expect("lang:json query runs")
        .len()
}

#[test]
fn daemon_builder_builds_with_the_manifest_roster() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);

    // N is computed from the fixture with the full roster, never hardcoded.
    let reference = sqry_core::graph::unified::build::build_unified_graph(
        &root,
        &create_plugin_manager_all(),
        &BuildConfig::default(),
    )
    .expect("reference build");
    let expected_json_nodes = json_node_count(Arc::new(reference), &root);
    assert!(
        expected_json_nodes >= 1,
        "fixture precondition: the full roster must produce at least one json node, got {expected_json_nodes}"
    );

    let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    let built = builder.build(&root).expect("daemon build succeeds");
    assert_eq!(built.roster.source, RosterSource::PersistedManifest);
    assert!(
        built.roster.contains("json"),
        "the record must carry json: {:?}",
        built.roster.active_plugin_ids
    );
    assert_eq!(built.roster.high_cost_mode.as_deref(), Some("include_all"));

    let daemon_json_nodes = json_node_count(Arc::new(built.graph), &root);
    assert_eq!(
        daemon_json_nodes, expected_json_nodes,
        "the daemon build must contain the same json nodes as the full-roster build"
    );

    // The same builder loads the persisted snapshot with the full roster and
    // records the manifest's selection.
    let loaded = builder
        .load_persisted(&root)
        .expect("load_persisted succeeds");
    assert_eq!(loaded.roster.source, RosterSource::PersistedManifest);
    assert!(loaded.roster.contains("json"));
    assert_eq!(
        json_node_count(Arc::new(loaded.graph), &root),
        expected_json_nodes,
        "the loaded snapshot must carry the same json nodes"
    );
}

#[test]
fn daemon_builder_without_manifest_uses_the_fast_path_control() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let resolved = resolver.resolve(&root).expect("fallback resolves");
    assert_eq!(resolved.record.source, RosterSource::Fallback);
    assert!(
        resolved.plugins.plugin_by_id("json").is_none(),
        "a workspace with no index must not have json turned on"
    );

    let builder = RealWorkspaceBuilder::new(resolver);
    let built = builder.build(&root).expect("fallback build succeeds");
    assert_eq!(built.roster.source, RosterSource::Fallback);
    assert!(!built.roster.contains("json"));
    assert_eq!(
        json_node_count(Arc::new(built.graph), &root),
        0,
        "the fast-path build must contain no json nodes"
    );
    assert!(
        builder.roster_for(&root).expect("roster_for").source == RosterSource::Fallback,
        "roster_for must agree with build"
    );
}

/// Index `root` the way `sqry index` does by default: the fast-path roster,
/// `high_cost_mode: fast_path_default` recorded in the manifest.
fn index_with_fast_path(root: &Path) {
    let plugins = create_plugin_manager();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &BuildConfig::default(),
        "test:fast_path",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

/// Sorted listing of every file under `<root>/.sqry` with its bytes, so
/// "nothing on disk changes" compares the whole index directory.
fn index_dir_listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(prefix).expect("under prefix");
            if path.is_dir() {
                walk(&path, prefix, out);
            } else {
                out.push((
                    rel.display().to_string(),
                    std::fs::read(&path).expect("file bytes"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(".sqry"), root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// T17 (round 2, D10): a valid fast-path snapshot whose manifest names an
/// id this binary did not compile is refused by `load_persisted` itself,
/// before the snapshot is read, and nothing on disk changes.
#[test]
fn daemon_load_persisted_refuses_a_manifest_naming_an_uncompiled_plugin() {
    assert!(
        create_plugin_manager_all()
            .plugin_by_id(PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_fast_path(&root);

    let storage = GraphStorage::new(&root);
    let mut manifest = storage.load_manifest().expect("manifest readable");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(PLANTED_ID.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    let listing_before = index_dir_listing(&root);
    assert!(
        listing_before.len() >= 2,
        "fixture precondition: manifest and snapshot exist, got {} files",
        listing_before.len()
    );

    let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    let err = builder
        .load_persisted(&root)
        .expect_err("a manifest naming an uncompiled id must be refused at load");
    match &err {
        DaemonError::WorkspaceIncompatibleGraph {
            root: named,
            reason,
        } => {
            assert_eq!(named, &root);
            assert!(
                reason.contains(PLANTED_ID),
                "the refusal must name the id: {reason}"
            );
            assert!(
                reason.contains(&storage.manifest_path().display().to_string()),
                "the refusal must name the manifest: {reason}"
            );
        }
        other => panic!("expected WorkspaceIncompatibleGraph, got {other:?}"),
    }
    assert_eq!(err.jsonrpc_code(), Some(-32005));
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "a refused load must change nothing under .sqry"
    );
}

/// T22, resolver leg (round 2, D9, D12): the daemon resolver refuses an
/// index whose manifest cannot be read, with `-32001`, naming the file and
/// the repair command, instead of resolving the fallback over it.
#[test]
fn daemon_resolver_refuses_an_unreadable_manifest_naming_the_repair() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_fast_path(&root);
    let storage = GraphStorage::new(&root);
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let listing_before = index_dir_listing(&root);

    let resolver = WorkspaceRosterResolver::new();
    let err = resolver
        .resolve(&root)
        .expect_err("an unreadable manifest must be refused");
    assert_eq!(err.jsonrpc_code(), Some(-32001), "{err:?}");
    let rendered = err.to_string();
    let manifest_path = storage.manifest_path().display().to_string();
    let repair = format!("sqry index --force {}", root.display());
    assert!(
        rendered.contains(&manifest_path),
        "the refusal must name the manifest: {rendered}"
    );
    assert!(
        rendered.contains(&repair),
        "the refusal must name the repair: {rendered}"
    );
    // The variant itself is pinned by the in-crate resolver test
    // (`resolve_unreadable_manifest_is_refused_naming_the_repair`); this
    // leg asserts the wire-visible contract only, so its red on the
    // pre-change head is the assertion above, not a compile error.
    let data = err.error_data().expect("error_data present");
    assert_eq!(data["root"], root.display().to_string());
    assert_eq!(data["manifest_path"], manifest_path);
    assert_eq!(data["repair_command"], repair);
    assert!(
        data["reason"]
            .as_str()
            .expect("reason string")
            .contains(&manifest_path),
        "data.reason must name the manifest: {data}"
    );
    assert_eq!(resolver.memo_len(), 0, "a refusal memoises nothing");
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "a refusal must change nothing under .sqry"
    );

    // The same builder's `load_persisted` refuses it too (it reads the
    // manifest through the same registry function), with the same code.
    let builder = RealWorkspaceBuilder::new(Arc::new(resolver));
    let load_err = builder
        .load_persisted(&root)
        .expect_err("load_persisted must refuse an unreadable manifest");
    assert_eq!(load_err.jsonrpc_code(), Some(-32001), "{load_err:?}");
    assert!(
        load_err.to_string().contains(&manifest_path),
        "load_persisted must name the manifest: {load_err}"
    );
    // Round 3 (design D15, R3-2, R3-15, R3-16): the code and the path in
    // Display are satisfied by `WorkspaceBuildFailed` too, which is how
    // both a fallback to the fast path and a generic build failure
    // survived this leg. The variant and the `error_data` keys are the
    // discriminating assertions.
    assert!(
        matches!(load_err, DaemonError::WorkspaceManifestUnreadable { .. }),
        "load_persisted must refuse with the typed variant, got {load_err:?}"
    );
    let load_data = load_err
        .error_data()
        .expect("WorkspaceManifestUnreadable carries error_data");
    assert_eq!(load_data["manifest_path"], manifest_path);
    assert_eq!(load_data["repair_command"], repair);
    assert_eq!(load_data["root"], root.display().to_string());
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "load_persisted's refusal must change nothing under .sqry"
    );
}

// ---------------------------------------------------------------------------
// T11 (surface parity W4, design W4-D7): the production daemon builder
// builds a workspace with the macro options its manifest records.
// ---------------------------------------------------------------------------

const W4_CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";

/// Index `root` the way `sqry index --cfg test` does: fast-path roster,
/// `MacroBuildOptions { cfg_flags: ["test"] }`, so the manifest records the
/// flag through the persistence transaction (W4-D6).
fn index_with_cfg_test(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(root.join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
    let plugins = create_plugin_manager();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    let config = BuildConfig {
        macro_options: sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: vec!["test".to_string()],
            expand_cache_dir: None,
        },
        ..BuildConfig::default()
    };
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &config,
        "test:cfg_test",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("cfg index persists");
    let record = GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
        .expect("fixture precondition: the manifest records the macro options");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
}

/// The activation `graph` records for the fixture's `cfg(test)` item.
fn cfg_test_activation(graph: &CodeGraph) -> Option<bool> {
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("recorded cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

#[test]
fn daemon_builder_builds_with_the_recorded_macro_options() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    index_with_cfg_test(&root);

    let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    let built = builder.build(&root).expect("daemon build succeeds");
    assert_eq!(
        cfg_test_activation(&built.graph),
        Some(true),
        "the daemon must build with the recorded --cfg test"
    );

    // Control: a workspace with no record builds with no options, as before.
    let plain = TempDir::new().expect("tempdir");
    let plain_root = plain.path().canonicalize().expect("canonical root");
    std::fs::create_dir_all(plain_root.join("src")).expect("src dir");
    std::fs::write(plain_root.join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
    index_with_fast_path(&plain_root);
    assert!(
        GraphStorage::new(&plain_root)
            .load_manifest()
            .expect("manifest readable")
            .macro_options
            .is_none(),
        "control precondition: no record"
    );
    let built = builder.build(&plain_root).expect("daemon build succeeds");
    assert_eq!(
        cfg_test_activation(&built.graph),
        None,
        "no record, no activation"
    );
}

/// T14 (daemon builder leg): a recorded expand cache directory that no
/// longer exists is refused by `RealWorkspaceBuilder::build` with
/// `RebuildMacroOptionsUnavailable` naming the directory; nothing on disk
/// changes.
#[test]
fn daemon_builder_refuses_a_missing_recorded_expand_cache() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(root.join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
    let cache = root.join("expand-cache");
    std::fs::create_dir_all(&cache).expect("cache dir");
    let config = BuildConfig {
        macro_options: sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: vec![],
            expand_cache_dir: Some(cache.clone()),
        },
        ..BuildConfig::default()
    };
    build_and_persist_graph_with_progress(
        &root,
        &create_plugin_manager(),
        &config,
        "test:expand_cache",
        None,
        no_op_reporter(),
    )
    .expect("index with an expand cache persists");
    let recorded = GraphStorage::new(&root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
        .and_then(|record| record.expand_cache_dir)
        .expect("the manifest names the expand cache directory");
    assert_eq!(Path::new(&recorded), cache.as_path());
    std::fs::remove_dir_all(&cache).expect("remove the cache");
    let listing_before = index_dir_listing(&root);

    let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    let err = builder
        .build(&root)
        .expect_err("a missing recorded expand cache must refuse");
    match &err {
        DaemonError::RebuildMacroOptionsUnavailable {
            root: reported_root,
            expand_cache_dir,
            origin,
        } => {
            assert_eq!(*origin, sqry_mcp::error::ExpandCacheOrigin::Recorded);
            assert_eq!(reported_root, &root);
            assert_eq!(expand_cache_dir, &cache);
        }
        other => panic!("expected RebuildMacroOptionsUnavailable, got {other:?}"),
    }
    assert_eq!(err.jsonrpc_code(), Some(-32022));
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "nothing on disk changed"
    );
}
