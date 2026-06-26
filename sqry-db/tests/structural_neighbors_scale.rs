//! U15 — AC-4 scale gate: structural nearest-neighbour lookup stays sublinear on
//! a synthetic >=100k-symbol workspace.
//!
//! The LSH band index must turn a probe into a *small candidate set* (the nodes
//! sharing at least one MinHash band with the probe), refined by exact
//! `shape_hash` identity and estimated Jaccard. The defining property of AC-4 is
//! that this candidate set does NOT grow with the singleton population: probing a
//! cluster of K structural twins embedded in 100k otherwise-distinct functions
//! must touch ~K candidates, never all 100k. We assert that directly against
//! [`StructuralLshIndex::candidates_for`] (a deterministic, timing-free proof of
//! sublinearity) and confirm the public [`structural_neighbors`] surface returns
//! the AC-4 two-number output (`shape_hash_exact` + `jaccard`) per neighbour.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::storage::shape::MINHASH_LANES;
use sqry_core::graph::unified::storage::{ShapeDescriptor, ShapeHash128, SignatureShape};
use sqry_db::queries::{StructuralNeighborsQuery, structural_neighbors};
use sqry_db::{QueryDb, QueryDbConfig};

/// A descriptor whose MinHash is `lane_fill` in every lane (so two descriptors
/// with the same fill collide in every band) plus a distinctive `shape_hash`.
fn descriptor(lane_fill: u32, shape_high: u64) -> ShapeDescriptor {
    ShapeDescriptor {
        minhash: [lane_fill; MINHASH_LANES],
        signature_shape: SignatureShape::default(),
        shape_hash: ShapeHash128 {
            high: shape_high,
            low: shape_high.wrapping_add(1),
        },
        ..ShapeDescriptor::default()
    }
}

/// Seed a graph with `twins` structural twins (identical sketch + shape_hash)
/// followed by `singletons` distinct functions (each its own sketch). Returns the
/// graph and the twin node ids.
fn seed_scale_graph(twins: usize, singletons: usize) -> (CodeGraph, Vec<NodeId>) {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("scale.rs"), Some(Language::Rust))
        .expect("register file");

    let total = twins + singletons;
    let mut twin_ids = Vec::with_capacity(twins);
    for i in 0..total {
        let name_id = graph
            .strings_mut()
            .intern(&format!("f{i}"))
            .expect("intern");
        let start = i as u32;
        let entry = NodeEntry::new(NodeKind::Function, name_id, file)
            .with_qualified_name(name_id)
            .with_byte_range(start, start + 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc");
        graph
            .indices_mut()
            .add(id, NodeKind::Function, name_id, Some(name_id), file);

        let desc = if i < twins {
            // One shared structural identity for the whole twin cluster.
            descriptor(0xC0FF_EE00, 0x4242)
        } else {
            // A distinct sketch per singleton so they scatter across bands and
            // never share a band with the twin cluster.
            descriptor(0x1000_0000u32.wrapping_add(i as u32), 0x5000 + i as u64)
        };
        let id_inserted = id;
        graph
            .macro_metadata_mut()
            .insert_shape_descriptor(id_inserted, desc);
        if i < twins {
            twin_ids.push(id);
        }
    }
    (graph, twin_ids)
}

#[test]
fn ac4_band_probe_is_sublinear_on_100k_workspace() {
    // 8 structural twins hidden among 100_000 distinct functions: 100_008 total.
    let twins = 8usize;
    let singletons = 100_000usize;
    let (graph, twin_ids) = seed_scale_graph(twins, singletons);
    let total = twins + singletons;
    assert_eq!(graph.node_count(), total, "100k+ symbol workspace built");

    let snapshot = Arc::new(graph.snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    // The LSH index is built once and cached. The candidate set for a twin probe
    // is the twins only (they share every band); it must NOT scale with the
    // 100k singleton population. This is the timing-free sublinearity proof.
    let probe = twin_ids[0];
    let probe_desc = db
        .snapshot()
        .macro_metadata()
        .shape_descriptors()
        .get(&probe)
        .expect("probe descriptor")
        .clone();
    let index = db.get::<StructuralNeighborsQuery>(&());
    let candidates = index.candidates_for(&probe_desc.minhash, probe);
    assert_eq!(
        candidates.len(),
        twins - 1,
        "band probe must return only the {} other twins out of {total} functions, not a full scan",
        twins - 1
    );
    assert!(
        candidates.len() < total / 100,
        "candidate set ({}) must be sublinear in the {total}-symbol population",
        candidates.len()
    );

    // The public surface refines that candidate set into the AC-4 two-number
    // output: exact structural identity plus estimated Jaccard, per neighbour.
    let neighbors = structural_neighbors(&db, db.snapshot(), probe, 0.99, total);
    assert_eq!(neighbors.len(), twins - 1, "all other twins are neighbours");
    for n in &neighbors {
        assert!(
            n.shape_hash_exact,
            "AC-4: exact shape_hash identity reported"
        );
        assert!(
            (n.jaccard - 1.0).abs() < f32::EPSILON,
            "AC-4: within-match Jaccard reported (==1.0 for identical twins)"
        );
        assert!(
            twin_ids.contains(&n.node),
            "neighbour is a twin, not a singleton"
        );
    }

    // A distinct singleton has no structural neighbour above the floor: the band
    // probe does not drag the twin cluster (or any other singleton) in.
    let singleton = db
        .snapshot()
        .macro_metadata()
        .shape_descriptors()
        .keys()
        .copied()
        .find(|id| !twin_ids.contains(id))
        .expect("a singleton node exists");
    let none = structural_neighbors(&db, db.snapshot(), singleton, 0.5, total);
    assert!(
        none.is_empty(),
        "a distinct singleton has no structural neighbours, got {}",
        none.len()
    );
}
