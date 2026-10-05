//! T47 (surface parity W1 round 5, design D27, codex R5-3): the graph
//! `execute_one_rebuild` hands the publish hook is the graph half of the
//! generation this rebuild published, never a re-read of the slot.
//!
//! The dispatcher's `TestCapture` gains a third observation point,
//! `post_publish_check`, fired after the publish block's read guard is
//! released and before it dispatches `SqrydHook::on_publish` (round 6,
//! design D31: its position relative to the `Loaded` state store is a
//! source order this test does not pin, battery row S25). This test parks
//! a rebuild there,
//! publishes a second generation for the same root from its own task
//! (`unload`, then `get_or_load` with a builder double whose node count the
//! fixture cannot produce), installs a recording hook, and releases the
//! rebuild. The hook must receive exactly one graph, the one the parked
//! rebuild published (`published_generations[0].graph` by pointer), and not
//! the slot's, which now holds the double's generation.
//!
//! Green on both heads (the dispatch already passes `published.graph`): a
//! declared control whose purpose is battery row K38
//! (`dispatch_publish_hook(&root, ws.graph())` at the dispatch), which it
//! kills deterministically because the second publication runs while the
//! rebuild is parked.

#![cfg(feature = "test-hooks")]

mod support;

use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};

use sqry_core::graph::CodeGraph;
use sqry_core::watch::ChangeSet;
use sqry_daemon::workspace::builder::FunctionGraphBuilder;
use sqry_daemon::{
    RebuildDispatcher, SqrydHook, TestCapture, WorkingSetInputs, working_set_estimate,
};

/// Function nodes in the double's generation: a count the one-function
/// fixture (`seed.rs`) cannot produce.
const DOUBLE_NODES: u32 = 7;

fn trivial_changes() -> ChangeSet {
    ChangeSet {
        changed_files: vec![PathBuf::from("seed.rs")],
        git_state_changed: false,
        git_change_class: None,
    }
}

/// A hook that keeps every graph `on_publish` receives.
#[derive(Debug, Default)]
struct GraphRecordingHook {
    received: parking_lot::Mutex<Vec<(PathBuf, Arc<CodeGraph>)>>,
}

impl SqrydHook for GraphRecordingHook {
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>) {
        self.received
            .lock()
            .push((workspace_root.to_path_buf(), graph));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuild_hook_receives_the_graph_of_the_generation_it_published() {
    let harness = support::WatcherHarness::new().await;

    let capture = Arc::new(TestCapture::new());
    harness
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("first install");
    capture.arm_post_publish_hold();

    let fixture_nodes = harness
        .manager
        .lookup(&harness.key)
        .expect("registered")
        .graph()
        .node_count();
    assert_ne!(
        fixture_nodes, DOUBLE_NODES as usize,
        "the double's node count must differ from the fixture's"
    );

    let dispatcher_clone: Arc<RebuildDispatcher> = Arc::clone(&harness.dispatcher);
    let key_clone = harness.key.clone();
    let rebuild_task = tokio::spawn(async move {
        dispatcher_clone
            .handle_changes(&key_clone, trivial_changes())
            .await
    });

    capture.wait_until_post_publish().await;

    // The rebuild is parked after its publish and before its hook
    // dispatch. Publish a second generation for the same root from here.
    assert!(
        harness.manager.unload(&harness.key),
        "the rebuilt workspace unloads"
    );
    let double = FunctionGraphBuilder::with_fast_path_record(DOUBLE_NODES);
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 64 * 1024,
        staging_overhead: 32 * 1024,
        interner_snapshot_bytes: 16 * 1024,
    });
    let second = harness
        .manager
        .get_or_load(&harness.key, &double, estimate)
        .expect("the second generation publishes");
    assert_eq!(second.node_count(), DOUBLE_NODES as usize);

    // Install the recording hook only now: the loader above dispatches
    // the hook for its own publication, and the claim under test is the
    // parked rebuild's dispatch alone.
    let hook = Arc::new(GraphRecordingHook::default());
    harness.manager.set_hook(hook.clone());

    capture.release_post_publish();
    rebuild_task
        .await
        .expect("join")
        .expect("the parked rebuild completes");

    let received = hook.received.lock().clone();
    let published = capture.published_generations.lock().clone();
    let slot_graph = harness
        .manager
        .lookup(&harness.key)
        .expect("the second generation is resident")
        .graph();
    assert_eq!(
        published.len(),
        1,
        "the capture recorded exactly the parked rebuild's generation"
    );
    assert_eq!(
        received.len(),
        1,
        "the hook received exactly one graph after it was installed"
    );
    let (hook_root, hook_graph) = &received[0];
    println!(
        "R5-3 hook plant: hook_nodes={} published_nodes={} slot_nodes={}",
        hook_graph.node_count(),
        published[0].graph.node_count(),
        slot_graph.node_count()
    );
    assert_eq!(
        hook_root, &harness.root,
        "the hook is told the rebuilt root"
    );
    assert_eq!(
        slot_graph.node_count(),
        DOUBLE_NODES as usize,
        "the second publication took effect: the slot holds the double's generation"
    );
    assert!(
        Arc::ptr_eq(hook_graph, &published[0].graph),
        "the hook must receive the graph of the generation this rebuild published \
         (hook_nodes={} published_nodes={} slot_nodes={})",
        hook_graph.node_count(),
        published[0].graph.node_count(),
        slot_graph.node_count()
    );
    assert!(
        !Arc::ptr_eq(hook_graph, &slot_graph),
        "the hook must not receive the slot's graph re-read at the dispatch"
    );
    assert_eq!(
        hook_graph.node_count(),
        fixture_nodes,
        "the parked rebuild's generation is the fixture's"
    );

    // Settle so the dispatcher's own bookkeeping after the hook is done
    // before the harness drops.
    tokio::time::sleep(Duration::from_millis(50)).await;
}
