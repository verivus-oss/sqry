use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::EdgeKind;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{
    Direction, PathPattern, PlanNode, Predicate, QueryPlan, SetOperation, StringPattern,
    execute_batch, fuse_plans,
};
use sqry_db::{QueryDb, QueryDbConfig};

fn add_node(graph: &mut CodeGraph, entry: NodeEntry) -> NodeId {
    let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
    graph
        .indices_mut()
        .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
    id
}

fn build_wide_fixture() -> Arc<GraphSnapshot> {
    let mut graph = CodeGraph::new();

    const FILES: usize = 10;
    const FUNCS_PER_FILE: usize = 20;
    const METHODS_PER_FILE: usize = 20;

    let mut file_ids = Vec::with_capacity(FILES);
    for idx in 0..FILES {
        let path = format!("src/mod_{idx:02}.rs");
        let fid = graph
            .files_mut()
            .register_with_language(Path::new(&path), Some(Language::Rust))
            .expect("register file");
        file_ids.push(fid);
    }

    let public_vis = graph.strings_mut().intern("public").expect("intern vis");

    for (file_index, &file_id) in file_ids.iter().enumerate() {
        for entry_index in 0..FUNCS_PER_FILE {
            let raw = format!("fn_{file_index}_{entry_index}");
            let name = graph.strings_mut().intern(&raw).expect("intern fn name");
            add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Function, name, file_id)
                    .with_qualified_name(name)
                    .with_byte_range((entry_index as u32) * 100, (entry_index as u32) * 100 + 80)
                    .with_visibility(public_vis),
            );
        }

        for entry_index in 0..METHODS_PER_FILE {
            let raw = format!("m_{file_index}_{entry_index}");
            let name = graph
                .strings_mut()
                .intern(&raw)
                .expect("intern method name");
            add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Method, name, file_id)
                    .with_qualified_name(name)
                    .with_byte_range(
                        2000 + (entry_index as u32) * 100,
                        2080 + (entry_index as u32) * 100,
                    ),
            );
        }
    }

    Arc::new(graph.snapshot())
}

fn scan(kind: NodeKind) -> PlanNode {
    PlanNode::NodeScan {
        kind: Some(kind),
        visibility: None,
        name_pattern: None,
    }
}

fn filter_has_caller() -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::HasCaller,
    }
}

fn filter_has_callee() -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::HasCallee,
    }
}

fn filter_in_file(glob: &str) -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::InFile(PathPattern::new(glob)),
    }
}

fn filter_matches_name(glob: &str) -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::MatchesName(StringPattern::glob(glob)),
    }
}

fn traverse_calls() -> PlanNode {
    PlanNode::EdgeTraversal {
        direction: Direction::Forward,
        edge_kind: Some(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        }),
        max_depth: 1,
    }
}

fn chain(steps: Vec<PlanNode>) -> QueryPlan {
    QueryPlan::new(PlanNode::Chain { steps })
}

fn standalone(node: PlanNode) -> QueryPlan {
    QueryPlan::new(node)
}

fn realistic_template() -> Vec<QueryPlan> {
    let setop = PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan(NodeKind::Function)),
        right: Box::new(scan(NodeKind::Method)),
    };

    vec![
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        chain(vec![scan(NodeKind::Function), filter_has_callee()]),
        chain(vec![scan(NodeKind::Class), filter_in_file("src/**")]),
        standalone(scan(NodeKind::Method)),
        standalone(setop.clone()),
        chain(vec![setop, traverse_calls()]),
        chain(vec![scan(NodeKind::Function), traverse_calls()]),
    ]
}

fn scaled_realistic_batch(target_count: usize) -> Vec<QueryPlan> {
    let template = realistic_template();
    template
        .iter()
        .cloned()
        .cycle()
        .take(target_count)
        .collect()
}

fn overlapping_subtree_batch(target_count: usize) -> Vec<QueryPlan> {
    let mut plans = Vec::with_capacity(target_count);

    for index in 0..target_count {
        let suffix = if index % 2 == 0 {
            filter_in_file(&format!("src/mod_{:02}.rs", index % 10))
        } else {
            filter_matches_name(&format!("fn_*_{}", index % 20))
        };

        plans.push(chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            suffix,
        ]));
    }

    plans
}

fn fused_postcard_bytes(plans: &[QueryPlan]) -> usize {
    let batch = fuse_plans(plans.to_vec());
    postcard::to_allocvec(&batch)
        .expect("serialize fused batch")
        .len()
}

fn operator_count(plans: &[QueryPlan]) -> usize {
    plans.iter().map(|plan| plan.root.operator_count()).sum()
}

fn bench_fuse_plans(c: &mut Criterion) {
    let plans = scaled_realistic_batch(100);
    let mut group = c.benchmark_group("planner_fuse");
    group.throughput(Throughput::Elements(plans.len() as u64));
    group.sample_size(20);

    let baseline_postcard_bytes = fused_postcard_bytes(&plans);
    let baseline_operator_count = operator_count(&plans);

    group.bench_with_input(
        BenchmarkId::new("realistic_mixed_batch", plans.len()),
        &plans,
        |bencher, input_plans| {
            bencher.iter(|| {
                black_box(fuse_plans(black_box(input_plans.to_vec())));
            });
        },
    );

    group.finish();

    eprintln!(
        "fusion_bench realistic_mixed_batch size={} postcard_bytes={} structural_operator_count={}",
        plans.len(),
        baseline_postcard_bytes,
        baseline_operator_count,
    );
}

fn bench_execute_batch(c: &mut Criterion) {
    let plans = overlapping_subtree_batch(100);
    let db = QueryDb::new(build_wide_fixture(), QueryDbConfig::default());
    let mut group = c.benchmark_group("planner_execute_batch");
    group.throughput(Throughput::Elements(plans.len() as u64));
    group.sample_size(20);

    let baseline_postcard_bytes = fused_postcard_bytes(&plans);
    let baseline_operator_count = operator_count(&plans);

    group.bench_with_input(
        BenchmarkId::new("overlapping_subtree_batch", plans.len()),
        &plans,
        |bencher, input_plans| {
            bencher.iter(|| {
                db.invalidate_all();
                let batch = fuse_plans(black_box(input_plans.to_vec()));
                black_box(execute_batch(black_box(&batch), black_box(&db)));
            });
        },
    );

    group.finish();

    eprintln!(
        "fusion_bench overlapping_subtree_batch size={} postcard_bytes={} structural_operator_count={}",
        plans.len(),
        baseline_postcard_bytes,
        baseline_operator_count,
    );
}

criterion_group!(fusion_benches, bench_fuse_plans, bench_execute_batch);
criterion_main!(fusion_benches);
