use sqry_core::graph::unified::analysis::condensation::{
    BudgetExceededPolicy, CondensationDag, LabelBudgetConfig,
};
use sqry_core::graph::unified::analysis::csr::CsrAdjacency;
use sqry_core::graph::unified::analysis::scc::SccData;
use sqry_core::graph::unified::compaction::{CompactionSnapshot, MergedEdge};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::node::NodeId;
use std::time::Instant;

#[test]
fn test_reachability_density_performance_regression() {
    // Create a dense graph that would have caused O(N^2 log N) sort/merge overhead.
    // N=2000 nodes.
    // Node i connects to nodes i+1, i+3, i+5, ... (many disjoint intervals if not careful)

    let n = 2000;
    let file = FileId::new(0);
    let kind = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
    };

    let mut edges = Vec::new();
    for i in 0..n {
        for j in (i + 1..n).step_by(2) {
            edges.push(MergedEdge::new(
                #[allow(clippy::cast_possible_truncation)] // Test graph sizes are small constants
                NodeId::new(i as u32, 0),
                #[allow(clippy::cast_possible_truncation)] // Test graph sizes are small constants
                NodeId::new(j as u32, 0),
                kind.clone(),
                1,
                file,
            ));
        }
    }

    let snapshot = CompactionSnapshot {
        csr_edges: edges,
        delta_edges: Vec::new(),
        node_count: n,
        csr_version: 0,
    };

    let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
    let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

    let budget_config = LabelBudgetConfig {
        budget_per_kind: 10_000_000,
        on_exceeded: BudgetExceededPolicy::Fail,
        density_gate_threshold: 0,
        skip_labels: false,
    };

    let start = Instant::now();
    let dag = CondensationDag::build_with_budget(&scc, &csr, &budget_config).unwrap();
    let duration = start.elapsed();

    println!("Density test completed in {duration:?}");
    println!("SCC count: {}", dag.scc_count);
    println!("Edge count: {}", dag.edge_count);

    // Check some intervals
    let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
    let scc_1 = scc.scc_of(NodeId::new(1, 0)).unwrap();
    #[allow(clippy::cast_possible_truncation)] // Test graph sizes are small constants
    let scc_last = scc.scc_of(NodeId::new((n - 1) as u32, 0)).unwrap();

    // Check if can_reach works for direct edge
    assert!(
        dag.can_reach(scc_0, scc_1),
        "Should reach direct successor 1"
    );
    assert!(
        dag.can_reach(scc_0, scc_last),
        "Should reach direct successor n-1"
    );
    assert!(
        !dag.can_reach(scc_last, scc_0),
        "Should not reach ancestor (no cycles)"
    );

    // Performance gate: should be sub-second even in debug build.
    // Before FastBitSet, this would involve millions of interval copies and sorts.
    assert!(
        duration.as_secs() < 5,
        "Reachability computation took too long: {duration:?}"
    );
}
