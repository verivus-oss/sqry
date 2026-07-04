//! Issue #467: Go stdlib/relative/third-party import classification, driven
//! through the full `build_unified_graph` pipeline and a snapshot round trip.
//!
//! The staging-level unit coverage lives in
//! `sqry-lang-go/tests/import_classification.rs` (which drives
//! `process_import_spec_unified` and inspects the staging
//! `NodeMetadataStore`). This integration test is the end-to-end
//! counterpart: it builds a real workspace with the Go plugin, resolves each
//! committed `NodeKind::Import` node by name, and asserts the classification
//! bits (`IMPORT_STDLIB` / `IMPORT_RELATIVE`) landed on the right node via the
//! Phase 4d-prime store-merge carriage. It then saves and reloads the graph
//! and re-asserts that every per-node flag survives the V17 snapshot and that
//! the loaded graph reports `import_classification_signal_present()`.
//!
//! Classification is total and ordered (01_SPEC / 02_DESIGN):
//!   1. stdlib   (`IMPORT_STDLIB`): no `.` and the first `/`-segment is a
//!      standard-library root, e.g. `fmt`, `net/http`.
//!   2. relative (`IMPORT_RELATIVE`): the path starts with `./` or `../`,
//!      the exact Go relative-import grammar, e.g. `./util`.
//!   3. third-party (neither bit): the residual, covering dotted-domain
//!      module paths (`github.com/foo/bar`, `net.example.com/x`) and bare
//!      no-dot tokens that are not stdlib roots (`internalpkg`,
//!      `internal/foo`). None of these are relative in Go.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_core::plugin::PluginManager;
use sqry_lang_go::GoPlugin;
use tempfile::TempDir;

/// The seven canonical import paths and their expected classification bits.
/// Each tuple is `(import_path, is_stdlib, is_relative)`. The import path is
/// also the committed import node's semantic name: for paths containing `/`
/// the Go plugin keeps the original path verbatim, and the bare no-dot tokens
/// (`fmt`, `internalpkg`) canonicalise to themselves.
const EXPECTED: &[(&str, bool, bool)] = &[
    ("fmt", true, false),
    ("net/http", true, false),
    ("github.com/foo/bar", false, false),
    ("net.example.com/x", false, false),
    ("./util", false, true),
    ("internalpkg", false, false),
    ("internal/foo", false, false),
];

fn plugin_manager() -> PluginManager {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(GoPlugin::default()));
    manager
}

/// Build a single-file Go workspace importing all seven fixture paths.
fn build_fixture_graph(root: &Path) -> CodeGraph {
    let body = "\
package app

import (
\t\"fmt\"
\t\"net/http\"
\t\"github.com/foo/bar\"
\t\"net.example.com/x\"
\t\"./util\"
\t\"internalpkg\"
\t\"internal/foo\"
)
";
    fs::write(root.join("app.go"), body).expect("write Go fixture");

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build_unified_graph succeeds")
}

/// Collect every committed `NodeKind::Import` node keyed by its resolved
/// semantic name, mapping each to `(is_import_stdlib, is_import_relative)`
/// as read from the graph's `NodeId`-keyed metadata store.
fn collect_import_flags(graph: &CodeGraph) -> BTreeMap<String, (bool, bool)> {
    let snap = graph.snapshot();
    let meta = snap.macro_metadata();
    let mut out = BTreeMap::new();
    for (node_id, entry) in snap.nodes().iter() {
        if entry.kind != NodeKind::Import {
            continue;
        }
        // Import nodes are not call-compatible, so Phase 4c-prime never
        // tombstones them; still, skip any defensive unification loser.
        if entry.is_unified_loser() {
            continue;
        }
        let name = snap
            .strings()
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        out.insert(
            name,
            (
                meta.is_import_stdlib(node_id),
                meta.is_import_relative(node_id),
            ),
        );
    }
    out
}

/// Assert the collected per-node flags match `EXPECTED` exactly: every fixture
/// path present once with the right bits, no node carrying both bits, and the
/// aggregate bucket counts (2 stdlib, 1 relative, 4 third-party) holding.
fn assert_matches_expected(flags: &BTreeMap<String, (bool, bool)>) {
    assert_eq!(
        flags.len(),
        EXPECTED.len(),
        "expected exactly {} committed import nodes, got {}: {flags:#?}",
        EXPECTED.len(),
        flags.len(),
    );

    let mut stdlib_count = 0usize;
    let mut relative_count = 0usize;
    for &(path, want_stdlib, want_relative) in EXPECTED {
        let (got_stdlib, got_relative) = flags
            .get(path)
            .copied()
            .unwrap_or_else(|| panic!("import node {path:?} missing from graph: {flags:#?}"));
        assert_eq!(
            (got_stdlib, got_relative),
            (want_stdlib, want_relative),
            "import {path:?} classified (stdlib={got_stdlib}, relative={got_relative}), \
             expected (stdlib={want_stdlib}, relative={want_relative})",
        );
        assert!(
            !(got_stdlib && got_relative),
            "import {path:?} carries both classification bits (mutually exclusive)",
        );
        stdlib_count += usize::from(got_stdlib);
        relative_count += usize::from(got_relative);
    }
    assert_eq!(
        stdlib_count, 2,
        "exactly two stdlib imports (fmt, net/http)"
    );
    assert_eq!(relative_count, 1, "exactly one relative import (./util)");
}

#[test]
fn go_imports_classified_per_node_through_full_build() {
    let tmp = TempDir::new().expect("tempdir");
    let graph = build_fixture_graph(tmp.path());

    let flags = collect_import_flags(&graph);
    assert_matches_expected(&flags);

    assert!(
        graph.import_classification_signal_present(),
        "a freshly built graph must report the import-classification signal present",
    );
}

#[test]
fn import_classification_survives_snapshot_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let graph = build_fixture_graph(tmp.path());
    let before = collect_import_flags(&graph);
    assert_matches_expected(&before);

    let snap_path = tmp.path().join("snapshot.sqry");
    let plugins = plugin_manager();
    save_to_path(&graph, &snap_path).expect("save_to_path");
    let reloaded = load_from_path(&snap_path, Some(&plugins)).expect("load_from_path");

    let after = collect_import_flags(&reloaded);
    assert_eq!(
        before, after,
        "per-node import-classification flags must be identical across the V17 \
         snapshot round trip",
    );
    assert_matches_expected(&after);

    assert!(
        reloaded.import_classification_signal_present(),
        "a graph loaded from a V17 snapshot must report the import-classification \
         signal present",
    );
}

/// Build a single-file Go workspace whose sole source file carries a
/// `//go:build` constraint, so the Go plugin runs `stamp_cfg_condition_for_file`
/// over every non-synthetic staged node (import nodes included) before it
/// returns. The header is an explicit `//go:build linux` directive rather than a
/// `_GOOS.go` filename suffix so the constraint fires on every host.
fn build_tagged_fixture_graph(root: &Path) -> CodeGraph {
    let body = "\
//go:build linux

package app

import (
\t\"fmt\"
\t\"net/http\"
\t\"github.com/foo/bar\"
\t\"./util\"
)
";
    fs::write(root.join("tagged.go"), body).expect("write build-tagged Go fixture");

    let plugins = plugin_manager();
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build_unified_graph succeeds")
}

/// Regression for issue #467: a build-tagged Go file stamps a `cfg_condition`
/// macro payload onto every staged node after the import classifier has already
/// set `IMPORT_STDLIB` / `IMPORT_RELATIVE`. The stamp merges a flags-empty,
/// typed-only store; before the fix `NodeMetadataStore::merge` overwrote the
/// whole entry and silently cleared the classification bits, so a build-tagged
/// file reported every import as third-party. The merge now OR-es flags and only
/// replaces the typed payload, so both channels co-exist.
#[test]
fn build_tagged_go_preserves_import_classification() {
    let tmp = TempDir::new().expect("tempdir");
    let graph = build_tagged_fixture_graph(tmp.path());

    let flags = collect_import_flags(&graph);
    assert_eq!(
        flags.len(),
        4,
        "expected exactly four committed import nodes, got {}: {flags:#?}",
        flags.len(),
    );

    assert_eq!(
        flags.get("fmt").copied(),
        Some((true, false)),
        "stdlib import `fmt` must keep IMPORT_STDLIB through the cfg-condition \
         stamp on a build-tagged file: {flags:#?}",
    );
    assert_eq!(
        flags.get("net/http").copied(),
        Some((true, false)),
        "stdlib import `net/http` must keep IMPORT_STDLIB through the stamp: {flags:#?}",
    );
    assert_eq!(
        flags.get("./util").copied(),
        Some((false, true)),
        "relative import `./util` must keep IMPORT_RELATIVE through the stamp: {flags:#?}",
    );
    assert_eq!(
        flags.get("github.com/foo/bar").copied(),
        Some((false, false)),
        "third-party import stays unclassified through the stamp: {flags:#?}",
    );

    assert!(
        graph.import_classification_signal_present(),
        "a build-tagged build must still report the import-classification signal present",
    );
}
