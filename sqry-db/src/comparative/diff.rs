//! Semantic diff implementation for [`super::ComparativeQueryDb`].
//!
//! Ported from `sqry-mcp::execution::diff_comparator` (which in turn adapted
//! `sqry-core::graph::diff::GraphComparator`) as part of Phase 3C / DB20. The
//! MCP copy diverged from sqry-core's in three ways, all of which are
//! preserved here so the MCP wire format is byte-for-byte stable:
//!
//! 1. Qualified names are formatted through
//!    [`sqry_core::graph::unified::resolution::display_graph_qualified_name`]
//!    when a known language is associated with the source file. This lets
//!    per-language plugins override the stored qualified name (e.g. Swift
//!    inserting `Type.` for `is_static` members).
//! 2. The `is_static` flag is threaded through the comparator so the above
//!    display logic receives the same information the graph node stored.
//! 3. Line numbers / columns / paths are reported as plain fields on
//!    [`NodeLocation`]; the MCP handler wraps them in its transport DTO
//!    (`NodeRefData` + `fileUri`) so the wire format is owned by MCP.
//!
//! Rename detection heuristics (Levenshtein over signatures weighted 70%,
//! location proximity weighted 30%, 90% confidence threshold) match the
//! previous MCP `GraphComparator` bit-for-bit.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::{ChannelPeerDirection, InferenceKind};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::resolution::display_graph_qualified_name;

// ============================================================================
// Rename heuristic constants (frozen; match the pre-DB20 MCP comparator)
// ============================================================================

const SIGNATURE_WEIGHT: f64 = 0.7;
const LOCATION_WEIGHT: f64 = 0.3;
const SIGNATURE_MIN_SCORE: f64 = 0.7;
const RENAME_CONFIDENCE_THRESHOLD: f64 = 0.9;
const SAME_FILE_LINE_WINDOW: i32 = 50;
const SAME_FILE_LINE_NORMALIZER: f64 = 100.0;
const SAME_FILE_MAX_PENALTY: f64 = 0.5;
const SAME_FILE_FAR_SCORE: f64 = 0.3;
const CROSS_FILE_LOCATION_SCORE: f64 = 0.7;

// ============================================================================
// Public output types
// ============================================================================

/// Kind of change detected between the two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeType {
    /// Node is present in the new snapshot but not the old.
    Added,
    /// Node is present in the old snapshot but not the new.
    Removed,
    /// Node exists on both sides but its body / location changed while the
    /// signature stayed identical.
    Modified,
    /// Node was matched heuristically as a renamed version of a node that
    /// disappeared (>= 90% confidence via signature + location scoring).
    Renamed,
    /// Node exists on both sides and its signature changed.
    SignatureChanged,
    /// Node exists unchanged on both sides. Callers opt into emitting these
    /// via their own filter logic — [`compute_diff`] never emits this variant
    /// directly.
    Unchanged,
}

impl ChangeType {
    /// Returns the stable wire-format string for this change type. The
    /// mapping matches the pre-DB20 MCP and CLI outputs exactly.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Added => "added",
            ChangeType::Removed => "removed",
            ChangeType::Modified => "modified",
            ChangeType::Renamed => "renamed",
            ChangeType::SignatureChanged => "signature_changed",
            ChangeType::Unchanged => "unchanged",
        }
    }
}

/// Location of a single changed node, reported in sqry-db-owned terms.
///
/// `file_path` is either absolute (when the caller supplied worktree roots
/// via [`DiffOptions`]) or workspace-relative (when no worktree root was
/// supplied — tests, CLI callers). Lines are 1-indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLocation {
    /// File path; prefixed with the matching worktree root if the caller
    /// supplied one, otherwise the path stored in the graph.
    pub file_path: PathBuf,
    /// Language id reported by the graph's file registry (e.g. `"rust"`,
    /// `"python"`). `"unknown"` if the registry had no mapping.
    pub language: String,
    /// Start line (1-indexed, as stored in the graph node arena).
    pub start_line: u32,
    /// End line (1-indexed).
    pub end_line: u32,
    /// Start column (0-indexed).
    pub start_column: u32,
    /// End column (0-indexed).
    pub end_column: u32,
}

/// A single change record.
#[derive(Debug, Clone)]
pub struct NodeChange {
    /// Short symbol name (e.g. `foo`).
    pub symbol_name: String,
    /// Display-form qualified name, run through
    /// [`display_graph_qualified_name`] when the language is known.
    pub qualified_name: String,
    /// Lowercase node kind string (`"function"`, `"method"`, …). Matches the
    /// pre-DB20 MCP string taxonomy used by `filters.symbol_kinds`.
    pub kind: String,
    /// What kind of change was detected.
    pub change_type: ChangeType,
    /// Location in the "old" snapshot (populated for `Removed`, `Modified`,
    /// `Renamed`, `SignatureChanged`).
    pub base_location: Option<NodeLocation>,
    /// Location in the "new" snapshot (populated for `Added`, `Modified`,
    /// `Renamed`, `SignatureChanged`).
    pub target_location: Option<NodeLocation>,
    /// Signature string in the "old" snapshot, if any.
    pub signature_before: Option<String>,
    /// Signature string in the "new" snapshot, if any.
    pub signature_after: Option<String>,
}

/// Summary counts for a diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSummary {
    /// Count of [`ChangeType::Added`] records.
    pub added: u64,
    /// Count of [`ChangeType::Removed`] records.
    pub removed: u64,
    /// Count of [`ChangeType::Modified`] records.
    pub modified: u64,
    /// Count of [`ChangeType::Renamed`] records.
    pub renamed: u64,
    /// Count of [`ChangeType::SignatureChanged`] records.
    pub signature_changed: u64,
    /// Count of [`ChangeType::Unchanged`] records (set by callers that
    /// post-process the diff to include unchanged nodes).
    pub unchanged: u64,
}

impl DiffSummary {
    /// Recomputes a summary from an existing slice of changes. Useful for
    /// callers that filter `compute_diff`'s output before rendering.
    #[must_use]
    pub fn from_changes(changes: &[NodeChange]) -> Self {
        let mut summary = Self::default();
        for change in changes {
            match change.change_type {
                ChangeType::Added => summary.added += 1,
                ChangeType::Removed => summary.removed += 1,
                ChangeType::Modified => summary.modified += 1,
                ChangeType::Renamed => summary.renamed += 1,
                ChangeType::SignatureChanged => summary.signature_changed += 1,
                ChangeType::Unchanged => summary.unchanged += 1,
            }
        }
        summary
    }
}

/// Output of [`compute_diff`] / [`super::ComparativeQueryDb::diff`].
#[derive(Debug, Clone, Default)]
pub struct DiffOutput {
    /// All detected changes, in the order the underlying `HashMap` iteration
    /// produces (caller-visible ordering should sort/paginate as needed).
    pub changes: Vec<NodeChange>,
    /// Pre-filter summary (matches `changes` bucket counts before any
    /// caller-side filter is applied).
    pub summary: DiffSummary,
    /// `ChannelPeer` edges present in the new snapshot but not the old.
    pub channel_peer_edges_added: Vec<EdgeDelta>,
    /// `ChannelPeer` edges present in the old snapshot but not the new.
    pub channel_peer_edges_removed: Vec<EdgeDelta>,
    /// `Instantiates` edges present in the new snapshot but not the old.
    pub instantiates_edges_added: Vec<EdgeDelta>,
    /// `Instantiates` edges present in the old snapshot but not the new.
    pub instantiates_edges_removed: Vec<EdgeDelta>,
}

// ============================================================================
// Edge-delta axis (T2 channel / generic edges — `02_DESIGN.md` §7.6)
//
// The node-axis comparator above is blind to the two edge kinds this feature
// introduces. A `Map[string, int]` → `Map[string, int64]` swap or an added send
// site is a behavioural change a user-facing diff must surface, so the edge axis
// compares `ChannelPeer` and `Instantiates` edges between the two snapshots.
//
// The diff key is `(source_qn, target_qn, DiffEdgeKey)`, where `DiffEdgeKey` is
// a COMPARATOR-LOCAL projection deliberately distinct from the planner's
// `normalize_edge_kind`: it PRESERVES the `Instantiates` type-argument vector
// (so type-argument changes are visible) and the `ChannelPeer` direction, while
// dropping the per-channel-node `buffer_kind` cache. Type-arg `StringId`s are
// resolved to owned strings during construction, because the two snapshots do
// not share a string table.
// ============================================================================

/// A resolved generic type argument in a diff key (snapshot-independent).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffTypeArg {
    /// Resolved type name, or the `"<unknown>"` sentinel.
    pub name: String,
    /// Set when filled by Go's untyped-constant default rule.
    pub default_typed: bool,
}

/// Comparator-local normalized edge key for the two feature edge kinds.
///
/// PRESERVES every byte that contributes to behavioural meaning at the site;
/// see the module-level rationale above and `02_DESIGN.md` §7.6.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffEdgeKey {
    /// A channel send / receive / close peer edge, keyed on direction only.
    ChannelPeer {
        /// `"send"` / `"receive"` / `"close"`.
        direction: String,
    },
    /// A generic instantiation edge, keyed on inference kind AND the full
    /// resolved type-argument vector.
    Instantiates {
        /// `"explicit"` / `"inferred"` / `"partial"` / `"unknown"`.
        inference_kind: String,
        /// Resolved type-name slots in declaration order.
        type_args: Vec<DiffTypeArg>,
    },
}

/// One edge difference between the two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeDelta {
    /// Display-form qualified name of the edge source node.
    pub source_qn: String,
    /// Display-form qualified name of the edge target node.
    pub target_qn: String,
    /// The comparator-local normalized kind (diff key payload).
    pub kind: DiffEdgeKey,
}

/// Options controlling the diff computation.
///
/// All fields default to empty; callers that need worktree translation
/// populate `old_worktree_path` / `new_worktree_path` with the absolute root
/// of each git worktree. See `sqry_mcp::execution::git_worktree::WorktreeManager`
/// for an example producer.
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// Absolute path of the worktree backing the "old" snapshot. When
    /// non-empty, every resolved per-node file path is joined onto this
    /// root so downstream callers can turn it into a `file://` URI.
    pub old_worktree_path: PathBuf,
    /// Absolute path of the worktree backing the "new" snapshot.
    pub new_worktree_path: PathBuf,
}

// ============================================================================
// Implementation
// ============================================================================

/// Internal snapshot of a single node used during comparison.
#[derive(Clone)]
struct NodeSnap {
    name: String,
    qualified_name: String,
    kind: NodeKind,
    kind_str: String,
    is_static: bool,
    signature: Option<String>,
    file_path: PathBuf,
    language: String,
    start_line: u32,
    end_line: u32,
    start_column: u32,
    end_column: u32,
}

impl NodeSnap {
    fn display_qualified_name(&self) -> String {
        Language::from_id(&self.language).map_or_else(
            || self.qualified_name.clone(),
            |language| {
                display_graph_qualified_name(
                    language,
                    &self.qualified_name,
                    self.kind,
                    self.is_static,
                )
            },
        )
    }

    fn into_location(self) -> NodeLocation {
        NodeLocation {
            file_path: self.file_path,
            language: self.language,
            start_line: self.start_line,
            end_line: self.end_line,
            start_column: self.start_column,
            end_column: self.end_column,
        }
    }

    fn to_location(&self) -> NodeLocation {
        NodeLocation {
            file_path: self.file_path.clone(),
            language: self.language.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            start_column: self.start_column,
            end_column: self.end_column,
        }
    }
}

/// Entry point: computes the semantic diff between `old` and `new`.
///
/// The algorithm is the same one the pre-DB20 MCP `GraphComparator` used:
///
/// 1. Build two `qualified_name -> NodeSnap` maps, skipping nodes without
///    qualified names (call sites, imports, etc.).
/// 2. Walk the "new" map: nodes absent from "old" are added candidates;
///    nodes present on both sides are checked for signature / body changes
///    (producing `SignatureChanged` or `Modified`).
/// 3. Walk the "old" map: nodes absent from "new" are removed candidates.
/// 4. Run heuristic rename detection between the remaining removed and
///    added sets (same kind, signature similarity >= 0.7, location scoring,
///    confidence threshold 0.9). Matched pairs emit `Renamed` records and
///    are removed from the added/removed pools.
/// 5. Emit the remaining `Added` and `Removed` records.
///
/// Output is pre-filter; callers apply their own `include_unchanged` /
/// `change_types` / `symbol_kinds` filters.
#[must_use]
pub fn compute_diff(old: &GraphSnapshot, new: &GraphSnapshot, opts: &DiffOptions) -> DiffOutput {
    let base_map = build_node_map(old, &opts.old_worktree_path);
    let target_map = build_node_map(new, &opts.new_worktree_path);

    let (added_nodes, modified_changes) = collect_added_and_modified(&base_map, &target_map, opts);
    let removed_nodes = collect_removed_nodes(&base_map, &target_map);

    let mut changes = modified_changes;

    let (rename_changes, renamed_qnames) = collect_renames(&removed_nodes, &added_nodes, opts);
    changes.extend(rename_changes);

    append_removed_changes(&mut changes, &removed_nodes, &renamed_qnames);
    append_added_changes(&mut changes, &added_nodes, &renamed_qnames);

    let summary = DiffSummary::from_changes(&changes);
    let edges = compute_edge_deltas(old, new);
    DiffOutput {
        changes,
        summary,
        channel_peer_edges_added: edges.channel_peer_added,
        channel_peer_edges_removed: edges.channel_peer_removed,
        instantiates_edges_added: edges.instantiates_added,
        instantiates_edges_removed: edges.instantiates_removed,
    }
}

/// Comparator-local normalization for the two feature edge kinds. Returns
/// `None` for every other edge kind (the edge axis only compares these two).
fn diff_normalized_kind(kind: &EdgeKind, snapshot: &GraphSnapshot) -> Option<DiffEdgeKey> {
    match kind {
        EdgeKind::ChannelPeer { direction, .. } => Some(DiffEdgeKey::ChannelPeer {
            direction: channel_direction_str(*direction).to_string(),
        }),
        EdgeKind::Instantiates {
            type_args,
            inference_kind,
        } => {
            let resolved = type_args
                .iter()
                .map(|ta| DiffTypeArg {
                    name: snapshot
                        .strings()
                        .resolve(ta.name)
                        .map_or_else(String::new, |a| a.to_string()),
                    default_typed: ta.default_typed,
                })
                .collect();
            Some(DiffEdgeKey::Instantiates {
                inference_kind: inference_kind_str(*inference_kind).to_string(),
                type_args: resolved,
            })
        }
        _ => None,
    }
}

fn channel_direction_str(direction: ChannelPeerDirection) -> &'static str {
    match direction {
        ChannelPeerDirection::Send => "send",
        ChannelPeerDirection::Receive => "receive",
        ChannelPeerDirection::Close => "close",
    }
}

fn inference_kind_str(kind: InferenceKind) -> &'static str {
    match kind {
        InferenceKind::Explicit => "explicit",
        InferenceKind::Inferred => "inferred",
        InferenceKind::Partial => "partial",
        InferenceKind::Unknown => "unknown",
    }
}

/// Resolve a node id to its (raw, canonical) qualified name string.
fn node_qn(snapshot: &GraphSnapshot, id: NodeId) -> Option<String> {
    let entry = snapshot.get_node(id)?;
    let sid = entry.qualified_name.unwrap_or(entry.name);
    snapshot.strings().resolve(sid).map(|a| a.to_string())
}

/// `(source_qn, target_qn, kind)` multiset for the two feature edge kinds.
type EdgeKeyTuple = (String, String, DiffEdgeKey);

fn collect_edge_keys(snapshot: &GraphSnapshot) -> HashMap<EdgeKeyTuple, usize> {
    let mut map: HashMap<EdgeKeyTuple, usize> = HashMap::new();
    for (source, target, kind) in snapshot.iter_edges() {
        let Some(diff_kind) = diff_normalized_kind(&kind, snapshot) else {
            continue;
        };
        let (Some(source_qn), Some(target_qn)) =
            (node_qn(snapshot, source), node_qn(snapshot, target))
        else {
            continue;
        };
        *map.entry((source_qn, target_qn, diff_kind)).or_insert(0) += 1;
    }
    map
}

/// The four edge-delta vectors produced by [`compute_edge_deltas`].
struct EdgeDeltas {
    channel_peer_added: Vec<EdgeDelta>,
    channel_peer_removed: Vec<EdgeDelta>,
    instantiates_added: Vec<EdgeDelta>,
    instantiates_removed: Vec<EdgeDelta>,
}

/// Compare the `ChannelPeer` / `Instantiates` edge multisets between the two
/// snapshots. An edge is "added" when the new snapshot holds more copies of a
/// given `(source_qn, target_qn, DiffEdgeKey)` key than the old, and vice
/// versa for "removed".
fn compute_edge_deltas(old: &GraphSnapshot, new: &GraphSnapshot) -> EdgeDeltas {
    let old_keys = collect_edge_keys(old);
    let new_keys = collect_edge_keys(new);

    let mut deltas = EdgeDeltas {
        channel_peer_added: Vec::new(),
        channel_peer_removed: Vec::new(),
        instantiates_added: Vec::new(),
        instantiates_removed: Vec::new(),
    };

    let push = |key: &EdgeKeyTuple, added: bool, deltas: &mut EdgeDeltas| {
        let delta = EdgeDelta {
            source_qn: key.0.clone(),
            target_qn: key.1.clone(),
            kind: key.2.clone(),
        };
        match (&key.2, added) {
            (DiffEdgeKey::ChannelPeer { .. }, true) => deltas.channel_peer_added.push(delta),
            (DiffEdgeKey::ChannelPeer { .. }, false) => deltas.channel_peer_removed.push(delta),
            (DiffEdgeKey::Instantiates { .. }, true) => deltas.instantiates_added.push(delta),
            (DiffEdgeKey::Instantiates { .. }, false) => deltas.instantiates_removed.push(delta),
        }
    };

    for (key, &new_count) in &new_keys {
        let old_count = old_keys.get(key).copied().unwrap_or(0);
        for _ in old_count..new_count {
            push(key, true, &mut deltas);
        }
    }
    for (key, &old_count) in &old_keys {
        let new_count = new_keys.get(key).copied().unwrap_or(0);
        for _ in new_count..old_count {
            push(key, false, &mut deltas);
        }
    }

    deltas
}

/// Builds a `qualified_name -> NodeSnap` map from a snapshot, joining each
/// node's stored file path onto `worktree_path` when the latter is set.
fn build_node_map(snapshot: &GraphSnapshot, worktree_path: &Path) -> HashMap<String, NodeSnap> {
    let strings = snapshot.strings();
    let files = snapshot.files();
    let mut map = HashMap::new();

    for (_node_id, entry) in snapshot.iter_nodes() {
        // Gate 0d iter-2 fix: skip unified losers from
        // `semantic_diff` node map. Losers have no name /
        // qualified_name / signature post-merge; the explicit guard
        // makes that contract visible to readers.
        // See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();

        let qualified_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| name.clone(), |s| s.to_string());

        // Skip nodes without qualified names (call sites, imports, etc.).
        if qualified_name.is_empty() {
            continue;
        }

        let signature = entry
            .signature
            .and_then(|sid| strings.resolve(sid))
            .map(|s| s.to_string());

        let file_path = files
            .resolve(entry.file)
            .map(|p| {
                if worktree_path.as_os_str().is_empty() {
                    PathBuf::from(p.as_ref())
                } else {
                    worktree_path.join(p.as_ref())
                }
            })
            .unwrap_or_default();

        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "unknown".to_string(), |l| l.to_string());

        let snap = NodeSnap {
            name,
            qualified_name: qualified_name.clone(),
            kind: entry.kind,
            kind_str: node_kind_to_string(entry.kind),
            is_static: entry.is_static,
            signature,
            file_path,
            language,
            start_line: entry.start_line,
            end_line: entry.end_line,
            start_column: entry.start_column,
            end_column: entry.end_column,
        };

        map.insert(qualified_name, snap);
    }

    map
}

fn collect_added_and_modified(
    base_map: &HashMap<String, NodeSnap>,
    target_map: &HashMap<String, NodeSnap>,
    opts: &DiffOptions,
) -> (Vec<NodeSnap>, Vec<NodeChange>) {
    let mut added = Vec::new();
    let mut changes = Vec::new();

    for (qname, target_snap) in target_map {
        match base_map.get(qname) {
            None => added.push(target_snap.clone()),
            Some(base_snap) => {
                if let Some(change) = detect_modification(base_snap, target_snap, opts) {
                    changes.push(change);
                }
            }
        }
    }

    (added, changes)
}

fn collect_removed_nodes(
    base_map: &HashMap<String, NodeSnap>,
    target_map: &HashMap<String, NodeSnap>,
) -> Vec<NodeSnap> {
    base_map
        .iter()
        .filter(|(qname, _)| !target_map.contains_key(*qname))
        .map(|(_, snap)| snap.clone())
        .collect()
}

fn detect_modification(
    base_snap: &NodeSnap,
    target_snap: &NodeSnap,
    opts: &DiffOptions,
) -> Option<NodeChange> {
    let signature_changed = base_snap.signature != target_snap.signature;

    // Normalise file paths so "same path in different worktrees" does not
    // register as a body change.
    let base_rel = strip_worktree_prefix(&base_snap.file_path, opts);
    let target_rel = strip_worktree_prefix(&target_snap.file_path, opts);

    let body_changed = base_snap.start_line != target_snap.start_line
        || base_snap.end_line != target_snap.end_line
        || base_rel != target_rel;

    if signature_changed {
        Some(NodeChange {
            symbol_name: target_snap.name.clone(),
            qualified_name: target_snap.display_qualified_name(),
            kind: target_snap.kind_str.clone(),
            change_type: ChangeType::SignatureChanged,
            base_location: Some(base_snap.to_location()),
            target_location: Some(target_snap.to_location()),
            signature_before: base_snap.signature.clone(),
            signature_after: target_snap.signature.clone(),
        })
    } else if body_changed {
        Some(NodeChange {
            symbol_name: target_snap.name.clone(),
            qualified_name: target_snap.display_qualified_name(),
            kind: target_snap.kind_str.clone(),
            change_type: ChangeType::Modified,
            base_location: Some(base_snap.to_location()),
            target_location: Some(target_snap.to_location()),
            signature_before: base_snap.signature.clone(),
            signature_after: target_snap.signature.clone(),
        })
    } else {
        None
    }
}

fn collect_renames(
    removed: &[NodeSnap],
    added: &[NodeSnap],
    opts: &DiffOptions,
) -> (Vec<NodeChange>, HashSet<String>) {
    let renames = detect_renames(removed, added, opts);
    let mut rename_changes = Vec::new();
    let mut renamed_qnames = HashSet::new();

    for (base_snap, target_snap) in &renames {
        renamed_qnames.insert(base_snap.qualified_name.clone());
        renamed_qnames.insert(target_snap.qualified_name.clone());
        rename_changes.push(create_renamed_change(base_snap, target_snap));
    }

    (rename_changes, renamed_qnames)
}

fn detect_renames(
    removed: &[NodeSnap],
    added: &[NodeSnap],
    opts: &DiffOptions,
) -> Vec<(NodeSnap, NodeSnap)> {
    let mut renames = Vec::new();
    let mut matched_added: HashSet<usize> = HashSet::new();

    for removed_snap in removed {
        let mut best_match: Option<(usize, f64)> = None;

        for (idx, added_snap) in added.iter().enumerate() {
            if matched_added.contains(&idx) {
                continue;
            }
            let Some(score) = is_likely_rename(removed_snap, added_snap, opts) else {
                continue;
            };
            let is_better = match best_match {
                Some((_, best_score)) => score > best_score,
                None => true,
            };
            if is_better {
                best_match = Some((idx, score));
            }
        }

        if let Some((idx, score)) = best_match
            && score >= RENAME_CONFIDENCE_THRESHOLD
        {
            matched_added.insert(idx);
            renames.push((removed_snap.clone(), added[idx].clone()));
        }
    }

    renames
}

fn is_likely_rename(base: &NodeSnap, target: &NodeSnap, opts: &DiffOptions) -> Option<f64> {
    // Criterion 1: same node kind.
    if base.kind != target.kind {
        return None;
    }

    // Criterion 2: signature similarity (70% weight).
    let sig_score = match (&base.signature, &target.signature) {
        (Some(base_sig), Some(target_sig)) => {
            if base_sig == target_sig {
                1.0
            } else {
                levenshtein_similarity(base_sig, target_sig)
            }
        }
        (None, None) => 1.0,
        _ => return None,
    };
    if sig_score < SIGNATURE_MIN_SCORE {
        return None;
    }
    let mut confidence = sig_score * SIGNATURE_WEIGHT;

    // Criterion 3: location proximity (30% weight).
    let base_rel = strip_worktree_prefix(&base.file_path, opts);
    let target_rel = strip_worktree_prefix(&target.file_path, opts);
    let location_score = if base_rel == target_rel {
        let base_line: i32 = base.start_line.try_into().unwrap_or(i32::MAX);
        let target_line: i32 = target.start_line.try_into().unwrap_or(i32::MAX);
        let line_diff = (base_line - target_line).abs();
        if line_diff <= SAME_FILE_LINE_WINDOW {
            1.0 - (f64::from(line_diff) / SAME_FILE_LINE_NORMALIZER).min(SAME_FILE_MAX_PENALTY)
        } else {
            SAME_FILE_FAR_SCORE
        }
    } else {
        CROSS_FILE_LOCATION_SCORE
    };
    confidence += location_score * LOCATION_WEIGHT;

    Some(confidence)
}

fn create_renamed_change(base: &NodeSnap, target: &NodeSnap) -> NodeChange {
    NodeChange {
        symbol_name: target.name.clone(),
        qualified_name: target.display_qualified_name(),
        kind: target.kind_str.clone(),
        change_type: ChangeType::Renamed,
        base_location: Some(base.to_location()),
        target_location: Some(target.to_location()),
        signature_before: base.signature.clone(),
        signature_after: target.signature.clone(),
    }
}

fn append_removed_changes(
    changes: &mut Vec<NodeChange>,
    removed: &[NodeSnap],
    renamed_qnames: &HashSet<String>,
) {
    for snap in removed {
        if !renamed_qnames.contains(&snap.qualified_name) {
            changes.push(NodeChange {
                symbol_name: snap.name.clone(),
                qualified_name: snap.display_qualified_name(),
                kind: snap.kind_str.clone(),
                change_type: ChangeType::Removed,
                base_location: Some(snap.clone().into_location()),
                target_location: None,
                signature_before: snap.signature.clone(),
                signature_after: None,
            });
        }
    }
}

fn append_added_changes(
    changes: &mut Vec<NodeChange>,
    added: &[NodeSnap],
    renamed_qnames: &HashSet<String>,
) {
    for snap in added {
        if !renamed_qnames.contains(&snap.qualified_name) {
            changes.push(NodeChange {
                symbol_name: snap.name.clone(),
                qualified_name: snap.display_qualified_name(),
                kind: snap.kind_str.clone(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: Some(snap.clone().into_location()),
                signature_before: None,
                signature_after: snap.signature.clone(),
            });
        }
    }
}

/// Strips whichever of the two worktree prefixes from `path` matches.
fn strip_worktree_prefix(path: &Path, opts: &DiffOptions) -> PathBuf {
    if !opts.old_worktree_path.as_os_str().is_empty()
        && let Ok(relative) = path.strip_prefix(&opts.old_worktree_path)
    {
        return relative.to_path_buf();
    }
    if !opts.new_worktree_path.as_os_str().is_empty()
        && let Ok(relative) = path.strip_prefix(&opts.new_worktree_path)
    {
        return relative.to_path_buf();
    }
    path.to_path_buf()
}

/// Normalised Levenshtein similarity in `[0.0, 1.0]`.
fn levenshtein_similarity(a: &str, b: &str) -> f64 {
    let distance = strsim::levenshtein(a, b);
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    let distance = f64::from(u32::try_from(distance).unwrap_or(u32::MAX));
    let max_len = f64::from(u32::try_from(max_len).unwrap_or(u32::MAX));
    1.0 - (distance / max_len)
}

/// Lowercase node-kind strings matching the pre-DB20 MCP taxonomy. Any kind
/// not explicitly listed collapses to `"other"` — this matches the legacy
/// behaviour so `filters.symbol_kinds` keeps its existing acceptance set.
fn node_kind_to_string(kind: NodeKind) -> String {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        _ => "other",
    }
    .to_string()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_similarity_bounds() {
        assert!((levenshtein_similarity("hello", "hello") - 1.0).abs() < 1e-10);
        assert!((levenshtein_similarity("", "") - 1.0).abs() < 1e-10);
        assert!(levenshtein_similarity("hello", "hallo") > 0.7);
        assert!(levenshtein_similarity("hello", "world") < 0.5);
    }

    #[test]
    fn diff_normalized_kind_drops_channel_buffer_but_keeps_direction() {
        use sqry_core::graph::unified::concurrent::CodeGraph;
        use sqry_core::graph::unified::edge::kind::ChannelBufferKind;

        let snap = CodeGraph::new().snapshot();
        let send_unbuffered = EdgeKind::ChannelPeer {
            direction: ChannelPeerDirection::Send,
            buffer_kind: ChannelBufferKind::Unbuffered,
        };
        let send_buffered = EdgeKind::ChannelPeer {
            direction: ChannelPeerDirection::Send,
            buffer_kind: ChannelBufferKind::Buffered,
        };
        let receive = EdgeKind::ChannelPeer {
            direction: ChannelPeerDirection::Receive,
            buffer_kind: ChannelBufferKind::Unbuffered,
        };

        // buffer_kind is a per-channel-node cache, not behaviourally
        // significant at the operation site: a buffer-only change must NOT
        // surface as a diff.
        assert_eq!(
            diff_normalized_kind(&send_unbuffered, &snap),
            diff_normalized_kind(&send_buffered, &snap),
        );
        // direction IS the semantic discriminator.
        assert_ne!(
            diff_normalized_kind(&send_unbuffered, &snap),
            diff_normalized_kind(&receive, &snap),
        );
    }

    #[test]
    fn diff_normalized_kind_ignores_non_feature_edges() {
        use sqry_core::graph::unified::concurrent::CodeGraph;

        let snap = CodeGraph::new().snapshot();
        assert!(diff_normalized_kind(&EdgeKind::Contains, &snap).is_none());
        assert!(diff_normalized_kind(&EdgeKind::Defines, &snap).is_none());
    }

    #[test]
    fn change_type_wire_strings_match_pre_db20() {
        assert_eq!(ChangeType::Added.as_str(), "added");
        assert_eq!(ChangeType::Removed.as_str(), "removed");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
        assert_eq!(ChangeType::Renamed.as_str(), "renamed");
        assert_eq!(ChangeType::SignatureChanged.as_str(), "signature_changed");
        assert_eq!(ChangeType::Unchanged.as_str(), "unchanged");
    }

    #[test]
    fn diff_summary_from_changes_tallies_each_bucket() {
        let changes = vec![
            NodeChange {
                symbol_name: "a".into(),
                qualified_name: "a".into(),
                kind: "function".into(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                symbol_name: "b".into(),
                qualified_name: "b".into(),
                kind: "function".into(),
                change_type: ChangeType::Removed,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                symbol_name: "c".into(),
                qualified_name: "c".into(),
                kind: "function".into(),
                change_type: ChangeType::SignatureChanged,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];
        let summary = DiffSummary::from_changes(&changes);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.signature_changed, 1);
        assert_eq!(summary.modified, 0);
        assert_eq!(summary.renamed, 0);
        assert_eq!(summary.unchanged, 0);
    }

    #[test]
    fn empty_snapshots_produce_empty_diff() {
        use std::sync::Arc;

        use sqry_core::graph::unified::concurrent::CodeGraph;

        let old = Arc::new(CodeGraph::new().snapshot());
        let new = Arc::new(CodeGraph::new().snapshot());

        let cmp = super::super::ComparativeQueryDb::new(old, new);
        let out = cmp.diff_default();
        assert!(out.changes.is_empty());
        assert_eq!(out.summary, DiffSummary::default());
    }

    #[test]
    fn strip_worktree_prefix_falls_back_when_empty() {
        let p = PathBuf::from("/tmp/foo/bar.rs");
        let out = strip_worktree_prefix(&p, &DiffOptions::default());
        // Default opts have empty paths → strip is a no-op.
        assert_eq!(out, p);
    }

    #[test]
    fn strip_worktree_prefix_strips_old_root() {
        let opts = DiffOptions {
            old_worktree_path: PathBuf::from("/tmp/old"),
            new_worktree_path: PathBuf::from("/tmp/new"),
        };
        let p = PathBuf::from("/tmp/old/src/foo.rs");
        let out = strip_worktree_prefix(&p, &opts);
        assert_eq!(out, PathBuf::from("src/foo.rs"));
    }
}
