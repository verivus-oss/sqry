//! Graph diff and comparison for semantic diff operations.
//!
//! This module provides functionality for comparing two CodeGraph instances
//! to detect symbol changes between different versions of code (e.g., git refs).

// Allow collapsible_if for readability in this module - the nested if patterns
// are clearer than collapsed let-chains for the rename detection logic.
#![allow(clippy::collapsible_if)]
// Allow needless_borrow since the borrow patterns are consistent for function calls
#![allow(clippy::needless_borrow)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::unified::concurrent::CodeGraph;
use super::unified::node::kind::NodeKind;

// ============================================================================
// Constants for rename detection heuristics
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
// Core Types
// ============================================================================

/// Type of change detected for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeType {
    /// Node was added in target version
    Added,
    /// Node was removed in target version
    Removed,
    /// Node body/location changed
    Modified,
    /// Node was renamed
    Renamed,
    /// Node signature changed
    SignatureChanged,
    /// Node is unchanged (only included if requested)
    Unchanged,
}

impl ChangeType {
    /// Returns the string representation of the change type.
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

/// Location information for a symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLocation {
    /// File path relative to workspace
    pub file_path: PathBuf,
    /// Start line (1-based)
    pub start_line: u32,
    /// End line (1-based)
    pub end_line: u32,
    /// Start column (0-based)
    pub start_column: u32,
    /// End column (0-based)
    pub end_column: u32,
}

/// A change record for a single symbol.
#[derive(Debug, Clone)]
pub struct NodeChange {
    /// Node name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Node kind (function, class, etc.)
    pub kind: String,
    /// Type of change
    pub change_type: ChangeType,
    /// Location in base version (for removed, modified, renamed)
    pub base_location: Option<NodeLocation>,
    /// Location in target version (for added, modified, renamed)
    pub target_location: Option<NodeLocation>,
    /// Signature in base version
    pub signature_before: Option<String>,
    /// Signature in target version
    pub signature_after: Option<String>,
}

/// Summary statistics for a diff.
#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    /// Number of added symbols
    pub added: u64,
    /// Number of removed symbols
    pub removed: u64,
    /// Number of modified symbols
    pub modified: u64,
    /// Number of renamed symbols
    pub renamed: u64,
    /// Number of signature-changed symbols
    pub signature_changed: u64,
    /// Number of unchanged symbols
    pub unchanged: u64,
}

impl DiffSummary {
    /// Computes summary from a list of changes.
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

/// Result of a graph comparison.
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// All detected changes
    pub changes: Vec<NodeChange>,
    /// Summary statistics
    pub summary: DiffSummary,
}

// ============================================================================
// Internal Node Snapshot
// ============================================================================

/// Internal representation of a node for comparison.
#[derive(Clone)]
struct NodeSnapshot {
    name: String,
    qualified_name: String,
    kind_str: String,
    kind: NodeKind,
    signature: Option<String>,
    file_path: PathBuf,
    start_line: u32,
    end_line: u32,
    start_column: u32,
    end_column: u32,
}

// ============================================================================
// Graph Comparator
// ============================================================================

/// Compares two `CodeGraph` instances and produces change records.
///
/// This comparator uses heuristic matching to detect renames and
/// distinguishes between body changes and signature changes.
///
/// # Example
///
/// ```no_run
/// use sqry_core::graph::diff::GraphComparator;
/// use sqry_core::graph::CodeGraph;
/// use std::sync::Arc;
/// use std::path::PathBuf;
///
/// let base_graph: Arc<CodeGraph> = // ... build graph for base version
/// # unimplemented!();
/// let target_graph: Arc<CodeGraph> = // ... build graph for target version
/// # unimplemented!();
///
/// let comparator = GraphComparator::new(
///     base_graph,
///     target_graph,
///     PathBuf::from("/workspace"),
///     PathBuf::from("/tmp/base-worktree"),
///     PathBuf::from("/tmp/target-worktree"),
/// );
///
/// let result = comparator.compute_changes()?;
/// println!("Found {} changes", result.changes.len());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct GraphComparator {
    base: Arc<CodeGraph>,
    target: Arc<CodeGraph>,
    #[allow(dead_code)] // Kept for future use (translating paths to workspace)
    workspace_root: PathBuf,
    base_worktree_path: PathBuf,
    target_worktree_path: PathBuf,
}

impl GraphComparator {
    /// Creates a new graph comparator.
    ///
    /// # Arguments
    ///
    /// * `base` - `CodeGraph` for the base version
    /// * `target` - `CodeGraph` for the target version
    /// * `workspace_root` - Path to the actual workspace root
    /// * `base_worktree_path` - Path to the temporary base worktree
    /// * `target_worktree_path` - Path to the temporary target worktree
    #[must_use]
    pub fn new(
        base: Arc<CodeGraph>,
        target: Arc<CodeGraph>,
        workspace_root: PathBuf,
        base_worktree_path: PathBuf,
        target_worktree_path: PathBuf,
    ) -> Self {
        Self {
            base,
            target,
            workspace_root,
            base_worktree_path,
            target_worktree_path,
        }
    }

    /// Computes all symbol changes between base and target graphs.
    ///
    /// # Errors
    ///
    /// Returns error if graph access fails.
    pub fn compute_changes(&self) -> anyhow::Result<DiffResult> {
        tracing::debug!("Computing symbol changes from CodeGraph");

        // Build qualified_name maps for O(1) lookup
        let base_map = Self::build_node_map(&self.base, &self.base_worktree_path);
        let target_map = Self::build_node_map(&self.target, &self.target_worktree_path);

        let (added_nodes, modified_changes) =
            self.collect_added_and_modified(&base_map, &target_map);
        let removed_nodes = collect_removed_nodes(&base_map, &target_map);

        let mut changes = modified_changes;

        let (rename_changes, renamed_qnames) = self.collect_renames(&removed_nodes, &added_nodes);
        changes.extend(rename_changes);

        self.append_removed_changes(&mut changes, &removed_nodes, &renamed_qnames);
        self.append_added_changes(&mut changes, &added_nodes, &renamed_qnames);

        let summary = DiffSummary::from_changes(&changes);

        tracing::debug!(total_changes = changes.len(), "Computed symbol changes");

        Ok(DiffResult { changes, summary })
    }

    /// Builds a map of `qualified_name` -> `NodeSnapshot` from the graph.
    fn build_node_map(graph: &CodeGraph, worktree_path: &Path) -> HashMap<String, NodeSnapshot> {
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let mut map = HashMap::new();

        for (_node_id, entry) in snapshot.iter_nodes() {
            let name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();

            let qualified_name = entry
                .qualified_name
                .and_then(|sid| strings.resolve(sid))
                .map_or_else(|| name.clone(), |s| s.to_string());

            // Skip nodes without qualified names (call sites, imports, etc.)
            if qualified_name.is_empty() {
                continue;
            }

            let signature = entry
                .signature
                .and_then(|sid| strings.resolve(sid))
                .map(|s| s.to_string());

            let file_path = files
                .resolve(entry.file)
                .map(|p| worktree_path.join(p.as_ref()))
                .unwrap_or_default();

            let node_snap = NodeSnapshot {
                name,
                qualified_name: qualified_name.clone(),
                kind_str: node_kind_to_string(entry.kind),
                kind: entry.kind,
                signature,
                file_path,
                start_line: entry.start_line,
                end_line: entry.end_line,
                start_column: entry.start_column,
                end_column: entry.end_column,
            };

            map.insert(qualified_name, node_snap);
        }

        map
    }

    fn collect_added_and_modified(
        &self,
        base_map: &HashMap<String, NodeSnapshot>,
        target_map: &HashMap<String, NodeSnapshot>,
    ) -> (Vec<NodeSnapshot>, Vec<NodeChange>) {
        let mut added_nodes = Vec::new();
        let mut changes = Vec::new();

        for (qname, target_snap) in target_map {
            match base_map.get(qname) {
                None => {
                    added_nodes.push(target_snap.clone());
                }
                Some(base_snap) => {
                    if let Some(change) = self.detect_modification(base_snap, target_snap, qname) {
                        changes.push(change);
                    }
                }
            }
        }

        (added_nodes, changes)
    }

    fn collect_renames(
        &self,
        removed_nodes: &[NodeSnapshot],
        added_nodes: &[NodeSnapshot],
    ) -> (Vec<NodeChange>, HashSet<String>) {
        let renames = self.detect_renames(removed_nodes, added_nodes);
        let mut rename_changes = Vec::new();
        let mut renamed_qnames = HashSet::new();

        for (base_snap, target_snap) in &renames {
            renamed_qnames.insert(base_snap.qualified_name.clone());
            renamed_qnames.insert(target_snap.qualified_name.clone());
            rename_changes.push(self.create_renamed_change(base_snap, target_snap));
        }

        (rename_changes, renamed_qnames)
    }

    fn append_removed_changes(
        &self,
        changes: &mut Vec<NodeChange>,
        removed_nodes: &[NodeSnapshot],
        renamed_qnames: &HashSet<String>,
    ) {
        for base_snap in removed_nodes {
            if !renamed_qnames.contains(&base_snap.qualified_name) {
                changes.push(self.create_removed_change(base_snap));
            }
        }
    }

    fn append_added_changes(
        &self,
        changes: &mut Vec<NodeChange>,
        added_nodes: &[NodeSnapshot],
        renamed_qnames: &HashSet<String>,
    ) {
        for target_snap in added_nodes {
            if !renamed_qnames.contains(&target_snap.qualified_name) {
                changes.push(self.create_added_change(target_snap));
            }
        }
    }

    /// Detects if a node was modified and returns the change record.
    fn detect_modification(
        &self,
        base_snap: &NodeSnapshot,
        target_snap: &NodeSnapshot,
        qname: &str,
    ) -> Option<NodeChange> {
        // Check for signature changes
        let signature_changed = base_snap.signature != target_snap.signature;

        // Normalize file paths for comparison
        let base_rel = self.strip_worktree_prefix(&base_snap.file_path);
        let target_rel = self.strip_worktree_prefix(&target_snap.file_path);

        // Check for body changes (line numbers or file location)
        let body_changed = base_snap.start_line != target_snap.start_line
            || base_snap.end_line != target_snap.end_line
            || base_rel != target_rel;

        if signature_changed {
            Some(NodeChange {
                name: target_snap.name.clone(),
                qualified_name: qname.to_string(),
                kind: target_snap.kind_str.clone(),
                change_type: ChangeType::SignatureChanged,
                base_location: Some(self.node_snap_to_location(base_snap, true)),
                target_location: Some(self.node_snap_to_location(target_snap, false)),
                signature_before: base_snap.signature.clone(),
                signature_after: target_snap.signature.clone(),
            })
        } else if body_changed {
            Some(NodeChange {
                name: target_snap.name.clone(),
                qualified_name: qname.to_string(),
                kind: target_snap.kind_str.clone(),
                change_type: ChangeType::Modified,
                base_location: Some(self.node_snap_to_location(base_snap, true)),
                target_location: Some(self.node_snap_to_location(target_snap, false)),
                signature_before: base_snap.signature.clone(),
                signature_after: target_snap.signature.clone(),
            })
        } else {
            None
        }
    }

    /// Detects renamed nodes using heuristic matching.
    fn detect_renames(
        &self,
        removed: &[NodeSnapshot],
        added: &[NodeSnapshot],
    ) -> Vec<(NodeSnapshot, NodeSnapshot)> {
        let mut renames = Vec::new();
        let mut matched_added = HashSet::new();

        for removed_snap in removed {
            let mut best_match: Option<(usize, f64)> = None;

            for (idx, added_snap) in added.iter().enumerate() {
                if matched_added.contains(&idx) {
                    continue;
                }

                let Some(score) = self.is_likely_rename(removed_snap, added_snap) else {
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

            if let Some((idx, score)) = best_match {
                if score >= RENAME_CONFIDENCE_THRESHOLD {
                    matched_added.insert(idx);
                    renames.push((removed_snap.clone(), added[idx].clone()));

                    tracing::debug!(
                        from = %removed_snap.qualified_name,
                        to = %added[idx].qualified_name,
                        confidence = %score,
                        "Detected rename"
                    );
                }
            }
        }

        renames
    }

    /// Determines if two nodes are likely the same symbol that was renamed.
    fn is_likely_rename(
        &self,
        base_snap: &NodeSnapshot,
        target_snap: &NodeSnapshot,
    ) -> Option<f64> {
        // Criterion 1: Must be same node kind
        if base_snap.kind != target_snap.kind {
            return None;
        }

        let mut confidence = 0.0;

        // Criterion 2: Signature similarity (70% weight)
        let sig_score = match (&base_snap.signature, &target_snap.signature) {
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

        confidence += sig_score * SIGNATURE_WEIGHT;

        // Normalize file paths for comparison
        let base_rel = self.strip_worktree_prefix(&base_snap.file_path);
        let target_rel = self.strip_worktree_prefix(&target_snap.file_path);

        // Criterion 3: Location proximity (30% weight)
        let location_score = if base_rel == target_rel {
            let base_line: i32 = base_snap.start_line.try_into().unwrap_or(i32::MAX);
            let target_line: i32 = target_snap.start_line.try_into().unwrap_or(i32::MAX);
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

    fn create_added_change(&self, snap: &NodeSnapshot) -> NodeChange {
        NodeChange {
            name: snap.name.clone(),
            qualified_name: snap.qualified_name.clone(),
            kind: snap.kind_str.clone(),
            change_type: ChangeType::Added,
            base_location: None,
            target_location: Some(self.node_snap_to_location(snap, false)),
            signature_before: None,
            signature_after: snap.signature.clone(),
        }
    }

    fn create_removed_change(&self, snap: &NodeSnapshot) -> NodeChange {
        NodeChange {
            name: snap.name.clone(),
            qualified_name: snap.qualified_name.clone(),
            kind: snap.kind_str.clone(),
            change_type: ChangeType::Removed,
            base_location: Some(self.node_snap_to_location(snap, true)),
            target_location: None,
            signature_before: snap.signature.clone(),
            signature_after: None,
        }
    }

    fn create_renamed_change(
        &self,
        base_snap: &NodeSnapshot,
        target_snap: &NodeSnapshot,
    ) -> NodeChange {
        NodeChange {
            name: target_snap.name.clone(),
            qualified_name: target_snap.qualified_name.clone(),
            kind: target_snap.kind_str.clone(),
            change_type: ChangeType::Renamed,
            base_location: Some(self.node_snap_to_location(base_snap, true)),
            target_location: Some(self.node_snap_to_location(target_snap, false)),
            signature_before: base_snap.signature.clone(),
            signature_after: target_snap.signature.clone(),
        }
    }

    fn node_snap_to_location(&self, snap: &NodeSnapshot, is_base: bool) -> NodeLocation {
        let relative_path = self.translate_worktree_path_to_relative(&snap.file_path, is_base);

        NodeLocation {
            file_path: relative_path,
            start_line: snap.start_line,
            end_line: snap.end_line,
            start_column: snap.start_column,
            end_column: snap.end_column,
        }
    }

    fn strip_worktree_prefix(&self, path: &Path) -> PathBuf {
        if let Ok(relative) = path.strip_prefix(&self.base_worktree_path) {
            return relative.to_path_buf();
        }
        if let Ok(relative) = path.strip_prefix(&self.target_worktree_path) {
            return relative.to_path_buf();
        }
        path.to_path_buf()
    }

    fn translate_worktree_path_to_relative(&self, worktree_path: &Path, is_base: bool) -> PathBuf {
        let worktree_root = if is_base {
            &self.base_worktree_path
        } else {
            &self.target_worktree_path
        };

        if let Ok(relative) = worktree_path.strip_prefix(worktree_root) {
            return relative.to_path_buf();
        }

        worktree_path.to_path_buf()
    }
}

/// Collect nodes that are in base but not in target.
fn collect_removed_nodes(
    base_map: &HashMap<String, NodeSnapshot>,
    target_map: &HashMap<String, NodeSnapshot>,
) -> Vec<NodeSnapshot> {
    base_map
        .iter()
        .filter(|(qname, _)| !target_map.contains_key(*qname))
        .map(|(_, snap)| snap.clone())
        .collect()
}

/// Computes the Levenshtein similarity between two strings (0.0 to 1.0).
fn levenshtein_similarity(a: &str, b: &str) -> f64 {
    use rapidfuzz::distance::levenshtein;

    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }

    let distance = levenshtein::distance(a.chars(), b.chars());
    let distance = f64::from(u32::try_from(distance).unwrap_or(u32::MAX));
    let max_len = f64::from(u32::try_from(max_len).unwrap_or(u32::MAX));
    1.0 - (distance / max_len)
}

/// Convert `NodeKind` to string representation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_similarity_identical() {
        assert!((levenshtein_similarity("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_levenshtein_similarity_empty() {
        assert!((levenshtein_similarity("", "") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_levenshtein_similarity_similar() {
        let score = levenshtein_similarity("hello", "hallo");
        assert!(score > 0.7);
    }

    #[test]
    fn test_levenshtein_similarity_different() {
        let score = levenshtein_similarity("hello", "world");
        assert!(score < 0.5);
    }

    #[test]
    fn test_diff_summary_from_changes() {
        let changes = vec![
            NodeChange {
                name: "foo".to_string(),
                qualified_name: "mod::foo".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: "bar".to_string(),
                qualified_name: "mod::bar".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Removed,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: "baz".to_string(),
                qualified_name: "mod::baz".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Modified,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];

        let summary = DiffSummary::from_changes(&changes);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.modified, 1);
        assert_eq!(summary.renamed, 0);
        assert_eq!(summary.signature_changed, 0);
    }

    #[test]
    fn test_change_type_as_str() {
        assert_eq!(ChangeType::Added.as_str(), "added");
        assert_eq!(ChangeType::Removed.as_str(), "removed");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
        assert_eq!(ChangeType::Renamed.as_str(), "renamed");
        assert_eq!(ChangeType::SignatureChanged.as_str(), "signature_changed");
        assert_eq!(ChangeType::Unchanged.as_str(), "unchanged");
    }

    #[test]
    fn test_node_kind_to_string() {
        assert_eq!(node_kind_to_string(NodeKind::Function), "function");
        assert_eq!(node_kind_to_string(NodeKind::Class), "class");
        assert_eq!(node_kind_to_string(NodeKind::Method), "method");
    }
}
