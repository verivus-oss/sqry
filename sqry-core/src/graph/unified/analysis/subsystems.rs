//! Path/package subsystem aggregation (the `subsystems` primitive).
//!
//! Groups a codebase into subsystems by directory prefix and reports the
//! directed couplings between them. This is the report's PRIMARY grouping: a
//! cross-LLM design review concluded that symbol-level modularity is the wrong
//! primary grouping for an onboarding report (the resolution limit collapses
//! real modules at kernel scale, and undirected modularity discards call
//! direction), so the honest, deterministic path/package aggregation leads and
//! Louvain (see [`super::community`]) is an optional power-user primitive.
//!
//! Everything here is integer counts over sorted keys: O(V + E), no float, no
//! RNG, no cost tier, never skips. The output is byte-stable across runs and
//! architectures.
//!
//! # Two documented deviations from the design sketch
//!
//! 1. **`&GraphSnapshot`, not `&CompactionSnapshot`.** As with
//!    [`super::centrality`], the edge-only `CompactionSnapshot` cannot expose
//!    node kind / name / file, so we take the node-and-edge-bearing
//!    [`GraphSnapshot`].
//! 2. **Bucket keys are `String`, not `StringId`.** A subsystem key is a
//!    *synthesized* directory prefix (e.g. `crate_a/src`), not a symbol that
//!    already lives in the snapshot's interner; a read-only snapshot cannot
//!    intern new strings. The design's "crate/package when the language graph
//!    exposes module/package info" branch has no supporting per-file/per-node
//!    package accessor in the current graph, so the deterministic directory
//!    prefix (the design's own stated fallback) is used universally. For the
//!    languages whose directory layout mirrors the package (Rust crates, Go
//!    packages, Java package dirs, ...) that prefix *is* the package.

use std::collections::BTreeMap;
use std::path::Path;

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;

use super::centrality::{HubMetric, HubOpts, KindMask, rank_hubs};

/// The subset of edge kinds that count as a subsystem coupling.
///
/// A purpose-built enum (rather than the design's `EdgeKindDiscriminant`, which
/// is neither `Ord` nor `Serialize`) so couplings can be ranked deterministically
/// and serialized for `--format json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplingEdgeKind {
    /// A `Calls` edge.
    Calls,
    /// An `Imports` edge.
    Imports,
    /// A `References` edge.
    References,
}

impl CouplingEdgeKind {
    /// Classifies an [`EdgeKind`] as a coupling kind, if it is one.
    #[must_use]
    pub fn from_edge_kind(kind: &EdgeKind) -> Option<Self> {
        match kind {
            EdgeKind::Calls { .. } => Some(Self::Calls),
            EdgeKind::Imports { .. } => Some(Self::Imports),
            EdgeKind::References => Some(Self::References),
            _ => None,
        }
    }
}

/// Options for [`aggregate_subsystems`].
#[derive(Debug, Clone, Copy)]
pub struct SubsystemOpts {
    /// Maximum subsystems and couplings to return. `0` means unbounded.
    pub top: usize,
    /// Number of leading directory components that form a bucket key.
    pub group_depth: usize,
}

impl Default for SubsystemOpts {
    fn default() -> Self {
        Self {
            top: 10,
            group_depth: 2,
        }
    }
}

/// One aggregated subsystem (a directory-prefix bucket).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Subsystem {
    /// The directory-prefix key (e.g. `crate_a/src`).
    pub key: String,
    /// Number of stub-free symbols in the bucket.
    pub size: u32,
    /// Number of coupling edges whose endpoints both lie in this bucket.
    pub internal_edges: u64,
    /// Representative node: the bucket's top hub (see [`rank_hubs`]).
    pub representative: NodeId,
}

/// One directed coupling between two subsystems.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Coupling {
    /// Source bucket key.
    pub from: String,
    /// Target bucket key.
    pub to: String,
    /// Which coupling kind these edges are.
    pub kind: CouplingEdgeKind,
    /// Number of `from -> to` edges of this kind (direction preserved).
    pub count: u32,
}

/// The "normal" (named) path components, dropping root/prefix/`.`/`..`.
fn named_components(path: &Path) -> Vec<String> {
    use std::path::Component;
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Length of the longest common leading directory-component prefix across all
/// files, capped so the shortest path keeps at least its file name.
///
/// Stripping this common prefix (usually the workspace root, whether the
/// registry stored absolute or relative paths) makes bucket keys
/// workspace-relative and redaction-friendly, and is a pure function of the file
/// set, so the result stays byte-stable.
fn common_prefix_len(all: &[&Vec<String>]) -> usize {
    if all.is_empty() {
        return 0;
    }
    let min_len = all.iter().map(|c| c.len()).min().unwrap_or(0);
    let max_common = min_len.saturating_sub(1);
    let mut common = 0;
    while common < max_common {
        let first = &all[0][common];
        if all.iter().any(|comps| &comps[common] != first) {
            break;
        }
        common += 1;
    }
    common
}

/// Builds a bucket key from already-split components, dropping `common` leading
/// components then taking the first `group_depth` *directory* components (the
/// file name is always excluded).
fn key_from_components(comps: &[String], common: usize, group_depth: usize) -> String {
    let rem = &comps[common.min(comps.len())..];
    let dir_count = rem.len().saturating_sub(1);
    let take = group_depth.min(dir_count);
    if take == 0 {
        return "<root>".to_string();
    }
    rem[..take].join("/")
}

/// Computes the directory-prefix bucket key for a resolved file path with no
/// common-prefix stripping (the leading-prefix primitive; see
/// [`key_from_components`] for the workspace-relative form used by
/// [`aggregate_subsystems`]).
///
/// Takes the first `group_depth` *directory* components, joined by `/`. A file
/// at the root yields the reserved key `<root>`.
#[cfg(test)]
fn bucket_key(path: &Path, group_depth: usize) -> String {
    key_from_components(&named_components(path), 0, group_depth)
}

/// Aggregates `snapshot` into path/package subsystems and their directed
/// couplings.
///
/// Returns `(subsystems, couplings)`, each ranked and bounded by `opts.top`.
/// Subsystems rank by `(size desc, internal-edge density desc, key asc)`.
/// Couplings rank to surface the sparse-but-high-fan pairs first (large endpoint
/// buckets joined by few edges), tie-broken by `(ordered bucket-key pair, kind,
/// count)`. Every ordering key is integer or a resolved string, so the output is
/// byte-stable.
///
/// See the [module docs](self) for the `&GraphSnapshot` / `String`-key
/// deviations from the design sketch.
#[must_use]
pub fn aggregate_subsystems(
    snapshot: &GraphSnapshot,
    opts: &SubsystemOpts,
) -> (Vec<Subsystem>, Vec<Coupling>) {
    let node_slots = snapshot.nodes().slot_count();

    // --- Phase 1: assign every live node to a bucket (by its file). ---------
    // `bucket_of[index]` is the compact bucket id for that node index (None for
    // vacant slots). Keys are collected into a BTreeMap so compact ids are
    // assigned in sorted-key order: deterministic and byte-stable.
    let mut key_to_id: BTreeMap<String, u32> = BTreeMap::new();
    let mut bucket_of: Vec<Option<u32>> = vec![None; node_slots];
    let mut size: Vec<u32> = Vec::new();
    let mut internal_edges: Vec<u64> = Vec::new();
    let mut keys: Vec<String> = Vec::new();

    // Pass A: resolve each participating file's path components once, so we can
    // strip the common (workspace-root) prefix before keying.
    let mut file_comps: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (_id, entry) in snapshot.iter_nodes() {
        file_comps.entry(entry.file.index()).or_insert_with(|| {
            snapshot
                .files()
                .resolve(entry.file)
                .map(|p| named_components(&p))
                .unwrap_or_default()
        });
    }
    let all_comps: Vec<&Vec<String>> = file_comps.values().collect();
    let common = common_prefix_len(&all_comps);
    // file index -> bucket key (workspace-relative).
    let mut file_key: BTreeMap<u32, String> = BTreeMap::new();
    for (&file_index, comps) in &file_comps {
        let key = if comps.is_empty() {
            "<unknown>".to_string()
        } else {
            key_from_components(comps, common, opts.group_depth)
        };
        file_key.insert(file_index, key);
    }

    // Pass B: intern keys and record per-node bucket ids.
    for (id, entry) in snapshot.iter_nodes() {
        let idx = id.index() as usize;
        let key = file_key
            .get(&entry.file.index())
            .cloned()
            .unwrap_or_else(|| "<unknown>".to_string());
        let next_id = u32::try_from(key_to_id.len()).unwrap_or(u32::MAX);
        let bucket_id = *key_to_id.entry(key).or_insert(next_id);
        if let Some(slot) = bucket_of.get_mut(idx) {
            *slot = Some(bucket_id);
        }
    }

    // Materialize the sorted key list and size/internal vectors.
    // BTreeMap iterates in sorted key order, but the compact ids were assigned
    // on first-encounter (arena order), so remap ids to the sorted order for a
    // fully key-sorted, deterministic bucket numbering.
    let mut old_to_sorted: Vec<u32> = vec![0; key_to_id.len()];
    for (sorted_id, (key, old_id)) in key_to_id.iter().enumerate() {
        let sorted_id = u32::try_from(sorted_id).unwrap_or(u32::MAX);
        old_to_sorted[*old_id as usize] = sorted_id;
        keys.push(key.clone());
    }
    let bucket_count = keys.len();
    size.resize(bucket_count, 0);
    internal_edges.resize(bucket_count, 0);
    for id in bucket_of.iter_mut().flatten() {
        *id = old_to_sorted[*id as usize];
    }

    // --- Phase 2: bucket sizes over stub-free symbols. ----------------------
    for (id, entry) in snapshot.iter_nodes() {
        if !super::centrality::node_is_symbol(snapshot, entry) {
            continue;
        }
        let idx = id.index() as usize;
        if let Some(Some(bucket_id)) = bucket_of.get(idx)
            && let Some(count) = size.get_mut(*bucket_id as usize)
        {
            *count = count.saturating_add(1);
        }
    }

    // --- Phase 3: directed couplings + internal-edge counts. ----------------
    // Key: (from_bucket, to_bucket, coupling_kind) -> count. BTreeMap keeps a
    // canonical iteration order.
    let mut couplings: BTreeMap<(u32, u32, CouplingEdgeKind), u32> = BTreeMap::new();
    for edge in snapshot.edges().all_live_forward_edges() {
        let Some(kind) = CouplingEdgeKind::from_edge_kind(&edge.kind) else {
            continue;
        };
        let src = edge.source.index() as usize;
        let tgt = edge.target.index() as usize;
        let (Some(Some(from)), Some(Some(to))) = (bucket_of.get(src), bucket_of.get(tgt)) else {
            continue;
        };
        if from == to {
            if let Some(count) = internal_edges.get_mut(*from as usize) {
                *count = count.saturating_add(1);
            }
        } else {
            *couplings.entry((*from, *to, kind)).or_insert(0) += 1;
        }
    }

    // --- Phase 4: representatives (top hub per bucket). ---------------------
    // Rank all eligible hubs once (top = 0 = unbounded), then the first hub in
    // each bucket by that global order is the bucket's representative.
    let hub_opts = HubOpts {
        top: 0,
        by: HubMetric::FanIn,
        kinds: KindMask::default(),
    };
    let mut representative: Vec<Option<NodeId>> = vec![None; bucket_count];
    for hub in rank_hubs(snapshot, &hub_opts) {
        let idx = hub.node.index() as usize;
        if let Some(Some(bucket_id)) = bucket_of.get(idx) {
            let slot = &mut representative[*bucket_id as usize];
            if slot.is_none() {
                *slot = Some(hub.node);
            }
        }
    }
    // Fallback: buckets with no ranked hub take their lowest-index stub-free
    // symbol (arena order is ascending, so first seen wins).
    for (id, entry) in snapshot.iter_nodes() {
        if !super::centrality::node_is_symbol(snapshot, entry) {
            continue;
        }
        let idx = id.index() as usize;
        if let Some(Some(bucket_id)) = bucket_of.get(idx) {
            let slot = &mut representative[*bucket_id as usize];
            if slot.is_none() {
                *slot = Some(id);
            }
        }
    }

    // --- Phase 5: rank subsystems. ------------------------------------------
    let mut subsystems: Vec<Subsystem> = (0..bucket_count)
        .filter_map(|b| {
            let sz = size[b];
            if sz == 0 {
                return None; // only report buckets that hold at least one symbol
            }
            representative[b].map(|rep| Subsystem {
                key: keys[b].clone(),
                size: sz,
                internal_edges: internal_edges[b],
                representative: rep,
            })
        })
        .collect();
    subsystems.sort_by(|a, b| {
        b.size
            .cmp(&a.size) // size descending
            .then_with(|| b.internal_edges.cmp(&a.internal_edges)) // density descending
            .then_with(|| a.key.cmp(&b.key)) // key ascending
    });
    if opts.top != 0 && subsystems.len() > opts.top {
        subsystems.truncate(opts.top);
    }

    // --- Phase 6: rank couplings (sparse-but-high-fan first). ---------------
    // Carry the bucket ids alongside each row so the interestingness comparator
    // reads sizes in O(1); interestingness = size(from) * size(to) / count,
    // compared exactly by u128 cross-multiplication (no float, no division).
    // Larger endpoint buckets joined by fewer edges rank first.
    let mut coupling_rows: Vec<(Coupling, u32, u32)> = couplings
        .into_iter()
        .map(|((from, to, kind), count)| {
            (
                Coupling {
                    from: keys[from as usize].clone(),
                    to: keys[to as usize].clone(),
                    kind,
                    count,
                },
                from,
                to,
            )
        })
        .collect();
    coupling_rows.sort_by(|(a, a_from, a_to), (b, b_from, b_to)| {
        let a_prod =
            u128::from(size[*a_from as usize]).saturating_mul(u128::from(size[*a_to as usize]));
        let b_prod =
            u128::from(size[*b_from as usize]).saturating_mul(u128::from(size[*b_to as usize]));
        // a.prod / a.count vs b.prod / b.count -> a.prod*b.count vs b.prod*a.count
        let lhs = a_prod.saturating_mul(u128::from(b.count));
        let rhs = b_prod.saturating_mul(u128::from(a.count));
        rhs.cmp(&lhs) // interestingness descending
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.count.cmp(&b.count))
    });
    let mut couplings_out: Vec<Coupling> =
        coupling_rows.into_iter().map(|(row, _, _)| row).collect();
    if opts.top != 0 && couplings_out.len() > opts.top {
        couplings_out.truncate(opts.top);
    }

    (subsystems, couplings_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::PathBuf;

    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    fn imports() -> EdgeKind {
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        }
    }

    fn register_file(graph: &mut CodeGraph, path: &str) -> FileId {
        graph
            .files_mut()
            .register_with_language(&PathBuf::from(path), Some(Language::Rust))
            .expect("register file")
    }

    fn add_fn(graph: &mut CodeGraph, name: &str, file: FileId) -> NodeId {
        let sid = graph.strings_mut().intern(name).expect("intern name");
        let entry = NodeEntry::new(NodeKind::Function, sid, file)
            .with_definition(true)
            .with_byte_range(0, 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph
            .indices_mut()
            .add(id, NodeKind::Function, sid, Some(sid), file);
        id
    }

    fn edge(graph: &CodeGraph, from: NodeId, to: NodeId, kind: EdgeKind, file: FileId) {
        graph.edges().add_edge(from, to, kind, file);
    }

    fn subsystem<'a>(subs: &'a [Subsystem], key: &str) -> Option<&'a Subsystem> {
        subs.iter().find(|s| s.key == key)
    }

    fn coupling<'a>(
        couplings: &'a [Coupling],
        from: &str,
        to: &str,
        kind: CouplingEdgeKind,
    ) -> Option<&'a Coupling> {
        couplings
            .iter()
            .find(|c| c.from == from && c.to == to && c.kind == kind)
    }

    #[test]
    fn bucket_key_directory_prefix() {
        assert_eq!(
            bucket_key(Path::new("crate_a/src/foo/bar.rs"), 2),
            "crate_a/src"
        );
        assert_eq!(
            bucket_key(Path::new("crate_a/src/foo/bar.rs"), 1),
            "crate_a"
        );
        assert_eq!(bucket_key(Path::new("top.rs"), 2), "<root>");
        assert_eq!(bucket_key(Path::new("/abs/crate/src/x.rs"), 2), "abs/crate");
    }

    #[test]
    fn bucket_assignment_at_group_depth() {
        let mut graph = CodeGraph::new();
        let fa1 = register_file(&mut graph, "crate_a/src/a1.rs");
        let fa2 = register_file(&mut graph, "crate_a/src/a2.rs");
        let fb1 = register_file(&mut graph, "crate_b/lib/b1.rs");
        add_fn(&mut graph, "a1", fa1);
        add_fn(&mut graph, "a2", fa2);
        add_fn(&mut graph, "b1", fb1);
        let snapshot = graph.snapshot();

        let (subs, _) = aggregate_subsystems(&snapshot, &SubsystemOpts::default());
        assert_eq!(subsystem(&subs, "crate_a/src").map(|s| s.size), Some(2));
        assert_eq!(subsystem(&subs, "crate_b/lib").map(|s| s.size), Some(1));

        let (subs1, _) = aggregate_subsystems(
            &snapshot,
            &SubsystemOpts {
                top: 0,
                group_depth: 1,
            },
        );
        assert_eq!(subsystem(&subs1, "crate_a").map(|s| s.size), Some(2));
        assert_eq!(subsystem(&subs1, "crate_b").map(|s| s.size), Some(1));
    }

    #[test]
    fn couplings_preserve_direction_and_kind() {
        let mut graph = CodeGraph::new();
        let fa = register_file(&mut graph, "crate_a/src/a.rs");
        let fb1 = register_file(&mut graph, "crate_b/src/b1.rs");
        let fb2 = register_file(&mut graph, "crate_b/src/b2.rs");
        let a = add_fn(&mut graph, "a", fa);
        let b1 = add_fn(&mut graph, "b1", fb1);
        let b2 = add_fn(&mut graph, "b2", fb2);

        // crate_a/src -> crate_b/src : 2 Calls + 1 Imports.
        edge(&graph, a, b1, calls(), fa);
        edge(&graph, a, b2, calls(), fa);
        edge(&graph, a, b1, imports(), fa);
        // crate_b/src -> crate_a/src : 1 Calls (reverse, distinct).
        edge(&graph, b1, a, calls(), fb1);

        let snapshot = graph.snapshot();
        let (_, couplings) = aggregate_subsystems(
            &snapshot,
            &SubsystemOpts {
                top: 0,
                group_depth: 2,
            },
        );

        assert_eq!(
            coupling(
                &couplings,
                "crate_a/src",
                "crate_b/src",
                CouplingEdgeKind::Calls
            )
            .map(|c| c.count),
            Some(2)
        );
        assert_eq!(
            coupling(
                &couplings,
                "crate_a/src",
                "crate_b/src",
                CouplingEdgeKind::Imports
            )
            .map(|c| c.count),
            Some(1)
        );
        assert_eq!(
            coupling(
                &couplings,
                "crate_b/src",
                "crate_a/src",
                CouplingEdgeKind::Calls
            )
            .map(|c| c.count),
            Some(1)
        );
        // Direction is preserved: there is no crate_b/src -> crate_a/src Imports.
        assert!(
            coupling(
                &couplings,
                "crate_b/src",
                "crate_a/src",
                CouplingEdgeKind::Imports
            )
            .is_none()
        );
    }

    #[test]
    fn coupling_ranking_surfaces_sparse_high_fan_first() {
        let mut graph = CodeGraph::new();
        // Two large buckets (5 symbols each) joined by a single edge.
        let big1 = register_file(&mut graph, "big1/src/m.rs");
        let big2 = register_file(&mut graph, "big2/src/m.rs");
        let mut big1_fns = Vec::new();
        let mut big2_fns = Vec::new();
        for i in 0..5 {
            big1_fns.push(add_fn(&mut graph, &format!("b1f{i}"), big1));
            big2_fns.push(add_fn(&mut graph, &format!("b2f{i}"), big2));
        }
        edge(&graph, big1_fns[0], big2_fns[0], calls(), big1);

        // Two small buckets (2 symbols each) densely joined (4 edges).
        let small1 = register_file(&mut graph, "small1/src/m.rs");
        let small2 = register_file(&mut graph, "small2/src/m.rs");
        let s1: Vec<NodeId> = (0..2)
            .map(|i| add_fn(&mut graph, &format!("s1f{i}"), small1))
            .collect();
        let s2: Vec<NodeId> = (0..2)
            .map(|i| add_fn(&mut graph, &format!("s2f{i}"), small2))
            .collect();
        for &from in &s1 {
            for &to in &s2 {
                edge(&graph, from, to, calls(), small1);
            }
        }

        let snapshot = graph.snapshot();
        let (_, couplings) = aggregate_subsystems(
            &snapshot,
            &SubsystemOpts {
                top: 0,
                group_depth: 2,
            },
        );
        assert!(!couplings.is_empty());
        // size(big)^2 / 1 = 25 beats size(small)^2 / 4 = 1.
        assert_eq!(couplings[0].from, "big1/src");
        assert_eq!(couplings[0].to, "big2/src");
    }

    #[test]
    fn representative_is_bucket_top_hub() {
        let mut graph = CodeGraph::new();
        let fa1 = register_file(&mut graph, "crate_a/src/a1.rs");
        let fa2 = register_file(&mut graph, "crate_a/src/a2.rs");
        // A second divergent top-level dir so the common prefix stops at the
        // workspace root and `crate_a/src` survives as a named bucket.
        let fb = register_file(&mut graph, "crate_b/src/b.rs");
        add_fn(&mut graph, "b", fb);
        let a1 = add_fn(&mut graph, "a1", fa1);
        let a2 = add_fn(&mut graph, "a2", fa2);
        // Give a1 the highest fan-in in the bucket.
        edge(&graph, a2, a1, calls(), fa2);
        let callers: Vec<NodeId> = (0..3)
            .map(|i| add_fn(&mut graph, &format!("caller{i}"), fa2))
            .collect();
        for c in callers {
            edge(&graph, c, a1, calls(), fa2);
        }

        let snapshot = graph.snapshot();
        let (subs, _) = aggregate_subsystems(&snapshot, &SubsystemOpts::default());
        let bucket = subsystem(&subs, "crate_a/src").expect("bucket present");
        assert_eq!(bucket.representative, a1, "top hub represents the bucket");
        let _ = a2;
    }

    #[test]
    fn output_is_deterministic() {
        let mut graph = CodeGraph::new();
        let fa = register_file(&mut graph, "crate_a/src/a.rs");
        let fb = register_file(&mut graph, "crate_b/src/b.rs");
        let a = add_fn(&mut graph, "a", fa);
        let b = add_fn(&mut graph, "b", fb);
        edge(&graph, a, b, calls(), fa);
        edge(&graph, b, a, calls(), fb);
        let snapshot = graph.snapshot();

        let first = aggregate_subsystems(&snapshot, &SubsystemOpts::default());
        for _ in 0..5 {
            assert_eq!(
                aggregate_subsystems(&snapshot, &SubsystemOpts::default()),
                first
            );
        }
    }
}
