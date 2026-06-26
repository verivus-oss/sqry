//! `StructuralNeighborsQuery` — LSH index over per-function MinHash signatures
//! for sublinear structural-nearest-neighbour lookup (body-shape-descriptor
//! feature, U06).
//!
//! # What it builds
//!
//! Each Function/Method node carries a `ShapeDescriptor` (U01–U04) with a
//! 64-lane MinHash sketch of its identifier-blind structure. This query builds a
//! locality-sensitive-hashing (LSH) band index over those sketches: the 64 lanes
//! are split into `bands` bands of `rows` lanes each (default `16 x 4 = 64`), and
//! every node is bucketed by the hash of each band. Two functions are *candidate*
//! structural neighbours when they collide in at least one band — the standard
//! banding trick that makes approximate-Jaccard nearest-neighbour search
//! sublinear: a probe touches `bands` buckets instead of scanning all N nodes.
//!
//! The cached value is the whole band index (`Arc<StructuralLshIndex>`), built
//! once and reused; [`structural_neighbors`] then probes it for a given start
//! node and refines the small candidate set by exact `shape_hash` identity and
//! estimated Jaccard. This is the AC-4 "band-probe -> candidate set -> exact
//! refine, not a full scan" path.
//!
//! # Invalidation (AC-7)
//!
//! Tier-1 (file-revision) ONLY: `TRACKS_EDGE_REVISION = false`,
//! `TRACKS_METADATA_REVISION = false`. Shape descriptors are AST-derived per node
//! and committed in the build seam (U04); they do not depend on edge topology or
//! the metadata-revision counter, so a Tier-1 dep is exactly right. `execute`
//! records a file dep for every file it reads, so a single-file edit invalidates
//! the index (it is rebuilt from the descriptors of the changed graph) while
//! every other Tier-1 file-scoped query for unchanged files stays cache-warm.
//!
//! # Persistence
//!
//! The index serializes through the existing PN3 `PersistedEntry` postcard stream
//! under the `max_entry_size_bytes` (1 MiB default) cap. A pathological workspace
//! whose band index exceeds the cap simply is not persisted to
//! `.sqry/graph/derived.sqry` (soft-miss) and is recomputed on the next cold
//! start — the in-memory cache still holds it for the live process.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::storage::shape::MINHASH_LANES;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

/// Default LSH band count (matches `QueryDbConfig::structural_lsh_bands`).
pub const DEFAULT_LSH_BANDS: usize = 16;
/// Default LSH rows-per-band (matches `QueryDbConfig::structural_lsh_rows`).
pub const DEFAULT_LSH_ROWS: usize = 4;

/// Resolve a valid `(bands, rows)` banding from the DB config, falling back to
/// the default `16 x 4` when the configured pair is unusable (`0`, or
/// `bands * rows` overruns the lane count).
fn resolve_banding(db: &QueryDb) -> (usize, usize) {
    let cfg = db.config();
    let bands = cfg.structural_lsh_bands;
    let rows = cfg.structural_lsh_rows;
    if bands == 0 || rows == 0 || bands.saturating_mul(rows) > MINHASH_LANES {
        (DEFAULT_LSH_BANDS, DEFAULT_LSH_ROWS)
    } else {
        (bands, rows)
    }
}

/// Deterministic 64-bit hash of one band's lanes, decorrelated per band index so
/// the same lane values land in different buckets across bands.
///
/// FNV-1a over the little-endian lane bytes — no RNG, no wall clock, no external
/// seed registry (the index is SHA-gated to the snapshot and recomputed on
/// mismatch, so this is not an on-disk schema contract).
fn band_hash(band_index: usize, lanes: &[u32]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET ^ (band_index as u64).wrapping_mul(FNV_PRIME);
    for &lane in lanes {
        for b in lane.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

/// Estimated Jaccard similarity of two MinHash sketches: the fraction of lanes
/// that agree. Both sketches must have the same length (always [`MINHASH_LANES`]).
#[must_use]
fn minhash_jaccard(a: &[u32; MINHASH_LANES], b: &[u32; MINHASH_LANES]) -> f32 {
    let matching = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    #[allow(clippy::cast_precision_loss)]
    {
        matching as f32 / MINHASH_LANES as f32
    }
}

/// LSH band index over per-function MinHash signatures.
///
/// `band_tables[b]` maps a band hash to the sorted node ids whose descriptor
/// hashes to that bucket in band `b`. Deterministic (`BTreeMap` + sorted
/// `Vec<NodeId>`) for stable serialization and reproducible probe order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralLshIndex {
    /// Band count this index was built with.
    bands: usize,
    /// Rows-per-band this index was built with.
    rows: usize,
    /// One bucket table per band: band-hash -> sorted node ids.
    band_tables: Vec<BTreeMap<u64, Vec<NodeId>>>,
}

impl StructuralLshIndex {
    /// Number of bands.
    #[must_use]
    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Rows per band.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Total number of `(band, bucket)` postings (for observability / tests).
    #[must_use]
    pub fn posting_count(&self) -> usize {
        self.band_tables
            .iter()
            .map(|t| t.values().map(Vec::len).sum::<usize>())
            .sum()
    }

    /// Band hashes for one MinHash sketch under this index's banding.
    fn band_hashes(&self, minhash: &[u32; MINHASH_LANES]) -> Vec<u64> {
        (0..self.bands)
            .map(|b| {
                let start = b * self.rows;
                band_hash(b, &minhash[start..start + self.rows])
            })
            .collect()
    }

    /// Candidate node ids that collide with `minhash` in at least one band,
    /// excluding `exclude` (the probe itself). Deduplicated and sorted.
    #[must_use]
    pub fn candidates_for(&self, minhash: &[u32; MINHASH_LANES], exclude: NodeId) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::new();
        for (table, h) in self.band_tables.iter().zip(self.band_hashes(minhash)) {
            if let Some(bucket) = table.get(&h) {
                out.extend(bucket.iter().copied().filter(|&n| n != exclude));
            }
        }
        out.sort_unstable_by_key(|id| (id.index(), id.generation()));
        out.dedup();
        out
    }
}

/// Builds and caches the [`StructuralLshIndex`] over every hashable
/// Function/Method descriptor in the snapshot. See the module docs for the
/// invalidation and persistence contract.
pub struct StructuralNeighborsQuery;

// AC-7 freeze: the structural-neighbour index is Tier-1 (file-revision) only.
// Descriptors are AST-derived per node and committed in the build seam (U04);
// they carry no edge-topology or metadata-revision dependency. A compile-time
// assertion guards against a future edit silently widening the tier.
const _: () = assert!(!StructuralNeighborsQuery::TRACKS_EDGE_REVISION);
const _: () = assert!(!StructuralNeighborsQuery::TRACKS_METADATA_REVISION);

impl DerivedQuery for StructuralNeighborsQuery {
    type Key = ();
    type Value = Arc<StructuralLshIndex>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::STRUCTURAL_NEIGHBORS;
    // Tier-1 only: descriptors are AST-derived per node (U04 build seam); no
    // edge-topology or metadata-revision dependency (AC-7).
    const TRACKS_EDGE_REVISION: bool = false;
    const TRACKS_METADATA_REVISION: bool = false;

    fn execute(_key: &(), db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<StructuralLshIndex> {
        // Tier-1 cold-start correctness: record a dep on every file read, exactly
        // like `AddressTakenQuery`.
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        let (bands, rows) = resolve_banding(db);
        let mut band_tables: Vec<BTreeMap<u64, Vec<NodeId>>> =
            (0..bands).map(|_| BTreeMap::new()).collect();

        let descriptors = snapshot.macro_metadata().shape_descriptors();
        for (&node_id, descriptor) in descriptors {
            // An unhashable (sub-token-floor) descriptor carries a zeroed sketch
            // and no meaningful structure; banding it would collapse every tiny
            // body into one bucket. Skip it — it has no structural neighbours.
            if descriptor.is_unhashable() {
                continue;
            }
            for (b, table) in band_tables.iter_mut().enumerate() {
                let start = b * rows;
                let h = band_hash(b, &descriptor.minhash[start..start + rows]);
                table.entry(h).or_default().push(node_id);
            }
        }
        // Sort each bucket for deterministic probe order across runs.
        for table in &mut band_tables {
            for bucket in table.values_mut() {
                bucket.sort_unstable_by_key(|id| (id.index(), id.generation()));
            }
        }

        Arc::new(StructuralLshIndex {
            bands,
            rows,
            band_tables,
        })
    }
}

/// One structural-neighbour match, carrying the two distinct numbers the surfaces
/// report: exact structural identity (`shape_hash_exact`) and the approximate
/// MinHash Jaccard similarity (`jaccard`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructuralNeighbor {
    /// The neighbour node.
    pub node: NodeId,
    /// True when the neighbour's `shape_hash` is byte-identical to the probe's
    /// (an exact rename/relocate-invariant structural match), and non-zero.
    pub shape_hash_exact: bool,
    /// Estimated Jaccard similarity of the two MinHash sketches (`0.0..=1.0`).
    pub jaccard: f32,
}

/// Probe the cached structural index for `probe`'s nearest structural neighbours,
/// refined by exact `shape_hash` identity and estimated Jaccard.
///
/// Returns at most `max_results` neighbours with `jaccard >= similarity_floor`,
/// sorted exact-matches-first then by descending similarity (ties broken by node
/// id for determinism). Returns empty when `probe` has no hashable descriptor.
///
/// This is the sublinear AC-4 path: the band index is fetched from the cache
/// (built once), the probe touches `bands` buckets, and the small candidate set
/// is refined against descriptors read straight from the snapshot.
#[must_use]
pub fn structural_neighbors(
    db: &QueryDb,
    snapshot: &GraphSnapshot,
    probe: NodeId,
    similarity_floor: f32,
    max_results: usize,
) -> Vec<StructuralNeighbor> {
    let descriptors = snapshot.macro_metadata().shape_descriptors();
    let Some(probe_desc) = descriptors.get(&probe) else {
        return Vec::new();
    };
    if probe_desc.is_unhashable() {
        return Vec::new();
    }

    let index = db.get::<StructuralNeighborsQuery>(&());
    let candidates = index.candidates_for(&probe_desc.minhash, probe);

    let mut out: Vec<StructuralNeighbor> = Vec::new();
    for cand in candidates {
        let Some(cand_desc) = descriptors.get(&cand) else {
            continue;
        };
        if cand_desc.is_unhashable() {
            continue;
        }
        let jaccard = minhash_jaccard(&probe_desc.minhash, &cand_desc.minhash);
        if jaccard < similarity_floor {
            continue;
        }
        let shape_hash_exact =
            !probe_desc.shape_hash.is_zero() && cand_desc.shape_hash == probe_desc.shape_hash;
        out.push(StructuralNeighbor {
            node: cand,
            shape_hash_exact,
            jaccard,
        });
    }

    // Exact structural matches first, then highest Jaccard, then node id.
    out.sort_by(|a, b| {
        b.shape_hash_exact
            .cmp(&a.shape_hash_exact)
            .then(
                b.jaccard
                    .partial_cmp(&a.jaccard)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| {
                (a.node.index(), a.node.generation()).cmp(&(b.node.index(), b.node.generation()))
            })
    });
    out.truncate(max_results);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::node::Language;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::kind::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use sqry_core::graph::unified::storage::{ShapeDescriptor, ShapeHash128, SignatureShape};

    /// Build a descriptor whose MinHash is `lane_fill` in every lane (so two
    /// descriptors with the same fill collide in every band) plus a distinctive
    /// shape_hash.
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

    /// Seed a graph with `n` function nodes, inserting a descriptor for each via
    /// the closure. Returns the graph and the allocated node ids in order.
    fn seed(n: usize, mut make: impl FnMut(usize) -> ShapeDescriptor) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(std::path::Path::new("/tmp/lsh.rs"), Some(Language::Rust))
            .expect("register file");
        let mut ids = Vec::new();
        for i in 0..n {
            let name = graph
                .strings_mut()
                .intern(&format!("f{i}"))
                .expect("intern");
            let entry = NodeEntry::new(NodeKind::Function, name, file_id)
                .with_location(1 + i as u32, 0, 1 + i as u32, 5)
                .with_qualified_name(name);
            let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc");
            graph
                .indices_mut()
                .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
            graph
                .macro_metadata_mut()
                .insert_shape_descriptor(id, make(i));
            ids.push(id);
        }
        (graph, ids)
    }

    fn db_for(graph: CodeGraph) -> QueryDb {
        let snapshot = Arc::new(graph.snapshot());
        let mut db = QueryDb::new(snapshot, QueryDbConfig::default());
        db.register::<StructuralNeighborsQuery>();
        db
    }

    #[test]
    fn identical_sketches_are_candidates_distinct_are_not() {
        // Two functions with identical sketches + a third that differs.
        let (graph, ids) = seed(3, |i| match i {
            0 | 1 => descriptor(0xAAAA_AAAA, 0x11),
            _ => descriptor(0x5555_5555, 0x22),
        });
        let db = db_for(graph);
        let snapshot = db.snapshot();

        let neighbors = structural_neighbors(&db, snapshot, ids[0], 0.5, 10);
        assert_eq!(neighbors.len(), 1, "only the identical twin is a neighbour");
        assert_eq!(neighbors[0].node, ids[1]);
        assert!(neighbors[0].shape_hash_exact, "identical shape_hash");
        assert!((neighbors[0].jaccard - 1.0).abs() < f32::EPSILON);

        // The distinct function has no neighbour above the floor.
        let none = structural_neighbors(&db, snapshot, ids[2], 0.5, 10);
        assert!(none.is_empty());
    }

    #[test]
    fn unhashable_probe_returns_empty() {
        let (graph, ids) = seed(2, |_| {
            let mut d = descriptor(0x1234_5678, 0x33);
            d.flags.set_unhashable();
            d
        });
        let db = db_for(graph);
        let neighbors = structural_neighbors(&db, db.snapshot(), ids[0], 0.0, 10);
        assert!(
            neighbors.is_empty(),
            "an unhashable probe has no structural neighbours"
        );
    }

    #[test]
    fn band_probe_is_sublinear_not_a_full_scan() {
        // A large bucket of identical twins plus many distinct singletons. The
        // candidate set for a twin must be the twins only — NOT every node — so
        // the probe is sublinear in the singleton population.
        let twins = 5usize;
        let singletons = 500usize;
        let (graph, ids) = seed(twins + singletons, |i| {
            if i < twins {
                descriptor(0xC0FF_EE00, 0x44)
            } else {
                // Distinct sketch per singleton so they scatter across buckets.
                descriptor(0x1000_0000 + i as u32, 0x50 + i as u64)
            }
        });
        let db = db_for(graph);
        let snapshot = db.snapshot();
        let neighbors = structural_neighbors(&db, snapshot, ids[0], 0.99, twins + singletons);
        // Only the other 4 twins, never the 500 singletons.
        assert_eq!(neighbors.len(), twins - 1);
        assert!(neighbors.iter().all(|n| n.shape_hash_exact));
    }

    #[test]
    fn index_is_deterministic_across_two_builds() {
        let make = |i: usize| descriptor(0xABCD_0000 + (i % 3) as u32, 0x60 + (i % 3) as u64);
        let (g1, _) = seed(12, make);
        let (g2, _) = seed(12, make);
        let i1 = db_for(g1).get::<StructuralNeighborsQuery>(&());
        let i2 = db_for(g2).get::<StructuralNeighborsQuery>(&());
        // Byte-identical serialized index across two independent builds.
        let b1 = postcard::to_allocvec(&*i1).expect("serialize i1");
        let b2 = postcard::to_allocvec(&*i2).expect("serialize i2");
        assert_eq!(b1, b2, "index must be deterministic across builds");
        assert_eq!(i1.bands(), DEFAULT_LSH_BANDS);
        assert_eq!(i1.rows(), DEFAULT_LSH_ROWS);
    }

    #[test]
    fn index_persists_through_postcard_round_trip() {
        let (graph, _) = seed(4, |i| {
            descriptor(0x7000_0000 + (i % 2) as u32, 0x70 + i as u64)
        });
        let index = db_for(graph).get::<StructuralNeighborsQuery>(&());
        let bytes = postcard::to_allocvec(&*index).expect("serialize");
        let back: StructuralLshIndex = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(back.bands(), index.bands());
        assert_eq!(back.posting_count(), index.posting_count());
    }
}
