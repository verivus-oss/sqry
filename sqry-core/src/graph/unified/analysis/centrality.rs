//! Degree-centrality hub ranking (the `hubs` primitive).
//!
//! Ranks the most load-bearing symbols in a graph by degree centrality. For
//! every ranked node we count its incoming and outgoing `Calls` + `References`
//! edges (`fan_in` / `fan_out`) in a single O(V + E) integer sweep, then rank by
//! the selected metric. A high `fan_in` marks a well-depended-upon core API; a
//! high `fan_out` marks a broad orchestrator; the product surfaces the true
//! hubs that both know and are known.
//!
//! The whole path is integer and float-free, so the ranking is byte-stable
//! across runs and architectures: ties break deterministically by
//! `(node kind, resolved name, node index)`.
//!
//! # API surface deviation (documented)
//!
//! The design pack (`docs/development/sqry-overview/02_DESIGN`) sketched
//! `rank_hubs(snapshot: &CompactionSnapshot, ..)`. The real
//! [`CompactionSnapshot`](crate::graph::unified::compaction::CompactionSnapshot)
//! is an *edge-only* structure (`csr_edges` / `delta_edges` / `node_count`); it
//! carries no `NodeKind`, interned name, or file, so it cannot back a ranking
//! that filters by kind and reports names. The load-bearing computation needs
//! both node metadata and edges, which is exactly what
//! [`GraphSnapshot`] bundles
//! (`nodes()` / `edges()` / `strings()` / `files()`). We therefore take a
//! `&GraphSnapshot`. The edge scan uses
//! [`BidirectionalEdgeStore::all_live_forward_edges`](crate::graph::unified::edge::BidirectionalEdgeStore::all_live_forward_edges),
//! the O(|csr| + |delta|) graph-wide edge iterator, which is the degree
//! transpose the design called for done as one fused pass (a single walk fills
//! both the fan-in and fan-out arrays).

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::storage::arena::NodeEntry;
use crate::graph::unified::string::StringId;

/// Which degree metric drives a hub's score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HubMetric {
    /// Most depended-upon symbols: incoming `Calls` + `References`. Default.
    #[default]
    FanIn,
    /// Broad orchestrators: outgoing `Calls` + `References`.
    FanOut,
    /// True hubs (`fan_in * fan_out`): both know and are known.
    Combined,
}

/// Bitmask over [`NodeKind`] selecting which kinds are eligible to be ranked.
///
/// The default is the real API surface a newcomer should learn first:
/// `Function`, `Method`, `Type`, `Class`, `Trait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindMask(u64);

impl KindMask {
    /// An empty mask (matches nothing).
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Returns a copy of this mask with `kind` added.
    #[must_use]
    pub const fn inserting(self, kind: NodeKind) -> Self {
        Self(self.0 | (1u64 << kind_bit(kind)))
    }

    /// Builds a mask from a slice of kinds.
    #[must_use]
    pub fn from_kinds(kinds: &[NodeKind]) -> Self {
        let mut mask = Self::empty();
        for &kind in kinds {
            mask = mask.inserting(kind);
        }
        mask
    }

    /// Whether `kind` is a member of this mask.
    #[must_use]
    pub const fn contains(self, kind: NodeKind) -> bool {
        self.0 & (1u64 << kind_bit(kind)) != 0
    }

    /// Whether the mask selects nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Default for KindMask {
    fn default() -> Self {
        Self::from_kinds(&[
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Type,
            NodeKind::Class,
            NodeKind::Trait,
        ])
    }
}

/// Assigns each [`NodeKind`] a unique, stable bit index (0..=34).
///
/// The mapping is fixed for on-machine determinism only (it is never persisted),
/// so its exact values are an internal detail. `NodeKind` has fewer than 64
/// variants, so a `u64` mask covers every kind.
const fn kind_bit(kind: NodeKind) -> u32 {
    match kind {
        NodeKind::Function => 0,
        NodeKind::Method => 1,
        NodeKind::Class => 2,
        NodeKind::Interface => 3,
        NodeKind::Trait => 4,
        NodeKind::Module => 5,
        NodeKind::Variable => 6,
        NodeKind::Constant => 7,
        NodeKind::Type => 8,
        NodeKind::Struct => 9,
        NodeKind::Enum => 10,
        NodeKind::EnumVariant => 11,
        NodeKind::Macro => 12,
        NodeKind::Parameter => 13,
        NodeKind::Property => 14,
        NodeKind::CallSite => 15,
        NodeKind::Import => 16,
        NodeKind::Export => 17,
        NodeKind::StyleRule => 18,
        NodeKind::StyleAtRule => 19,
        NodeKind::StyleVariable => 20,
        NodeKind::Lifetime => 21,
        NodeKind::Component => 22,
        NodeKind::Service => 23,
        NodeKind::Resource => 24,
        NodeKind::Endpoint => 25,
        NodeKind::Test => 26,
        NodeKind::TypeParameter => 27,
        NodeKind::Annotation => 28,
        NodeKind::AnnotationValue => 29,
        NodeKind::LambdaTarget => 30,
        NodeKind::JavaModule => 31,
        NodeKind::EnumConstant => 32,
        NodeKind::Channel => 33,
        NodeKind::Other => 34,
    }
}

/// Options for [`rank_hubs`].
#[derive(Debug, Clone, Copy)]
pub struct HubOpts {
    /// Maximum number of hubs to return. `0` means unbounded (return all
    /// eligible nodes in ranked order).
    pub top: usize,
    /// Which degree metric ranks the hubs.
    pub by: HubMetric,
    /// Which node kinds are eligible to be ranked.
    pub kinds: KindMask,
}

impl Default for HubOpts {
    fn default() -> Self {
        Self {
            top: 10,
            by: HubMetric::default(),
            kinds: KindMask::default(),
        }
    }
}

/// One ranked hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubRank {
    /// The ranked node.
    pub node: NodeId,
    /// Its interned simple name.
    pub name: StringId,
    /// Its node kind.
    pub kind: NodeKind,
    /// Incoming `Calls` + `References` edge count.
    pub fan_in: u32,
    /// Outgoing `Calls` + `References` edge count.
    pub fan_out: u32,
}

impl HubRank {
    /// The score under a given metric, widened to avoid overflow.
    #[must_use]
    pub fn score(&self, by: HubMetric) -> u64 {
        match by {
            HubMetric::FanIn => u64::from(self.fan_in),
            HubMetric::FanOut => u64::from(self.fan_out),
            HubMetric::Combined => u64::from(self.fan_in) * u64::from(self.fan_out),
        }
    }
}

/// Whether a node is a real, stub-free symbol (not a tombstone, unification
/// loser, synthetic placeholder, or edge-construction stub).
///
/// Tombstoned (removed) nodes never reach this function because
/// [`GraphSnapshot`] iteration skips vacant arena slots. When the snapshot
/// carries genuine `is_definition` signal we additionally require the node to
/// be a real declaration; otherwise we fall back to the name-shape and
/// unification-loser checks so older snapshots (which lack the signal) still
/// rank their real symbols.
fn is_stub_free(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    if entry.is_unified_loser() {
        return false;
    }
    let Some(name) = snapshot.strings().resolve(entry.name) else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    if NodeEntry::is_synthetic_placeholder_name(&name) {
        return false;
    }
    if snapshot.definition_signal_present() && !entry.is_definition() {
        return false;
    }
    true
}

/// Whether a node is a real codebase symbol (stub-free), regardless of kind.
///
/// This is the kind-agnostic form of the hub eligibility test: it is used by
/// [`super::subsystems`] to size buckets and by [`super::community`] to pick
/// representatives. See [`is_stub_free`] for the exclusion rules.
#[must_use]
pub(crate) fn node_is_symbol(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    is_stub_free(snapshot, entry)
}

/// Whether an edge kind participates in degree centrality (`Calls` or
/// `References`).
fn is_degree_edge(kind: &crate::graph::unified::edge::EdgeKind) -> bool {
    use crate::graph::unified::edge::EdgeKind;
    matches!(kind, EdgeKind::Calls { .. } | EdgeKind::References)
}

/// Ranks the most load-bearing symbols in `snapshot` by degree centrality.
///
/// Deterministic: sorts by the chosen score descending, tie-breaking by
/// `(node kind, resolved name, node index)`. The whole path is integer, so the
/// result is byte-stable across runs and architectures.
///
/// See the [module docs](self) for the `&GraphSnapshot` (rather than
/// `&CompactionSnapshot`) parameter rationale.
#[must_use]
pub fn rank_hubs(snapshot: &GraphSnapshot, opts: &HubOpts) -> Vec<HubRank> {
    let node_slots = snapshot.nodes().slot_count();

    // Single fused degree pass: one walk of every live forward edge fills both
    // the fan-in (transpose) and fan-out (forward) arrays. Saturating adds keep
    // pathological multigraphs from overflowing u32.
    let mut fan_in = vec![0u32; node_slots];
    let mut fan_out = vec![0u32; node_slots];
    for edge in snapshot.edges().all_live_forward_edges() {
        if !is_degree_edge(&edge.kind) {
            continue;
        }
        let src = edge.source.index() as usize;
        let tgt = edge.target.index() as usize;
        if let Some(slot) = fan_out.get_mut(src) {
            *slot = slot.saturating_add(1);
        }
        if let Some(slot) = fan_in.get_mut(tgt) {
            *slot = slot.saturating_add(1);
        }
    }

    // Collect the eligible (kind-matching, stub-free) nodes with a pre-resolved
    // name string so the tie-break comparator stays allocation-free and stable.
    let mut ranked: Vec<(HubRank, String)> = Vec::new();
    for (id, entry) in snapshot.iter_nodes() {
        if !opts.kinds.contains(entry.kind) {
            continue;
        }
        if !is_stub_free(snapshot, entry) {
            continue;
        }
        let idx = id.index() as usize;
        let name = snapshot
            .strings()
            .resolve(entry.name)
            .map_or_else(String::new, |arc| arc.to_string());
        ranked.push((
            HubRank {
                node: id,
                name: entry.name,
                kind: entry.kind,
                fan_in: fan_in.get(idx).copied().unwrap_or(0),
                fan_out: fan_out.get(idx).copied().unwrap_or(0),
            },
            name,
        ));
    }

    let by = opts.by;
    ranked.sort_by(|(a, a_name), (b, b_name)| {
        b.score(by)
            .cmp(&a.score(by)) // score descending
            .then_with(|| a.kind.cmp(&b.kind)) // then kind ascending
            .then_with(|| a_name.cmp(b_name)) // then resolved name ascending
            .then_with(|| a.node.index().cmp(&b.node.index())) // then node index ascending
    });

    let mut hubs: Vec<HubRank> = ranked.into_iter().map(|(hub, _)| hub).collect();
    if opts.top != 0 && hubs.len() > opts.top {
        hubs.truncate(opts.top);
    }
    hubs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::PathBuf;

    use crate::graph::Language;
    use crate::graph::unified::string::StringId;

    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    fn references() -> EdgeKind {
        EdgeKind::References
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

    /// Adds a real (definition) symbol.
    fn add_symbol(graph: &mut CodeGraph, kind: NodeKind, name: &str, file: FileId) -> NodeId {
        let sid = graph.strings_mut().intern(name).expect("intern name");
        let entry = NodeEntry::new(kind, sid, file)
            .with_definition(true)
            .with_byte_range(0, 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph.indices_mut().add(id, kind, sid, Some(sid), file);
        id
    }

    /// Adds a non-definition stub symbol (default `is_definition = false`).
    fn add_stub(graph: &mut CodeGraph, kind: NodeKind, name: &str, file: FileId) -> NodeId {
        let sid = graph.strings_mut().intern(name).expect("intern name");
        let entry = NodeEntry::new(kind, sid, file).with_byte_range(0, 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph.indices_mut().add(id, kind, sid, Some(sid), file);
        id
    }

    fn edge(graph: &CodeGraph, from: NodeId, to: NodeId, kind: EdgeKind, file: FileId) {
        graph.edges().add_edge(from, to, kind, file);
    }

    fn find_by_name<'a>(
        hubs: &'a [HubRank],
        snapshot: &GraphSnapshot,
        name: &str,
    ) -> Option<&'a HubRank> {
        hubs.iter().find(|h| {
            snapshot
                .strings()
                .resolve(h.name)
                .is_some_and(|n| &*n == name)
        })
    }

    #[test]
    fn fan_in_and_fan_out_counts_are_correct() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let hub = add_symbol(&mut graph, NodeKind::Function, "hub", f);
        let a = add_symbol(&mut graph, NodeKind::Function, "a", f);
        let b = add_symbol(&mut graph, NodeKind::Function, "b", f);
        let c = add_symbol(&mut graph, NodeKind::Function, "c", f);
        let sink = add_symbol(&mut graph, NodeKind::Function, "sink", f);

        // hub fan_in = 3 (mix of Calls and References), fan_out = 2.
        edge(&graph, a, hub, calls(), f);
        edge(&graph, b, hub, references(), f);
        edge(&graph, c, hub, calls(), f);
        edge(&graph, hub, sink, calls(), f);
        edge(&graph, hub, a, calls(), f);
        // Imports must NOT count toward degree.
        edge(&graph, a, b, imports(), f);

        let snapshot = graph.snapshot();
        let hubs = rank_hubs(&snapshot, &HubOpts::default());

        let h = find_by_name(&hubs, &snapshot, "hub").expect("hub ranked");
        assert_eq!(h.fan_in, 3, "3 incoming Calls+References");
        assert_eq!(h.fan_out, 2, "2 outgoing Calls");

        let a_rank = find_by_name(&hubs, &snapshot, "a").expect("a ranked");
        // a: incoming = hub->a (1); the Imports a->b and a->hub do not count out.
        assert_eq!(a_rank.fan_in, 1);
        assert_eq!(a_rank.fan_out, 1, "a->hub Calls; a->b Imports excluded");
    }

    #[test]
    fn metric_selection_changes_the_top_hub() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let h = add_symbol(&mut graph, NodeKind::Function, "h_high_in", f);
        let o = add_symbol(&mut graph, NodeKind::Function, "o_orchestrator", f);
        let t = add_symbol(&mut graph, NodeKind::Function, "t_truehub", f);
        let s: Vec<NodeId> = (0..4)
            .map(|i| add_symbol(&mut graph, NodeKind::Function, &format!("s{i}"), f))
            .collect();
        let k: Vec<NodeId> = (0..4)
            .map(|i| add_symbol(&mut graph, NodeKind::Function, &format!("k{i}"), f))
            .collect();

        // H: fan_in 4, fan_out 1  -> combined 4
        for src in &s {
            edge(&graph, *src, h, calls(), f);
        }
        edge(&graph, h, k[0], calls(), f);
        // O: fan_in 1, fan_out 4  -> combined 4
        edge(&graph, s[0], o, calls(), f);
        for dst in &k {
            edge(&graph, o, *dst, calls(), f);
        }
        // T: fan_in 3, fan_out 3  -> combined 9
        for src in s.iter().take(3) {
            edge(&graph, *src, t, calls(), f);
        }
        for dst in k.iter().take(3) {
            edge(&graph, t, *dst, calls(), f);
        }

        let snapshot = graph.snapshot();

        let by_in = rank_hubs(
            &snapshot,
            &HubOpts {
                top: 0,
                by: HubMetric::FanIn,
                kinds: KindMask::default(),
            },
        );
        assert_eq!(
            snapshot.strings().resolve(by_in[0].name).as_deref(),
            Some("h_high_in"),
            "fan-in ranks the most depended-upon symbol first"
        );

        let by_out = rank_hubs(
            &snapshot,
            &HubOpts {
                top: 0,
                by: HubMetric::FanOut,
                kinds: KindMask::default(),
            },
        );
        assert_eq!(
            snapshot.strings().resolve(by_out[0].name).as_deref(),
            Some("o_orchestrator"),
            "fan-out ranks the broadest orchestrator first"
        );

        let by_combined = rank_hubs(
            &snapshot,
            &HubOpts {
                top: 0,
                by: HubMetric::Combined,
                kinds: KindMask::default(),
            },
        );
        assert_eq!(
            snapshot.strings().resolve(by_combined[0].name).as_deref(),
            Some("t_truehub"),
            "combined ranks the true hub first"
        );
    }

    #[test]
    fn tie_break_is_stable_by_name_then_index() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        // Two functions with identical (zero) degree; expect name-ascending order.
        let beta = add_symbol(&mut graph, NodeKind::Function, "beta", f);
        let alpha = add_symbol(&mut graph, NodeKind::Function, "alpha", f);
        let _ = (beta, alpha);

        let snapshot = graph.snapshot();
        let hubs = rank_hubs(&snapshot, &HubOpts::default());
        let names: Vec<String> = hubs
            .iter()
            .map(|h| snapshot.strings().resolve(h.name).unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "beta"]);

        // Determinism across repeated runs.
        for _ in 0..5 {
            assert_eq!(rank_hubs(&snapshot, &HubOpts::default()), hubs);
        }
    }

    #[test]
    fn tie_break_orders_by_kind_before_name() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        // Function "z" vs Class "a": equal score, kind decides (Function < Class).
        let _class_a = add_symbol(&mut graph, NodeKind::Class, "a_class", f);
        let _fn_z = add_symbol(&mut graph, NodeKind::Function, "z_fn", f);

        let snapshot = graph.snapshot();
        let hubs = rank_hubs(&snapshot, &HubOpts::default());
        assert_eq!(hubs[0].kind, NodeKind::Function);
        assert_eq!(
            snapshot.strings().resolve(hubs[0].name).as_deref(),
            Some("z_fn"),
            "kind ordering beats name ordering"
        );
    }

    #[test]
    fn only_target_kinds_are_ranked() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let func = add_symbol(&mut graph, NodeKind::Function, "the_fn", f);
        let var = add_symbol(&mut graph, NodeKind::Variable, "the_var", f);
        let konst = add_symbol(&mut graph, NodeKind::Constant, "the_const", f);
        // Give the non-target kinds high fan-in so exclusion is meaningful.
        edge(&graph, func, var, calls(), f);
        edge(&graph, func, konst, calls(), f);

        let snapshot = graph.snapshot();
        let hubs = rank_hubs(&snapshot, &HubOpts::default());
        for h in &hubs {
            assert!(
                matches!(
                    h.kind,
                    NodeKind::Function
                        | NodeKind::Method
                        | NodeKind::Type
                        | NodeKind::Class
                        | NodeKind::Trait
                ),
                "unexpected kind {:?} in ranking",
                h.kind
            );
        }
        assert!(find_by_name(&hubs, &snapshot, "the_var").is_none());
        assert!(find_by_name(&hubs, &snapshot, "the_const").is_none());
    }

    #[test]
    fn tombstoned_and_stub_nodes_are_excluded() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let real = add_symbol(&mut graph, NodeKind::Function, "real_fn", f);

        // is_definition = false stub (signal is present on a fresh CodeGraph).
        let stub = add_stub(&mut graph, NodeKind::Function, "stub_fn", f);
        // synthetic-shape placeholder name.
        let synthetic = add_symbol(&mut graph, NodeKind::Function, "<closure>", f);
        // unification loser: name == StringId::INVALID.
        let loser_entry = NodeEntry::new(NodeKind::Function, StringId::INVALID, f)
            .with_definition(true)
            .with_byte_range(0, 1);
        let loser = graph.nodes_mut().alloc(loser_entry).expect("alloc loser");

        // Give every excluded node a huge fan-in so, were it ranked, it would top.
        for src_name in ["s0", "s1", "s2", "s3", "s4"] {
            let s = add_symbol(&mut graph, NodeKind::Function, src_name, f);
            edge(&graph, s, stub, calls(), f);
            edge(&graph, s, synthetic, calls(), f);
            edge(&graph, s, loser, calls(), f);
            edge(&graph, s, real, calls(), f);
        }

        let snapshot = graph.snapshot();
        assert!(snapshot.definition_signal_present());
        let hubs = rank_hubs(
            &snapshot,
            &HubOpts {
                top: 0,
                by: HubMetric::FanIn,
                kinds: KindMask::default(),
            },
        );

        assert!(find_by_name(&hubs, &snapshot, "real_fn").is_some());
        assert!(find_by_name(&hubs, &snapshot, "stub_fn").is_none());
        assert!(find_by_name(&hubs, &snapshot, "<closure>").is_none());
        // The unification loser has no resolvable name, so it cannot appear.
        assert!(
            hubs.iter()
                .all(|h| snapshot.strings().resolve(h.name).is_some())
        );
    }

    #[test]
    fn top_bounds_the_result() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        for i in 0..10 {
            add_symbol(&mut graph, NodeKind::Function, &format!("fn{i}"), f);
        }
        let snapshot = graph.snapshot();
        let hubs = rank_hubs(
            &snapshot,
            &HubOpts {
                top: 3,
                by: HubMetric::FanIn,
                kinds: KindMask::default(),
            },
        );
        assert_eq!(hubs.len(), 3);
    }

    #[test]
    fn kind_mask_membership() {
        let mask = KindMask::default();
        assert!(mask.contains(NodeKind::Function));
        assert!(mask.contains(NodeKind::Trait));
        assert!(!mask.contains(NodeKind::Variable));
        assert!(KindMask::empty().is_empty());
        let custom = KindMask::from_kinds(&[NodeKind::Enum, NodeKind::Struct]);
        assert!(custom.contains(NodeKind::Enum));
        assert!(custom.contains(NodeKind::Struct));
        assert!(!custom.contains(NodeKind::Function));
    }
}
