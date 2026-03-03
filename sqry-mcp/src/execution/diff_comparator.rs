use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::kind::NodeKind;

use super::{DiffSummary, NodeChange, NodeRefData, PositionData, RangeData};

const SIGNATURE_WEIGHT: f64 = 0.7;
const LOCATION_WEIGHT: f64 = 0.3;
const SIGNATURE_MIN_SCORE: f64 = 0.7;
const RENAME_CONFIDENCE_THRESHOLD: f64 = 0.9;
const SAME_FILE_LINE_WINDOW: i32 = 50;
const SAME_FILE_LINE_NORMALIZER: f64 = 100.0;
const SAME_FILE_MAX_PENALTY: f64 = 0.5;
const SAME_FILE_FAR_SCORE: f64 = 0.3;
const CROSS_FILE_LOCATION_SCORE: f64 = 0.7;

/// Computes the Levenshtein similarity between two strings (0.0 to 1.0)
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

/// Computes summary statistics from a list of changes
pub fn compute_summary(changes: &[NodeChange]) -> DiffSummary {
    let mut summary = DiffSummary {
        added: 0,
        removed: 0,
        modified: 0,
        renamed: 0,
        signature_changed: 0,
        unchanged: 0,
    };

    for change in changes {
        match change.change_type.as_str() {
            "added" => summary.added += 1,
            "removed" => summary.removed += 1,
            "modified" => summary.modified += 1,
            "renamed" => summary.renamed += 1,
            "signature_changed" => summary.signature_changed += 1,
            _ => summary.unchanged += 1,
        }
    }

    summary
}

// ============================================================================
// GraphComparator - CodeGraph-based comparison (replaces IndexComparator)
// ============================================================================

/// Internal representation of a node for comparison.
#[derive(Clone)]
struct NodeSnapshot {
    /// Resolved name string.
    name: String,
    /// Resolved qualified name string.
    qualified_name: String,
    /// Node kind as string.
    kind_str: String,
    /// Node kind enum for comparison.
    kind: NodeKind,
    /// Resolved signature string.
    signature: Option<String>,
    /// Resolved file path (relative to workspace).
    file_path: PathBuf,
    /// Start line (1-indexed).
    start_line: u32,
    /// End line (1-indexed).
    end_line: u32,
    /// Language string.
    language: String,
}

/// Compares two `CodeGraph` instances and produces change records.
///
/// This is the unified graph replacement for `IndexComparator`.
pub struct GraphComparator {
    base: Arc<CodeGraph>,
    target: Arc<CodeGraph>,
    workspace_root: PathBuf,
    base_worktree_path: PathBuf,
    target_worktree_path: PathBuf,
}

impl GraphComparator {
    /// Creates a new graph comparator.
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
    pub fn compute_changes(&self) -> anyhow::Result<Vec<NodeChange>> {
        tracing::debug!("Computing symbol changes from CodeGraph");

        // Build qualified_name maps for O(1) lookup
        let base_map = Self::build_node_map(&self.base, &self.base_worktree_path);
        let target_map = Self::build_node_map(&self.target, &self.target_worktree_path);

        let (added_nodes, modified_changes) =
            self.collect_added_and_modified(&base_map, &target_map)?;
        let removed_nodes = collect_removed_nodes(&base_map, &target_map);

        let mut changes = modified_changes;

        let (rename_changes, renamed_qnames) =
            self.collect_renames(&removed_nodes, &added_nodes)?;
        changes.extend(rename_changes);

        self.append_removed_changes(&mut changes, &removed_nodes, &renamed_qnames)?;
        self.append_added_changes(&mut changes, &added_nodes, &renamed_qnames)?;

        tracing::debug!(total_changes = changes.len(), "Computed symbol changes");

        Ok(changes)
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

            let language = files
                .language_for_file(entry.file)
                .map_or_else(|| "unknown".to_string(), |l| l.to_string());

            let node_snap = NodeSnapshot {
                name,
                qualified_name: qualified_name.clone(),
                kind_str: node_kind_to_string(entry.kind),
                kind: entry.kind,
                signature,
                file_path,
                start_line: entry.start_line,
                end_line: entry.end_line,
                language,
            };

            map.insert(qualified_name, node_snap);
        }

        map
    }

    fn collect_added_and_modified(
        &self,
        base_map: &HashMap<String, NodeSnapshot>,
        target_map: &HashMap<String, NodeSnapshot>,
    ) -> anyhow::Result<(Vec<NodeSnapshot>, Vec<NodeChange>)> {
        let mut added_nodes = Vec::new();
        let mut changes = Vec::new();

        for (qname, target_snap) in target_map {
            match base_map.get(qname) {
                None => {
                    added_nodes.push(target_snap.clone());
                }
                Some(base_snap) => {
                    if let Some(change) = self.detect_modification(base_snap, target_snap, qname)? {
                        changes.push(change);
                    }
                }
            }
        }

        Ok((added_nodes, changes))
    }

    fn collect_renames(
        &self,
        removed_nodes: &[NodeSnapshot],
        added_nodes: &[NodeSnapshot],
    ) -> anyhow::Result<(Vec<NodeChange>, HashSet<String>)> {
        let renames = self.detect_renames(removed_nodes, added_nodes);
        let mut rename_changes = Vec::new();
        let mut renamed_qnames = HashSet::new();

        for (base_snap, target_snap) in &renames {
            renamed_qnames.insert(base_snap.qualified_name.clone());
            renamed_qnames.insert(target_snap.qualified_name.clone());
            rename_changes.push(self.create_renamed_change(base_snap, target_snap)?);
        }

        Ok((rename_changes, renamed_qnames))
    }

    fn append_removed_changes(
        &self,
        changes: &mut Vec<NodeChange>,
        removed_nodes: &[NodeSnapshot],
        renamed_qnames: &HashSet<String>,
    ) -> anyhow::Result<()> {
        for base_snap in removed_nodes {
            if !renamed_qnames.contains(&base_snap.qualified_name) {
                changes.push(self.create_removed_change(base_snap)?);
            }
        }
        Ok(())
    }

    fn append_added_changes(
        &self,
        changes: &mut Vec<NodeChange>,
        added_nodes: &[NodeSnapshot],
        renamed_qnames: &HashSet<String>,
    ) -> anyhow::Result<()> {
        for target_snap in added_nodes {
            if !renamed_qnames.contains(&target_snap.qualified_name) {
                changes.push(self.create_added_change(target_snap)?);
            }
        }
        Ok(())
    }

    /// Detects if a node was modified and returns the change record.
    fn detect_modification(
        &self,
        base_snap: &NodeSnapshot,
        target_snap: &NodeSnapshot,
        qname: &str,
    ) -> anyhow::Result<Option<NodeChange>> {
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
            // Signature change is more specific than modified
            Ok(Some(NodeChange {
                symbol_name: target_snap.name.clone(),
                qualified_name: qname.to_string(),
                kind: target_snap.kind_str.clone(),
                change_type: "signature_changed".to_string(),
                base_location: Some(self.node_snap_to_ref(base_snap, true)?),
                target_location: Some(self.node_snap_to_ref(target_snap, false)?),
                signature_before: base_snap.signature.clone(),
                signature_after: target_snap.signature.clone(),
            }))
        } else if body_changed {
            // Body changed but signature stayed the same
            Ok(Some(NodeChange {
                symbol_name: target_snap.name.clone(),
                qualified_name: qname.to_string(),
                kind: target_snap.kind_str.clone(),
                change_type: "modified".to_string(),
                base_location: Some(self.node_snap_to_ref(base_snap, true)?),
                target_location: Some(self.node_snap_to_ref(target_snap, false)?),
                signature_before: base_snap.signature.clone(),
                signature_after: target_snap.signature.clone(),
            }))
        } else {
            // No changes detected
            Ok(None)
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

            // If we found a good match (>= 90% confidence), record it as a rename
            if let Some((idx, score)) = best_match
                && score >= RENAME_CONFIDENCE_THRESHOLD
            {
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

    /// Creates a change record for an added node.
    fn create_added_change(&self, snap: &NodeSnapshot) -> anyhow::Result<NodeChange> {
        Ok(NodeChange {
            symbol_name: snap.name.clone(),
            qualified_name: snap.qualified_name.clone(),
            kind: snap.kind_str.clone(),
            change_type: "added".to_string(),
            base_location: None,
            target_location: Some(self.node_snap_to_ref(snap, false)?),
            signature_before: None,
            signature_after: snap.signature.clone(),
        })
    }

    /// Creates a change record for a removed node.
    fn create_removed_change(&self, snap: &NodeSnapshot) -> anyhow::Result<NodeChange> {
        Ok(NodeChange {
            symbol_name: snap.name.clone(),
            qualified_name: snap.qualified_name.clone(),
            kind: snap.kind_str.clone(),
            change_type: "removed".to_string(),
            base_location: Some(self.node_snap_to_ref(snap, true)?),
            target_location: None,
            signature_before: snap.signature.clone(),
            signature_after: None,
        })
    }

    /// Creates a change record for a renamed node.
    fn create_renamed_change(
        &self,
        base_snap: &NodeSnapshot,
        target_snap: &NodeSnapshot,
    ) -> anyhow::Result<NodeChange> {
        Ok(NodeChange {
            symbol_name: target_snap.name.clone(),
            qualified_name: target_snap.qualified_name.clone(),
            kind: target_snap.kind_str.clone(),
            change_type: "renamed".to_string(),
            base_location: Some(self.node_snap_to_ref(base_snap, true)?),
            target_location: Some(self.node_snap_to_ref(target_snap, false)?),
            signature_before: base_snap.signature.clone(),
            signature_after: target_snap.signature.clone(),
        })
    }

    /// Converts a `NodeSnapshot` to a `NodeRefData`, translating worktree paths.
    fn node_snap_to_ref(&self, snap: &NodeSnapshot, is_base: bool) -> anyhow::Result<NodeRefData> {
        // Translate worktree path to workspace path
        let real_file_path = self.translate_worktree_path_to_workspace(&snap.file_path, is_base);

        let file_uri = url::Url::from_file_path(&real_file_path)
            .map_err(|()| anyhow::anyhow!("Invalid file path: {}", real_file_path.display()))?
            .to_string();

        let range = RangeData {
            start: PositionData {
                line: snap.start_line.saturating_sub(1),
                character: 0,
            },
            end: PositionData {
                line: snap.end_line.saturating_sub(1),
                character: 0,
            },
        };

        Ok(NodeRefData {
            name: snap.name.clone(),
            qualified_name: snap.qualified_name.clone(),
            kind: snap.kind_str.clone(),
            language: snap.language.clone(),
            file_uri,
            range,
            metadata: None,
        })
    }

    /// Strips worktree prefix from a path to get relative path.
    fn strip_worktree_prefix(&self, path: &Path) -> PathBuf {
        if let Ok(relative) = path.strip_prefix(&self.base_worktree_path) {
            return relative.to_path_buf();
        }
        if let Ok(relative) = path.strip_prefix(&self.target_worktree_path) {
            return relative.to_path_buf();
        }
        path.to_path_buf()
    }

    /// Translates a worktree path to the real workspace path.
    fn translate_worktree_path_to_workspace(&self, worktree_path: &Path, is_base: bool) -> PathBuf {
        let worktree_root = if is_base {
            &self.base_worktree_path
        } else {
            &self.target_worktree_path
        };

        if let Ok(relative) = worktree_path.strip_prefix(worktree_root) {
            return self.workspace_root.join(relative);
        }

        // If prefix didn't match, return as-is
        tracing::trace!(
            path = %worktree_path.display(),
            "Worktree path did not match expected root; returning as-is"
        );
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
    use approx::assert_abs_diff_eq;

    #[test]
    fn test_levenshtein_similarity() {
        assert_abs_diff_eq!(
            levenshtein_similarity("hello", "hello"),
            1.0,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(levenshtein_similarity("", ""), 1.0, epsilon = 1e-10);
        assert!(levenshtein_similarity("hello", "hallo") > 0.7);
        assert!(levenshtein_similarity("hello", "world") < 0.5);
    }

    #[test]
    fn test_compute_summary() {
        let changes = vec![
            NodeChange {
                symbol_name: "foo".to_string(),
                qualified_name: "mod::foo".to_string(),
                kind: "function".to_string(),
                change_type: "added".to_string(),
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                symbol_name: "bar".to_string(),
                qualified_name: "mod::bar".to_string(),
                kind: "function".to_string(),
                change_type: "removed".to_string(),
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                symbol_name: "baz".to_string(),
                qualified_name: "mod::baz".to_string(),
                kind: "function".to_string(),
                change_type: "modified".to_string(),
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];

        let summary = compute_summary(&changes);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.modified, 1);
        assert_eq!(summary.renamed, 0);
        assert_eq!(summary.signature_changed, 0);
    }
}
