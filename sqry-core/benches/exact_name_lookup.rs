//! Criterion benchmark — warm exact-name lookup on a 1M-node `CodeGraph`.
//!
//! Establishes the baseline latency that verivus-oss/sqry#238 cites as
//! "should be sub-second on a warm in-memory index". The field user
//! observed 15s of silence on `sqry --exact start_kernel .` against the
//! Linux kernel; this bench confirms the underlying lookup itself is
//! sub-millisecond, isolating the latency to snapshot load / dispatch
//! overhead rather than the lookup primitive.
//!
//! The bench performs two roles:
//!   1. Criterion timing measurements (mean / std-dev / outlier reporting)
//!      for trend tracking across PRs.
//!   2. A hard p99 < 50ms assertion that fires `panic!` (and thus nonzero
//!      exit) on regression. Per the DAG `EXACT_NAME_BENCH` acceptance:
//!      "bench fails (returns nonzero) if p99 exceeds 50ms".
//!
//! The fixture is a 1M-node graph constructed deterministically (no RNG —
//! names are simply `symbol_NNNNNNN` for N in 0..1_000_000). This is
//! sufficient to characterize the O(log N) interner lookup + O(1) arena
//! hit cost; using more nodes would only inflate setup time.

use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use std::hint::black_box;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const NODE_COUNT: u32 = 1_000_000;
/// Name we look up — middle of the range, so the lookup must traverse
/// roughly half the interner table before resolving.
const HIT_NAME: &str = "symbol_0500000";
/// Name guaranteed NOT to be in the graph — exercises the "no match"
/// path.
const MISS_NAME: &str = "symbol_definitely_not_present";
/// Hard p99 ceiling for the regression-detector assertion. 50ms is wildly
/// generous for an O(log N) interner lookup but tight enough to catch
/// any future regression that pushes lookup into linear-scan territory.
const P99_CEILING: Duration = Duration::from_millis(50);
const SAMPLE_COUNT: usize = 1_000;

/// Build a 1M-node `CodeGraph` once per process and reuse across all
/// bench iterations. Building dominates the bench's total runtime
/// (~hundreds of ms) but isn't part of what we're measuring.
fn shared_graph() -> &'static CodeGraph {
    static GRAPH: OnceLock<CodeGraph> = OnceLock::new();
    GRAPH.get_or_init(build_million_node_graph)
}

fn build_million_node_graph() -> CodeGraph {
    let mut graph = CodeGraph::new();
    let file_id = graph
        .files_mut()
        .register(Path::new("bench.rs"))
        .expect("register bench file");

    // Names need to be stable across runs, so we use deterministic
    // formatting rather than an RNG-seeded scheme. The interner is sized
    // for the workload (1M entries) and Rayon is not used here — the
    // alloc/intern/index pipeline is serial.
    for i in 0..NODE_COUNT {
        let name = format!("symbol_{i:07}");
        let name_id = graph
            .strings_mut()
            .intern(&name)
            .expect("intern symbol name");
        let node_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_id, file_id))
            .expect("alloc node");
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, name_id, None, file_id);
    }

    graph
}

/// Assert the p99 latency ceiling. Runs `SAMPLE_COUNT` lookups, sorts the
/// sample times, and panics if the 99th-percentile sample exceeds
/// `P99_CEILING`. The bench process exits nonzero on panic, satisfying the
/// DAG acceptance "nonzero exit on regression".
fn assert_p99_under_ceiling(graph: &CodeGraph) {
    let snapshot = graph.snapshot();
    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let result = black_box(snapshot.find_by_exact_name(HIT_NAME));
        samples.push(start.elapsed());
        debug_assert_eq!(
            result.len(),
            1,
            "fixture invariant: each name maps to exactly one node"
        );
    }
    samples.sort_unstable();
    // 990th index is the 99th percentile for SAMPLE_COUNT=1000.
    let p50 = samples[SAMPLE_COUNT / 2];
    let p99 = samples[(SAMPLE_COUNT * 99) / 100];
    eprintln!("exact_name_lookup latency: p50={p50:?} p99={p99:?} (ceiling={P99_CEILING:?})");
    assert!(
        p99 < P99_CEILING,
        "find_by_exact_name p99 = {p99:?} exceeds ceiling {P99_CEILING:?} \
         on {NODE_COUNT}-node graph; this is a regression in the interner \
         or arena hot path. See verivus-oss/sqry#238 EXACT_NAME_BENCH."
    );
}

fn benchmark_exact_name_lookup(c: &mut Criterion) {
    let graph = shared_graph();
    let snapshot = graph.snapshot();

    // Manual p99 assertion first — exits the bench process nonzero on
    // regression before criterion's reporting can muddy the signal.
    assert_p99_under_ceiling(graph);

    let mut group = c.benchmark_group("exact_name_lookup");
    // 1000 samples is plenty for an O(log N) lookup; criterion's default
    // 100 is too few for distribution analysis when the per-call time is
    // sub-microsecond.
    group.sample_size(1000);

    group.bench_function("hit_middle_of_range", |b| {
        b.iter(|| black_box(snapshot.find_by_exact_name(HIT_NAME)));
    });

    group.bench_function("miss_no_such_name", |b| {
        b.iter(|| black_box(snapshot.find_by_exact_name(MISS_NAME)));
    });

    group.finish();
}

criterion_group!(benches, benchmark_exact_name_lookup);
criterion_main!(benches);
