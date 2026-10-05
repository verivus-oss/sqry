//! T10 (surface parity, unit W1): the registry resolver reads a workspace
//! roster from its manifest, falls back to the caller's config when no index
//! exists, refuses an unreadable manifest, and names an id this binary did
//! not compile.
//!
//! T28 (round 2, design D8): the one build-and-persist helper every
//! non-durable site calls records the resolved selection, refuses or falls
//! back on an unreadable manifest per [`UnreadableManifestPolicy`], and
//! writes nothing when it refuses. On the pre-change head (`abefdd8e3`) the
//! helper does not exist and this file does not compile.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

use std::path::Path;

use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::persistence::{
    BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
};
use sqry_core::progress::no_op_reporter;
use sqry_plugin_registry::{
    BuildWithRosterError, PluginSelectionConfig, PluginSelectionError, RosterSource,
    UnreadableManifestPolicy, build_and_persist_with_workspace_roster, builtin_plugin_ids,
    create_plugin_manager_all, resolve_persisted_selection, resolve_workspace_roster,
    resolve_workspace_roster_for_rebuild,
};
use tempfile::TempDir;

fn write_manifest(root: &Path, selection: Option<PluginSelectionManifest>) {
    let storage = GraphStorage::new(root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test"),
    )
    .with_plugin_selection(selection)
    .save(storage.manifest_path())
    .expect("manifest saved");
}

fn full_roster_ids() -> Vec<String> {
    create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

#[test]
fn no_index_resolves_to_none() {
    let tmp = TempDir::new().expect("tempdir");
    let resolved = resolve_persisted_selection(tmp.path()).expect("no index is not an error");
    assert!(resolved.is_none(), "expected None, got {resolved:?}");
}

#[test]
fn legacy_manifest_resolves_to_every_builtin_with_include_all() {
    let tmp = TempDir::new().expect("tempdir");
    write_manifest(tmp.path(), None);

    let resolved = resolve_persisted_selection(tmp.path())
        .expect("legacy manifest resolves")
        .expect("index exists");
    assert_eq!(
        resolved.source,
        RosterSource::LegacyManifestWithoutSelection
    );
    assert_eq!(resolved.active_plugin_ids, builtin_plugin_ids());
    assert_eq!(resolved.high_cost_mode.as_deref(), Some("include_all"));
    assert_eq!(
        resolved.manifest_path.as_deref(),
        Some(GraphStorage::new(tmp.path()).manifest_path())
    );
}

#[test]
fn recorded_selection_round_trips() {
    let tmp = TempDir::new().expect("tempdir");
    let recorded = PluginSelectionManifest {
        active_plugin_ids: vec!["rust".to_string(), "json".to_string()],
        high_cost_mode: Some("include_all".to_string()),
    };
    write_manifest(tmp.path(), Some(recorded.clone()));

    let resolved = resolve_persisted_selection(tmp.path())
        .expect("recorded manifest resolves")
        .expect("index exists");
    assert_eq!(resolved.source, RosterSource::PersistedManifest);
    assert_eq!(resolved.active_plugin_ids, recorded.active_plugin_ids);
    assert_eq!(resolved.high_cost_mode, recorded.high_cost_mode);
    assert_eq!(
        resolved.selection_manifest(),
        recorded,
        "the selection must convert back to the manifest block it was read from"
    );
    let back: PluginSelectionManifest = resolved.into();
    assert_eq!(back, recorded);
}

#[test]
fn unreadable_manifest_names_the_path() {
    let tmp = TempDir::new().expect("tempdir");
    let storage = GraphStorage::new(tmp.path());
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    std::fs::write(storage.manifest_path(), b"{ this is not json").expect("write garbage");
    assert!(
        storage.exists(),
        "a manifest file present means the index exists"
    );

    let err = resolve_persisted_selection(tmp.path()).expect_err("garbage manifest must fail");
    match &err {
        PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        } => {
            assert_eq!(manifest_path, storage.manifest_path());
            assert!(!reason.is_empty(), "reason must carry the parse error");
        }
        other => panic!("expected ManifestUnreadable, got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains(&storage.manifest_path().display().to_string()),
        "Display must name the manifest path: {rendered}"
    );
}

#[test]
fn roster_from_uncompiled_id_is_unknown_plugin_ids_with_manifest_path() {
    let tmp = TempDir::new().expect("tempdir");
    // `w1-not-a-plugin` is unknown in every build; `terraform` is
    // feature-gated and only joins the planted list when this binary did
    // not compile it, so the assertion never passes vacuously.
    let mut planted = vec!["w1-not-a-plugin".to_string()];
    if create_plugin_manager_all()
        .plugin_by_id("terraform")
        .is_none()
    {
        planted.push("terraform".to_string());
    }
    let mut ids = vec!["rust".to_string()];
    ids.extend(planted.iter().cloned());
    write_manifest(
        tmp.path(),
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: None,
        }),
    );

    let err = resolve_workspace_roster(tmp.path(), &PluginSelectionConfig::default())
        .expect_err("uncompiled id must refuse");
    match err {
        PluginSelectionError::UnknownPluginIdsCtx {
            ids, manifest_path, ..
        } => {
            let mut expected = planted.clone();
            expected.sort();
            assert_eq!(ids, expected, "every planted id must be listed");
            assert_eq!(
                manifest_path.as_deref(),
                Some(GraphStorage::new(tmp.path()).manifest_path()),
                "the error must name the manifest that recorded the id"
            );
        }
        other => panic!("expected UnknownPluginIdsCtx, got {other:?}"),
    }
}

#[test]
fn roster_without_index_uses_the_fallback_fast_path() {
    let tmp = TempDir::new().expect("tempdir");
    let roster = resolve_workspace_roster(tmp.path(), &PluginSelectionConfig::default())
        .expect("fallback resolves");
    assert_eq!(roster.selection.source, RosterSource::Fallback);
    assert_eq!(
        roster.selection.high_cost_mode.as_deref(),
        Some("fast_path_default")
    );
    assert!(roster.selection.manifest_path.is_none());
    assert!(
        roster.plugin_manager.plugin_by_id("json").is_none(),
        "fast-path fallback must not register json"
    );
    assert!(roster.plugin_manager.plugin_by_id("rust").is_some());
    let registered: Vec<String> = roster
        .plugin_manager
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    assert_eq!(registered, roster.selection.active_plugin_ids);
}

#[test]
fn roster_from_include_all_manifest_registers_json() {
    let tmp = TempDir::new().expect("tempdir");
    write_manifest(
        tmp.path(),
        Some(PluginSelectionManifest {
            active_plugin_ids: full_roster_ids(),
            high_cost_mode: Some("include_all".to_string()),
        }),
    );

    let roster = resolve_workspace_roster(tmp.path(), &PluginSelectionConfig::default())
        .expect("include_all manifest resolves");
    assert_eq!(roster.selection.source, RosterSource::PersistedManifest);
    assert!(
        roster.plugin_manager.plugin_by_id("json").is_some(),
        "an include_all manifest must register json even though the fallback is fast path"
    );
    let registered: Vec<String> = roster
        .plugin_manager
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    assert_eq!(registered.len(), full_roster_ids().len());
}

/// A rebuild or repair over an index whose manifest cannot be read falls
/// back to the fast path and says so, while a readable manifest naming an
/// uncompiled id is still refused, and the strict resolver still refuses
/// the unreadable manifest.
#[test]
fn rebuild_resolver_falls_back_on_unreadable_manifest_but_refuses_unknown_ids() {
    let tmp = TempDir::new().expect("tempdir");
    let storage = GraphStorage::new(tmp.path());
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    std::fs::write(storage.manifest_path(), b"{}").expect("write unparseable manifest");

    let strict = resolve_workspace_roster(tmp.path(), &PluginSelectionConfig::default())
        .expect_err("the strict resolver refuses an unreadable manifest");
    assert!(
        matches!(strict, PluginSelectionError::ManifestUnreadable { .. }),
        "unexpected error: {strict:?}"
    );

    let roster =
        resolve_workspace_roster_for_rebuild(tmp.path(), &PluginSelectionConfig::default())
            .expect("a rebuild over an unreadable manifest resolves to the fallback");
    assert_eq!(roster.selection.source, RosterSource::Fallback);
    assert!(roster.plugin_manager.plugin_by_id("json").is_none());
    let unreadable = roster
        .unreadable_manifest
        .expect("the fallback must be reported, not silent");
    assert_eq!(unreadable.manifest_path, storage.manifest_path());
    assert!(!unreadable.reason.is_empty());

    // A readable manifest naming an uncompiled id is refused by both.
    write_manifest(
        tmp.path(),
        Some(PluginSelectionManifest {
            active_plugin_ids: vec!["rust".to_string(), "w1-not-a-plugin".to_string()],
            high_cost_mode: None,
        }),
    );
    let err = resolve_workspace_roster_for_rebuild(tmp.path(), &PluginSelectionConfig::default())
        .expect_err("an uncompiled id in a readable manifest is refused on the rebuild path too");
    assert!(
        matches!(err, PluginSelectionError::UnknownPluginIdsCtx { .. }),
        "unexpected error: {err:?}"
    );
    assert!(resolve_workspace_roster(tmp.path(), &PluginSelectionConfig::default()).is_err());
}

// ---------------------------------------------------------------------------
// T28 (round 2): the build-and-persist helper.
// ---------------------------------------------------------------------------

/// One Rust file and one JSON file, so the fast path and the full roster
/// build different graphs and the recorded selection is observable in the
/// graph as well as in the manifest.
fn write_mixed_fixture(root: &Path) {
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

/// The manifest's recorded selection, read back after a persist.
fn recorded_selection(root: &Path) -> PluginSelectionManifest {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .plugin_selection
        .expect("plugin_selection recorded")
}

/// Sorted listing of `<root>/.sqry`, recursive, so "wrote nothing" is a
/// comparison of the whole index directory, not one file.
fn index_dir_listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
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

fn build(
    root: &Path,
    policy: UnreadableManifestPolicy,
) -> Result<sqry_plugin_registry::PersistedBuild, BuildWithRosterError> {
    build_and_persist_with_workspace_roster(
        root,
        &PluginSelectionConfig::default(),
        policy,
        "test:t28",
        &BuildConfig::default(),
        &sqry_core::graph::unified::build::MacroOptionsRequest::empty(),
        no_op_reporter(),
    )
}

#[test]
fn helper_over_include_all_manifest_records_json_and_include_all() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    let ids = full_roster_ids();
    assert!(ids.iter().any(|id| id == "json"), "fixture precondition");
    write_manifest(
        &root,
        Some(PluginSelectionManifest {
            active_plugin_ids: ids.clone(),
            high_cost_mode: Some("include_all".to_string()),
        }),
    );

    for policy in [
        UnreadableManifestPolicy::Refuse,
        UnreadableManifestPolicy::FallBack,
    ] {
        let built = build(&root, policy).expect("include_all manifest builds");
        assert_eq!(
            built.roster.selection.source,
            RosterSource::PersistedManifest
        );
        assert!(built.roster.unreadable_manifest.is_none());
        assert_eq!(built.roster.selection.active_plugin_ids, ids);
        assert_eq!(
            built.build_result.active_plugin_ids, ids,
            "the build must run with exactly the recorded ids"
        );
        let recorded = recorded_selection(&root);
        assert_eq!(recorded.active_plugin_ids, ids, "policy {policy:?}");
        assert_eq!(
            recorded.high_cost_mode.as_deref(),
            Some("include_all"),
            "high_cost_mode must be carried through, policy {policy:?}"
        );
        assert!(
            built.graph.node_count() > 0,
            "a built graph has nodes, policy {policy:?}"
        );
    }
}

#[test]
fn helper_over_no_manifest_records_the_fast_path() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    assert!(!GraphStorage::new(&root).exists(), "fixture precondition");

    let built = build(&root, UnreadableManifestPolicy::Refuse).expect("fresh index builds");
    assert_eq!(built.roster.selection.source, RosterSource::Fallback);
    assert!(built.roster.unreadable_manifest.is_none());
    assert!(
        !built
            .roster
            .selection
            .active_plugin_ids
            .iter()
            .any(|id| id == "json"),
        "the fast-path fallback must not enable json: {:?}",
        built.roster.selection.active_plugin_ids
    );
    let recorded = recorded_selection(&root);
    assert_eq!(
        recorded.active_plugin_ids,
        built.roster.selection.active_plugin_ids
    );
    assert_eq!(
        recorded.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "the fallback mode is recorded, not None"
    );
    assert_eq!(
        recorded.active_plugin_ids,
        built.build_result.active_plugin_ids
    );
}

#[test]
fn helper_refuses_an_unreadable_manifest_and_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    let storage = GraphStorage::new(&root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    std::fs::write(storage.manifest_path(), b"{}").expect("write unparseable manifest");
    let before = index_dir_listing(&root);
    assert_eq!(
        before.len(),
        1,
        "fixture precondition: only the manifest exists"
    );

    let err = build(&root, UnreadableManifestPolicy::Refuse)
        .expect_err("Refuse must not build over an unreadable manifest");
    match &err {
        BuildWithRosterError::Selection(PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        }) => {
            assert_eq!(manifest_path, storage.manifest_path());
            assert!(!reason.is_empty());
        }
        other => panic!("expected Selection(ManifestUnreadable), got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains(&storage.manifest_path().display().to_string()),
        "a single-line render must name the manifest: {rendered}"
    );
    assert_eq!(
        index_dir_listing(&root),
        before,
        "the refusal must write nothing under .sqry"
    );
}

#[test]
fn helper_falls_back_on_an_unreadable_manifest_and_reports_the_file() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    let storage = GraphStorage::new(&root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    std::fs::write(storage.manifest_path(), b"{}").expect("write unparseable manifest");

    let built = build(&root, UnreadableManifestPolicy::FallBack)
        .expect("FallBack rebuilds over an unreadable manifest");
    assert_eq!(built.roster.selection.source, RosterSource::Fallback);
    let unreadable = built
        .roster
        .unreadable_manifest
        .as_ref()
        .expect("the fallback must be reported, not silent");
    assert_eq!(unreadable.manifest_path, storage.manifest_path());
    assert!(!unreadable.reason.is_empty());
    let recorded = recorded_selection(&root);
    assert_eq!(
        recorded.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "the fallback selection is recorded in the rewritten manifest"
    );
    assert!(!recorded.active_plugin_ids.iter().any(|id| id == "json"));
    assert_eq!(
        recorded.active_plugin_ids,
        built.roster.selection.active_plugin_ids
    );
}

#[test]
fn helper_refuses_an_uncompiled_id_under_both_policies_and_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    assert!(
        create_plugin_manager_all()
            .plugin_by_id("w1-r2-planted-plugin")
            .is_none(),
        "the planted id must be one no build compiles"
    );
    write_manifest(
        &root,
        Some(PluginSelectionManifest {
            active_plugin_ids: vec!["rust".to_string(), "w1-r2-planted-plugin".to_string()],
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
    );
    let before = index_dir_listing(&root);

    for policy in [
        UnreadableManifestPolicy::Refuse,
        UnreadableManifestPolicy::FallBack,
    ] {
        let err = build(&root, policy).expect_err("an uncompiled id is refused, policy {policy:?}");
        match &err {
            BuildWithRosterError::Selection(PluginSelectionError::UnknownPluginIdsCtx {
                ids,
                manifest_path,
                ..
            }) => {
                assert_eq!(ids, &vec!["w1-r2-planted-plugin".to_string()]);
                assert_eq!(
                    manifest_path.as_deref(),
                    Some(GraphStorage::new(&root).manifest_path())
                );
            }
            other => panic!("expected Selection(UnknownPluginIdsCtx), got {other:?}"),
        }
        assert!(
            err.to_string().contains("w1-r2-planted-plugin"),
            "a single-line render must name the id: {err}"
        );
        assert_eq!(
            index_dir_listing(&root),
            before,
            "the refusal must write nothing under .sqry, policy {policy:?}"
        );
    }
}
