//! T2 semantic_diff edge-delta axis (`02_DESIGN.md` §7.6).
//!
//! Builds two real Go snapshots through `build_unified_graph` + the Go plugin
//! and asserts that `ComparativeQueryDb::diff` surfaces `Instantiates` and
//! `ChannelPeer` edge changes — the axis the node-only comparator was blind to.

use std::fs;
use std::sync::Arc;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::plugin::PluginManager;
use sqry_db::{ComparativeQueryDb, DiffEdgeKey};
use sqry_lang_go::GoPlugin;
use tempfile::TempDir;

fn snapshot_for(source: &str) -> (TempDir, Arc<GraphSnapshot>) {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join("q.go"), source).expect("write fixture");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let graph = build_unified_graph(tmp.path(), &plugins, &BuildConfig::default())
        .expect("build_unified_graph succeeds");
    (tmp, Arc::new(graph.snapshot()))
}

fn type_arg_names(kind: &DiffEdgeKey) -> Vec<String> {
    match kind {
        DiffEdgeKey::Instantiates { type_args, .. } => {
            type_args.iter().map(|ta| ta.name.clone()).collect()
        }
        DiffEdgeKey::ChannelPeer { .. } => Vec::new(),
    }
}

#[test]
fn instantiates_type_argument_change_surfaces_in_diff() {
    // The two sources differ ONLY in the generic instantiation's second type
    // argument (`int` -> `int64`). The edit is inside the call, so the
    // call-site byte offset (hence the source qn) is stable on both sides.
    let old = r#"
package q

func Map[K comparable, V any](m map[K]V) []K { return nil }

func main() {
    _ = Map[string, int](nil)
}
"#;
    let new = r#"
package q

func Map[K comparable, V any](m map[K]V) []K { return nil }

func main() {
    _ = Map[string, int64](nil)
}
"#;
    let (_old_tmp, old_snap) = snapshot_for(old);
    let (_new_tmp, new_snap) = snapshot_for(new);

    let diff = ComparativeQueryDb::new(old_snap, new_snap).diff_default();

    assert_eq!(
        diff.instantiates_edges_removed.len(),
        1,
        "the [string, int] instantiation must be removed"
    );
    assert_eq!(
        type_arg_names(&diff.instantiates_edges_removed[0].kind),
        vec!["string".to_string(), "int".to_string()],
    );

    assert_eq!(
        diff.instantiates_edges_added.len(),
        1,
        "the [string, int64] instantiation must be added"
    );
    assert_eq!(
        type_arg_names(&diff.instantiates_edges_added[0].kind),
        vec!["string".to_string(), "int64".to_string()],
    );
}

#[test]
fn added_channel_send_site_surfaces_in_diff() {
    // The new snapshot adds a second send site AFTER the first, so the first
    // send's byte offset is unchanged and only the new one is a delta.
    let old = r#"
package q

func main() {
    ch := make(chan int, 4)
    ch <- 1
}
"#;
    let new = r#"
package q

func main() {
    ch := make(chan int, 4)
    ch <- 1
    ch <- 2
}
"#;
    let (_old_tmp, old_snap) = snapshot_for(old);
    let (_new_tmp, new_snap) = snapshot_for(new);

    let diff = ComparativeQueryDb::new(old_snap, new_snap).diff_default();

    assert_eq!(
        diff.channel_peer_edges_added.len(),
        1,
        "exactly one new ChannelPeer (the second send) must be added"
    );
    assert!(
        matches!(
            diff.channel_peer_edges_added[0].kind,
            DiffEdgeKey::ChannelPeer { ref direction } if direction == "send"
        ),
        "the added edge is a Send"
    );
    assert!(
        diff.channel_peer_edges_added[0]
            .target_qn
            .ends_with("main::ch"),
        "the added send targets q::main::ch (got {:?})",
        diff.channel_peer_edges_added[0].target_qn
    );
    assert!(
        diff.channel_peer_edges_removed.is_empty(),
        "nothing was removed"
    );
}

#[test]
fn identical_snapshots_have_no_edge_deltas() {
    let src = r#"
package q

func Work[T any](x T) {}

func main() {
    ch := make(chan int)
    ch <- 1
    <-ch
    Work[int](42)
}
"#;
    let (_old_tmp, old_snap) = snapshot_for(src);
    let (_new_tmp, new_snap) = snapshot_for(src);

    let diff = ComparativeQueryDb::new(old_snap, new_snap).diff_default();
    assert!(diff.channel_peer_edges_added.is_empty());
    assert!(diff.channel_peer_edges_removed.is_empty());
    assert!(diff.instantiates_edges_added.is_empty());
    assert!(diff.instantiates_edges_removed.is_empty());
}
