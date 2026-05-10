//! Container tree building for hierarchical search
//!
//! Implements the v4 algorithm: smallest→largest processing to preserve nested containers.
//!
//! Key features:
//! - Real file content cache with `Arc<String>` to prevent duplicate reads
//! - `IndexMap` for deterministic iteration order
//! - Complete tie-breaker chains for reproducible sorting
//!
//! This module uses native graph types (`NodeId`, `NodeEntry`) directly without
//! intermediate Symbol conversion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::{NodeId, NodeKind};

use crate::execution::symbol_utils::build_context;
use crate::execution::types::{PositionData, RangeData};
use crate::tools::HierarchicalSearchArgs;

use super::{
    ContainerGroup, HierarchicalSymbol, estimate_tokens, node_kind_to_string, round_relevance_score,
};

/// Request-scoped cache for file content
///
/// Key hygiene: Uses canonicalized paths to prevent duplicate entries
/// for the same file accessed via different paths.
pub struct FileContentCache {
    cache: parking_lot::Mutex<HashMap<PathBuf, Arc<String>>>,
}

impl FileContentCache {
    pub fn new() -> Self {
        Self {
            cache: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Get file content, reading from disk if not cached
    ///
    /// Path canonicalization ensures consistent cache keys.
    /// Errors are propagated, not swallowed.
    pub fn get(&self, path: &Path) -> Result<Arc<String>> {
        // Canonicalize path for consistent cache keys
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Check cache first
        {
            let cache = self.cache.lock();
            if let Some(content) = cache.get(&canonical) {
                return Ok(Arc::clone(content));
            }
        }

        // Read from disk - errors propagated
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let arc = Arc::new(content);

        // Insert into cache
        {
            let mut cache = self.cache.lock();
            cache.insert(canonical, Arc::clone(&arc));
        }

        Ok(arc)
    }

    /// Get number of cached files (for diagnostics/testing)
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.cache.lock().len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cache.lock().is_empty()
    }
}

impl Default for FileContentCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Container kinds that can contain other symbols/containers
const CONTAINER_KINDS: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::Module,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Service,
    NodeKind::Component,
];

/// Key for uniquely identifying containers by line range
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContainerKey {
    start_line: u32,
    end_line: u32,
}

impl ContainerKey {
    fn from_node(start_line: u32, end_line: u32) -> Self {
        Self {
            start_line,
            end_line,
        }
    }
}

/// Build recursive container tree from flat node list
///
/// Algorithm (v4 - FIXED):
/// 1. Identify all containers in the file
/// 2. Build parent-child relationships using line ranges (no removal)
/// 3. Sort containers by span size (smallest first)
/// 4. Process smallest→largest: attach children to parents
/// 5. Recursive `depth/parent_path` update from roots
/// 6. Assign symbols to their immediate parent container
pub fn build_container_tree(
    matched_nodes: &[(NodeId, f64)],
    all_file_nodes: &[NodeId],
    snapshot: &GraphSnapshot,
    _workspace_root: &Path,
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
    file_path: &Path,
) -> Result<(Vec<ContainerGroup>, Vec<HierarchicalSymbol>)> {
    // Step 1: Identify all containers with their ranges
    let containers = collect_containers(all_file_nodes, snapshot);

    if containers.is_empty() {
        // No containers - all symbols are top-level
        let top_level =
            build_top_level_symbols(matched_nodes, snapshot, args, file_cache, file_path)?;
        return Ok((Vec::new(), top_level));
    }

    // Step 2: Sort containers by span size (SMALLEST FIRST) using line range
    let mut sorted_containers = containers.clone();
    sort_containers_by_span(&mut sorted_containers, snapshot);

    // Step 3: Build containment map (child_key -> parent_key)
    let parent_map = build_parent_map(&sorted_containers, snapshot);

    // Step 4: Identify root containers (those without parents)
    let root_keys = collect_root_keys(&sorted_containers, &parent_map, snapshot);

    // Step 5: Create all container groups with initial depth=1, empty parent_path
    let mut container_map = build_container_map(&sorted_containers, snapshot);

    // Step 6: Build tree structure by processing from SMALLEST to LARGEST
    attach_container_children(
        &sorted_containers,
        &parent_map,
        &mut container_map,
        snapshot,
    );

    // Step 7: Extract root containers (everything left in map should be roots)
    let mut root_containers = extract_root_containers(&root_keys, &mut container_map);

    // Step 8: Recursively update depth and parent_path from roots down
    for root in &mut root_containers {
        update_nested_depth_and_path(root);
    }

    // Step 9: Assign matched symbols to containers
    let top_level_symbols = assign_symbols_to_containers(
        matched_nodes,
        snapshot,
        args,
        file_cache,
        file_path,
        &mut root_containers,
    )?;

    // Step 10: Sort containers by score and update totals
    sort_containers_by_score(&mut root_containers);
    for container in &mut root_containers {
        update_container_metadata(container);
    }

    Ok((root_containers, top_level_symbols))
}

fn collect_containers(all_file_nodes: &[NodeId], snapshot: &GraphSnapshot) -> Vec<NodeId> {
    all_file_nodes
        .iter()
        .filter(|&&node_id| {
            snapshot
                .get_node(node_id)
                .is_some_and(|entry| CONTAINER_KINDS.contains(&entry.kind))
        })
        .copied()
        .collect()
}

fn sort_containers_by_span(containers: &mut [NodeId], snapshot: &GraphSnapshot) {
    containers.sort_by_key(|&node_id| {
        snapshot
            .get_node(node_id)
            .map_or(0, |entry| entry.end_line.saturating_sub(entry.start_line))
    });
}

fn build_parent_map(
    containers: &[NodeId],
    snapshot: &GraphSnapshot,
) -> IndexMap<ContainerKey, ContainerKey> {
    let mut parent_map = IndexMap::new();
    for &node_id in containers {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };
        let key = ContainerKey::from_node(entry.start_line, entry.end_line);
        if let Some(parent_key) = find_immediate_parent_container(node_id, containers, snapshot) {
            parent_map.insert(key, parent_key);
        }
    }
    parent_map
}

fn collect_root_keys(
    containers: &[NodeId],
    parent_map: &IndexMap<ContainerKey, ContainerKey>,
    snapshot: &GraphSnapshot,
) -> Vec<ContainerKey> {
    containers
        .iter()
        .filter_map(|&node_id| {
            snapshot
                .get_node(node_id)
                .map(|entry| ContainerKey::from_node(entry.start_line, entry.end_line))
        })
        .filter(|k| !parent_map.contains_key(k))
        .collect()
}

fn build_container_map(
    containers: &[NodeId],
    snapshot: &GraphSnapshot,
) -> IndexMap<ContainerKey, ContainerGroup> {
    let mut container_map = IndexMap::new();
    for &node_id in containers {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };
        let key = ContainerKey::from_node(entry.start_line, entry.end_line);
        let group = create_container_group(node_id, snapshot);
        container_map.insert(key, group);
    }
    container_map
}

fn attach_container_children(
    containers: &[NodeId],
    parent_map: &IndexMap<ContainerKey, ContainerKey>,
    container_map: &mut IndexMap<ContainerKey, ContainerGroup>,
    snapshot: &GraphSnapshot,
) {
    for &node_id in containers {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };
        let key = ContainerKey::from_node(entry.start_line, entry.end_line);

        // Find all direct children of this container
        let child_keys: Vec<ContainerKey> = parent_map
            .iter()
            .filter(|(_, parent)| **parent == key)
            .map(|(child, _)| child.clone())
            .collect();

        // Move children from map into this container's nested_containers
        for child_key in child_keys {
            if let Some(child_container) = container_map.shift_remove(&child_key)
                && let Some(parent_container) = container_map.get_mut(&key)
            {
                parent_container.nested_containers.push(child_container);
            }
        }
    }
}

fn extract_root_containers(
    root_keys: &[ContainerKey],
    container_map: &mut IndexMap<ContainerKey, ContainerGroup>,
) -> Vec<ContainerGroup> {
    root_keys
        .iter()
        .filter_map(|k| container_map.shift_remove(k))
        .collect()
}

fn assign_symbols_to_containers(
    matched_nodes: &[(NodeId, f64)],
    snapshot: &GraphSnapshot,
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
    file_path: &Path,
    root_containers: &mut [ContainerGroup],
) -> Result<Vec<HierarchicalSymbol>> {
    let strings = snapshot.strings();
    let mut top_level_symbols = Vec::new();

    for &(node_id, score) in matched_nodes {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };

        let hier_symbol =
            build_hierarchical_symbol(node_id, score, snapshot, args, file_cache, file_path)?;
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Find the smallest container that contains this symbol
        let parent_container =
            find_containing_container(entry.start_line, entry.end_line, root_containers);

        if let Some(container) = parent_container {
            container.children_names.push(name);
            container.children_count += 1;
            container.symbols.push(hier_symbol);
        } else {
            // Top-level symbol
            top_level_symbols.push(hier_symbol);
        }
    }

    Ok(top_level_symbols)
}

/// Recursively update depth and `parent_path` for all nested containers
fn update_nested_depth_and_path(container: &mut ContainerGroup) {
    let parent_depth = container.depth;
    let mut parent_path = container.parent_path.clone();
    parent_path.push(container.name.clone());

    for nested in &mut container.nested_containers {
        nested.depth = parent_depth + 1;
        nested.parent_path.clone_from(&parent_path);
        update_nested_depth_and_path(nested);
    }
}

/// Find the immediate parent container for a given container
fn find_immediate_parent_container(
    child_node_id: NodeId,
    all_containers: &[NodeId],
    snapshot: &GraphSnapshot,
) -> Option<ContainerKey> {
    let child_entry = snapshot.get_node(child_node_id)?;
    let child_start = child_entry.start_line;
    let child_end = child_entry.end_line;

    // Find the smallest container that fully contains this child
    let mut best_parent: Option<(u32, u32)> = None;
    let mut best_span = u32::MAX;

    for &candidate_id in all_containers {
        let Some(cand_entry) = snapshot.get_node(candidate_id) else {
            continue;
        };
        let cand_start = cand_entry.start_line;
        let cand_end = cand_entry.end_line;

        // Skip self
        if cand_start == child_start && cand_end == child_end {
            continue;
        }

        // Check if candidate contains child
        if cand_start <= child_start && cand_end >= child_end {
            let span = cand_end.saturating_sub(cand_start);
            if span < best_span {
                best_span = span;
                best_parent = Some((cand_start, cand_end));
            }
        }
    }

    best_parent.map(|(start, end)| ContainerKey::from_node(start, end))
}

/// Create a `ContainerGroup` from a node
fn create_container_group(node_id: NodeId, snapshot: &GraphSnapshot) -> ContainerGroup {
    let entry = snapshot.get_node(node_id);
    let strings = snapshot.strings();

    let (name, qualified_name, kind, start_line, end_line) = match entry {
        Some(entry) => {
            let fallback_name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry,
                strings,
                snapshot.files(),
                &fallback_name,
            );
            let name = qualified_name.clone();
            let kind = node_kind_to_string(entry.kind).to_string();
            (
                name,
                qualified_name,
                kind,
                entry.start_line as usize,
                entry.end_line as usize,
            )
        }
        None => (String::new(), String::new(), String::new(), 0, 0),
    };

    ContainerGroup {
        name,
        qualified_name,
        kind,
        estimated_tokens: 0,
        depth: 1,
        parent_path: Vec::new(),
        byte_range: (start_line, end_line), // Using line range as a proxy
        symbols: Vec::new(),
        nested_containers: Vec::new(),
        symbol_count: 0,
        children_count: 0,
        children_names: Vec::new(),
        container_context: None,
        merged_container_tokens: 0,
    }
}

/// Find the smallest containing container for a symbol (recursive search)
fn find_containing_container(
    sym_start: u32,
    sym_end: u32,
    containers: &mut [ContainerGroup],
) -> Option<&mut ContainerGroup> {
    // Find the smallest container that contains this symbol
    let mut best_idx: Option<usize> = None;
    let mut best_span = usize::MAX;

    for (idx, container) in containers.iter().enumerate() {
        let (cstart, cend) = container.byte_range; // Actually line_range

        // Check if container contains symbol
        if cstart <= sym_start as usize && cend >= sym_end as usize {
            let span = cend.saturating_sub(cstart);
            if span < best_span {
                best_span = span;
                best_idx = Some(idx);
            }
        }
    }

    let idx = best_idx?;

    // Check nested containers first (they're smaller, so preferred)
    // We need to check if nested containers contain the symbol before getting mutable ref
    let has_matching_nested = containers[idx].nested_containers.iter().any(|nested| {
        let (cstart, cend) = nested.byte_range;
        cstart <= sym_start as usize && cend >= sym_end as usize
    });

    if has_matching_nested {
        find_containing_container(sym_start, sym_end, &mut containers[idx].nested_containers)
    } else {
        Some(&mut containers[idx])
    }
}

/// Build a `HierarchicalSymbol` from a node
fn build_hierarchical_symbol(
    node_id: NodeId,
    score: f64,
    snapshot: &GraphSnapshot,
    args: &HierarchicalSearchArgs,
    _file_cache: &FileContentCache,
    file_path: &Path,
) -> Result<HierarchicalSymbol> {
    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow::anyhow!("Node not found in snapshot"))?;
    let strings = snapshot.strings();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry,
        strings,
        snapshot.files(),
        &name,
    );
    let display_name = qualified_name.clone();
    let kind = node_kind_to_string(entry.kind).to_string();

    // Build code context using the actual build_context function
    let context = if args.context_lines > 0 {
        // build_context takes (file_path, start_line, end_line, context_lines)
        // and returns Result<Option<CodeContext>>
        build_context(
            file_path,
            entry.start_line as usize,
            entry.end_line as usize,
            args.context_lines,
        )?
    } else {
        None
    };

    // Estimate tokens
    let estimated_tokens = context.as_ref().map_or(0, |c| estimate_tokens(&c.code));

    // Get signature from graph
    let signature = entry
        .signature
        .and_then(|sid| strings.resolve(sid))
        .map(|s| s.to_string());

    Ok(HierarchicalSymbol {
        name: display_name,
        qualified_name,
        kind,
        range: RangeData {
            start: PositionData {
                line: entry.start_line,
                character: entry.start_column,
            },
            end: PositionData {
                line: entry.end_line,
                character: entry.end_column,
            },
        },
        score: round_relevance_score(score),
        estimated_tokens,
        context,
        signature,
        merged: false,
        original_level: None,
        clustered_count: None,
        macro_metadata: crate::execution::symbol_utils::get_macro_metadata_for_node(
            snapshot, node_id,
        ),
    })
}

/// Build top-level symbols (when there are no containers)
fn build_top_level_symbols(
    nodes: &[(NodeId, f64)],
    snapshot: &GraphSnapshot,
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
    file_path: &Path,
) -> Result<Vec<HierarchicalSymbol>> {
    nodes
        .iter()
        .map(|&(node_id, score)| {
            build_hierarchical_symbol(node_id, score, snapshot, args, file_cache, file_path)
        })
        .collect()
}

/// Sort containers deterministically
/// Order: score desc → `start_line` asc → `end_line` asc → name asc
fn sort_containers_by_score(containers: &mut [ContainerGroup]) {
    containers.sort_by(|a, b| {
        let a_max = max_score_in_container(a);
        let b_max = max_score_in_container(b);

        match b_max
            .partial_cmp(&a_max)
            .unwrap_or(std::cmp::Ordering::Equal)
        {
            std::cmp::Ordering::Equal => {
                let (a_start, a_end) = a.byte_range;
                let (b_start, b_end) = b.byte_range;
                match a_start.cmp(&b_start) {
                    std::cmp::Ordering::Equal => match a_end.cmp(&b_end) {
                        std::cmp::Ordering::Equal => a.name.cmp(&b.name),
                        other => other,
                    },
                    other => other,
                }
            }
            other => other,
        }
    });

    // Recursively sort nested containers and their symbols
    for container in containers {
        sort_containers_by_score(&mut container.nested_containers);
        sort_symbols_deterministic(&mut container.symbols);
    }
}

/// Sort symbols deterministically
/// Order: score desc → line asc → name asc
fn sort_symbols_deterministic(symbols: &mut [HierarchicalSymbol]) {
    symbols.sort_by(|a, b| {
        match b
            .score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
        {
            std::cmp::Ordering::Equal => match a.range.start.line.cmp(&b.range.start.line) {
                std::cmp::Ordering::Equal => a.name.cmp(&b.name),
                other => other,
            },
            other => other,
        }
    });
}

/// Get maximum score in a container (recursive)
fn max_score_in_container(container: &ContainerGroup) -> f64 {
    let direct_max = container
        .symbols
        .iter()
        .map(|s| s.score)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);

    let nested_max = container
        .nested_containers
        .iter()
        .map(max_score_in_container)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);

    direct_max.max(nested_max)
}

/// Update container metadata (`symbol_count`, `estimated_tokens`) recursively
pub fn update_container_metadata(container: &mut ContainerGroup) {
    // First update nested containers recursively
    for nested in &mut container.nested_containers {
        update_container_metadata(nested);
    }

    // Count symbols (recursive total)
    let direct_symbols = container.symbols.len() as u64;
    let nested_symbols: u64 = container
        .nested_containers
        .iter()
        .map(|n| n.symbol_count)
        .sum();
    container.symbol_count = direct_symbols + nested_symbols;

    // children_count includes BOTH direct symbols AND nested containers
    container.children_count =
        container.symbols.len() as u64 + container.nested_containers.len() as u64;

    // Sum tokens (recursive)
    let direct_tokens: u64 = container.symbols.iter().map(|s| s.estimated_tokens).sum();
    let nested_tokens: u64 = container
        .nested_containers
        .iter()
        .map(|n| n.estimated_tokens)
        .sum();
    container.estimated_tokens = direct_tokens + nested_tokens;

    // Update children_names to include nested container names
    let nested_names: Vec<String> = container
        .nested_containers
        .iter()
        .map(|n| n.name.clone())
        .collect();
    container.children_names.extend(nested_names);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;
    use tempfile::TempDir;

    fn make_args() -> crate::tools::HierarchicalSearchArgs {
        crate::tools::HierarchicalSearchArgs {
            query: "test".to_string(),
            path: ".".to_string(),
            filters: crate::tools::SearchFilters::default(),
            max_results: 100,
            context_lines: 0,
            pagination: crate::tools::PaginationArgs {
                offset: 0,
                size: 100,
            },
            score_min: None,
            auto_merge: false,
            merge_threshold: 256,
            max_files: 20,
            max_containers_per_file: 50,
            max_symbols_per_container: 100,
            max_total_symbols: 2000,
            file_target_tokens: 2000,
            container_target_tokens: 1500,
            symbol_target_tokens: 500,
            context_cluster_target_tokens: 768,
            include_file_context: false,
            include_container_context: false,
            expand_files: vec![],
            budget_rows: None,
        }
    }

    // ===== FileContentCache tests =====

    #[test]
    fn file_content_cache_starts_empty() {
        let cache = FileContentCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn file_content_cache_reads_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let cache = FileContentCache::new();
        let content = cache.get(&file_path).unwrap();
        assert_eq!(content.as_str(), "hello world");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn file_content_cache_returns_cached_on_second_access() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("cached.txt");
        std::fs::write(&file_path, "cached content").unwrap();

        let cache = FileContentCache::new();
        let content1 = cache.get(&file_path).unwrap();
        let content2 = cache.get(&file_path).unwrap();
        // Same Arc
        assert!(Arc::ptr_eq(&content1, &content2));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn file_content_cache_error_on_missing_file() {
        let cache = FileContentCache::new();
        let result = cache.get(Path::new("/nonexistent/path/that/does/not/exist.rs"));
        assert!(result.is_err());
    }

    #[test]
    fn file_content_cache_default_is_empty() {
        let cache = FileContentCache::default();
        assert!(cache.is_empty());
    }

    // ===== ContainerKey tests =====

    #[test]
    fn container_key_from_node_stores_lines() {
        let key = ContainerKey::from_node(10, 50);
        assert_eq!(key.start_line, 10);
        assert_eq!(key.end_line, 50);
    }

    #[test]
    fn container_key_equality() {
        let k1 = ContainerKey::from_node(1, 100);
        let k2 = ContainerKey::from_node(1, 100);
        let k3 = ContainerKey::from_node(2, 100);
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    // ===== update_container_metadata tests =====

    #[test]
    fn update_container_metadata_empty_container() {
        let mut container = ContainerGroup::default();
        update_container_metadata(&mut container);
        assert_eq!(container.symbol_count, 0);
        assert_eq!(container.children_count, 0);
        assert_eq!(container.estimated_tokens, 0);
    }

    #[test]
    fn update_container_metadata_counts_direct_symbols() {
        let mut container = ContainerGroup::default();
        container.symbols.push(HierarchicalSymbol {
            name: "sym1".to_string(),
            estimated_tokens: 50,
            ..HierarchicalSymbol::default()
        });
        container.symbols.push(HierarchicalSymbol {
            name: "sym2".to_string(),
            estimated_tokens: 30,
            ..HierarchicalSymbol::default()
        });

        update_container_metadata(&mut container);
        assert_eq!(container.symbol_count, 2);
        assert_eq!(container.children_count, 2);
        assert_eq!(container.estimated_tokens, 80);
    }

    #[test]
    fn update_container_metadata_recurses_into_nested() {
        let mut inner = ContainerGroup::default();
        inner.symbols.push(HierarchicalSymbol {
            name: "inner_sym".to_string(),
            estimated_tokens: 20,
            ..HierarchicalSymbol::default()
        });

        let mut outer = ContainerGroup::default();
        outer.nested_containers.push(inner);

        update_container_metadata(&mut outer);
        // outer has 0 direct symbols, 1 nested container
        assert_eq!(outer.symbol_count, 1); // recursive: 0 direct + 1 from nested
        assert_eq!(outer.children_count, 1); // 0 symbols + 1 nested container
        assert_eq!(outer.estimated_tokens, 20);
    }

    #[test]
    fn update_container_metadata_includes_nested_names() {
        let mut nested = ContainerGroup {
            name: "NestedClass".to_string(),
            ..ContainerGroup::default()
        };
        nested.symbols.push(HierarchicalSymbol {
            name: "nested_method".to_string(),
            estimated_tokens: 10,
            ..HierarchicalSymbol::default()
        });

        let mut outer = ContainerGroup {
            name: "OuterClass".to_string(),
            ..ContainerGroup::default()
        };
        outer.nested_containers.push(nested);

        update_container_metadata(&mut outer);
        assert!(outer.children_names.contains(&"NestedClass".to_string()));
    }

    // ===== update_nested_depth_and_path tests =====

    #[test]
    fn update_nested_depth_and_path_updates_children() {
        let child = ContainerGroup {
            name: "Child".to_string(),
            depth: 1,
            parent_path: vec![],
            ..ContainerGroup::default()
        };

        let mut root = ContainerGroup {
            name: "Root".to_string(),
            depth: 1,
            parent_path: vec![],
            ..ContainerGroup::default()
        };
        root.nested_containers.push(child.clone());

        update_nested_depth_and_path(&mut root);

        // Root stays at depth=1, its child should be at depth=2
        let child_after = &root.nested_containers[0];
        assert_eq!(child_after.depth, 2);
        assert_eq!(child_after.parent_path, vec!["Root".to_string()]);
        drop(child);
    }

    #[test]
    fn update_nested_depth_and_path_deep_nesting() {
        let grandchild = ContainerGroup {
            name: "Grandchild".to_string(),
            depth: 1,
            ..ContainerGroup::default()
        };
        let mut child = ContainerGroup {
            name: "Child".to_string(),
            depth: 1,
            ..ContainerGroup::default()
        };
        child.nested_containers.push(grandchild);

        let mut root = ContainerGroup {
            name: "Root".to_string(),
            depth: 1,
            ..ContainerGroup::default()
        };
        root.nested_containers.push(child);

        update_nested_depth_and_path(&mut root);

        let child_after = &root.nested_containers[0];
        assert_eq!(child_after.depth, 2);

        let grandchild_after = &child_after.nested_containers[0];
        assert_eq!(grandchild_after.depth, 3);
        assert_eq!(
            grandchild_after.parent_path,
            vec!["Root".to_string(), "Child".to_string()]
        );
    }

    // ===== find_containing_container tests =====

    #[test]
    fn find_containing_container_returns_matching_container() {
        let mut containers = vec![ContainerGroup {
            byte_range: (10, 50),
            ..ContainerGroup::default()
        }];

        let result = find_containing_container(15, 25, &mut containers);
        assert!(result.is_some());
    }

    #[test]
    fn find_containing_container_returns_none_when_no_match() {
        let mut containers = vec![ContainerGroup {
            byte_range: (10, 20),
            ..ContainerGroup::default()
        }];

        // Symbol at lines 30-40 is outside container 10-20
        let result = find_containing_container(30, 40, &mut containers);
        assert!(result.is_none());
    }

    #[test]
    fn find_containing_container_prefers_smallest_container() {
        let mut containers = vec![ContainerGroup {
            name: "outer".to_string(),
            byte_range: (1, 100),
            nested_containers: vec![ContainerGroup {
                name: "inner".to_string(),
                byte_range: (10, 50),
                ..ContainerGroup::default()
            }],
            ..ContainerGroup::default()
        }];

        // Symbol at 15-20 fits in both, should return inner (smallest span)
        let result = find_containing_container(15, 20, &mut containers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "inner");
    }

    #[test]
    fn find_containing_container_returns_outer_when_inner_does_not_match() {
        let mut containers = vec![ContainerGroup {
            name: "outer".to_string(),
            byte_range: (1, 100),
            nested_containers: vec![ContainerGroup {
                name: "inner".to_string(),
                byte_range: (60, 90),
                ..ContainerGroup::default()
            }],
            ..ContainerGroup::default()
        }];

        // Symbol at 10-20 fits outer but not inner
        let result = find_containing_container(10, 20, &mut containers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "outer");
    }

    // ===== max_score_in_container tests =====

    #[test]
    fn max_score_in_container_empty_returns_zero() {
        let container = ContainerGroup::default();
        assert!((max_score_in_container(&container) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn max_score_in_container_returns_highest_score() {
        let mut container = ContainerGroup::default();
        container.symbols.push(HierarchicalSymbol {
            score: 0.8,
            ..HierarchicalSymbol::default()
        });
        container.symbols.push(HierarchicalSymbol {
            score: 0.3,
            ..HierarchicalSymbol::default()
        });
        assert!((max_score_in_container(&container) - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn max_score_in_container_checks_nested_containers() {
        let mut nested = ContainerGroup::default();
        nested.symbols.push(HierarchicalSymbol {
            score: 0.95,
            ..HierarchicalSymbol::default()
        });

        let mut container = ContainerGroup::default();
        container.symbols.push(HierarchicalSymbol {
            score: 0.5,
            ..HierarchicalSymbol::default()
        });
        container.nested_containers.push(nested);

        assert!((max_score_in_container(&container) - 0.95).abs() < f64::EPSILON);
    }

    // ===== sort_containers_by_score tests =====

    #[test]
    fn sort_containers_by_score_higher_score_first() {
        let mut sym_high = HierarchicalSymbol {
            score: 0.9,
            ..HierarchicalSymbol::default()
        };
        sym_high.name = "high".to_string();
        let mut sym_low = HierarchicalSymbol {
            score: 0.2,
            ..HierarchicalSymbol::default()
        };
        sym_low.name = "low".to_string();

        let mut c1 = ContainerGroup {
            name: "low_container".to_string(),
            ..ContainerGroup::default()
        };
        c1.symbols.push(sym_low);

        let mut c2 = ContainerGroup {
            name: "high_container".to_string(),
            ..ContainerGroup::default()
        };
        c2.symbols.push(sym_high);

        let mut containers = vec![c1, c2];
        sort_containers_by_score(&mut containers);

        // high_container should be first
        assert_eq!(containers[0].name, "high_container");
    }

    #[test]
    fn sort_containers_by_score_equal_scores_ordered_by_position() {
        let mut c1 = ContainerGroup {
            name: "z".to_string(),
            byte_range: (1, 10),
            ..ContainerGroup::default()
        };
        c1.symbols.push(HierarchicalSymbol {
            score: 0.5,
            ..HierarchicalSymbol::default()
        });

        let mut c2 = ContainerGroup {
            name: "a".to_string(),
            byte_range: (20, 30),
            ..ContainerGroup::default()
        };
        c2.symbols.push(HierarchicalSymbol {
            score: 0.5,
            ..HierarchicalSymbol::default()
        });

        let mut containers = vec![c2, c1];
        sort_containers_by_score(&mut containers);

        // Same score => ordered by start_line ascending
        assert_eq!(containers[0].byte_range.0, 1);
    }

    // ===== sort_symbols_deterministic tests =====

    #[test]
    fn sort_symbols_deterministic_higher_score_first() {
        use crate::execution::types::{PositionData, RangeData};
        let mut symbols = vec![
            HierarchicalSymbol {
                name: "low".to_string(),
                score: 0.2,
                range: RangeData {
                    start: PositionData {
                        line: 10,
                        character: 0,
                    },
                    end: PositionData {
                        line: 15,
                        character: 0,
                    },
                },
                ..HierarchicalSymbol::default()
            },
            HierarchicalSymbol {
                name: "high".to_string(),
                score: 0.9,
                range: RangeData {
                    start: PositionData {
                        line: 5,
                        character: 0,
                    },
                    end: PositionData {
                        line: 8,
                        character: 0,
                    },
                },
                ..HierarchicalSymbol::default()
            },
        ];
        sort_symbols_deterministic(&mut symbols);
        assert_eq!(symbols[0].name, "high");
    }

    #[test]
    fn sort_symbols_deterministic_same_score_by_line() {
        use crate::execution::types::{PositionData, RangeData};
        let mut symbols = vec![
            HierarchicalSymbol {
                name: "later".to_string(),
                score: 0.5,
                range: RangeData {
                    start: PositionData {
                        line: 20,
                        character: 0,
                    },
                    end: PositionData {
                        line: 25,
                        character: 0,
                    },
                },
                ..HierarchicalSymbol::default()
            },
            HierarchicalSymbol {
                name: "earlier".to_string(),
                score: 0.5,
                range: RangeData {
                    start: PositionData {
                        line: 5,
                        character: 0,
                    },
                    end: PositionData {
                        line: 10,
                        character: 0,
                    },
                },
                ..HierarchicalSymbol::default()
            },
        ];
        sort_symbols_deterministic(&mut symbols);
        assert_eq!(symbols[0].name, "earlier");
    }

    // ===== build_container_tree tests =====

    #[test]
    fn build_container_tree_no_nodes_returns_empty() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = make_args();
        let cache = FileContentCache::new();
        let file_path = Path::new("/workspace/src/lib.rs");

        let (containers, top_level) =
            build_container_tree(&[], &[], &snapshot, ws, &args, &cache, file_path).unwrap();
        assert!(containers.is_empty());
        assert!(top_level.is_empty());
    }

    #[test]
    fn build_container_tree_with_only_non_containers_has_top_level() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("standalone_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = make_args();
        let cache = FileContentCache::new();
        let file_path = Path::new("/workspace/src/lib.rs");

        let matched_nodes = vec![(node_id, 0.9)];
        let all_nodes = vec![node_id];

        let (containers, top_level) = build_container_tree(
            &matched_nodes,
            &all_nodes,
            &snapshot,
            ws,
            &args,
            &cache,
            file_path,
        )
        .unwrap();

        // No containers in the graph => all symbols are top-level
        assert!(containers.is_empty());
        assert_eq!(top_level.len(), 1);
        assert_eq!(top_level[0].name, "standalone_fn");
    }

    #[test]
    fn build_container_tree_with_container_groups_symbols() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();

        // Container: a Class from lines 1-50
        let cls_name = graph.strings_mut().intern("MyClass").unwrap();
        let cls_entry =
            NodeEntry::new(NodeKind::Class, cls_name, file_id).with_location(1, 0, 50, 0);
        let cls_id = graph.nodes_mut().alloc(cls_entry).unwrap();
        graph
            .indices_mut()
            .add(cls_id, NodeKind::Class, cls_name, None, file_id);

        // Method inside the class: lines 10-20
        let meth_name = graph.strings_mut().intern("my_method").unwrap();
        let meth_entry =
            NodeEntry::new(NodeKind::Method, meth_name, file_id).with_location(10, 0, 20, 0);
        let meth_id = graph.nodes_mut().alloc(meth_entry).unwrap();
        graph
            .indices_mut()
            .add(meth_id, NodeKind::Method, meth_name, None, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = make_args();
        let cache = FileContentCache::new();
        let file_path = Path::new("/workspace/src/lib.rs");

        // Matched: the method
        let matched_nodes = vec![(meth_id, 0.8)];
        let all_nodes = vec![cls_id, meth_id];

        let (containers, top_level) = build_container_tree(
            &matched_nodes,
            &all_nodes,
            &snapshot,
            ws,
            &args,
            &cache,
            file_path,
        )
        .unwrap();

        // The method should be inside the class container
        assert!(!containers.is_empty());
        assert!(top_level.is_empty());
        let cls_container = &containers[0];
        assert_eq!(cls_container.symbols.len(), 1);
        assert_eq!(cls_container.symbols[0].name, "my_method");
    }

    // ===== create_container_group tests =====

    #[test]
    fn create_container_group_from_valid_node() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/mod.rs")).unwrap();
        let nm = graph.strings_mut().intern("MyModule").unwrap();
        let entry = NodeEntry::new(NodeKind::Module, nm, file_id).with_location(5, 0, 30, 0);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let group = create_container_group(node_id, &snapshot);
        assert!(!group.name.is_empty());
        assert_eq!(group.depth, 1);
        assert_eq!(group.byte_range.0, 5);
        assert_eq!(group.byte_range.1, 30);
    }

    #[test]
    fn create_container_group_from_invalid_node() {
        use sqry_core::graph::unified::node::NodeId;
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let fake_id = NodeId::new(99999, 0);
        let group = create_container_group(fake_id, &snapshot);
        // Invalid node => empty group
        assert!(group.name.is_empty());
        assert_eq!(group.byte_range, (0, 0));
    }

    // ===== find_immediate_parent_container tests =====

    #[test]
    fn find_immediate_parent_container_no_parent() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("lone").unwrap();
        let entry = NodeEntry::new(NodeKind::Class, nm, file_id).with_location(1, 0, 10, 0);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();

        let snapshot = graph.snapshot();
        let all_containers = vec![node_id];
        let result = find_immediate_parent_container(node_id, &all_containers, &snapshot);
        // Only self in containers => no parent
        assert!(result.is_none());
    }

    #[test]
    fn find_immediate_parent_container_finds_parent() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();

        let outer_nm = graph.strings_mut().intern("Outer").unwrap();
        let outer_entry =
            NodeEntry::new(NodeKind::Class, outer_nm, file_id).with_location(1, 0, 100, 0);
        let outer_id = graph.nodes_mut().alloc(outer_entry).unwrap();

        let inner_nm = graph.strings_mut().intern("Inner").unwrap();
        let inner_entry =
            NodeEntry::new(NodeKind::Class, inner_nm, file_id).with_location(10, 0, 50, 0);
        let inner_id = graph.nodes_mut().alloc(inner_entry).unwrap();

        let snapshot = graph.snapshot();
        let all_containers = vec![outer_id, inner_id];
        let parent = find_immediate_parent_container(inner_id, &all_containers, &snapshot);
        // outer fully contains inner => outer is parent
        assert!(parent.is_some());
        let key = parent.unwrap();
        assert_eq!(key.start_line, 1);
        assert_eq!(key.end_line, 100);
    }

    // ===== collect_containers tests =====

    #[test]
    fn collect_containers_only_container_kinds() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/x.rs")).unwrap();

        let cls_nm = graph.strings_mut().intern("AClass").unwrap();
        let cls_entry = NodeEntry::new(NodeKind::Class, cls_nm, file_id);
        let cls_id = graph.nodes_mut().alloc(cls_entry).unwrap();

        let fn_nm = graph.strings_mut().intern("a_func").unwrap();
        let fn_entry = NodeEntry::new(NodeKind::Function, fn_nm, file_id);
        let fn_id = graph.nodes_mut().alloc(fn_entry).unwrap();

        let snapshot = graph.snapshot();
        let containers = collect_containers(&[cls_id, fn_id], &snapshot);
        // Only Class is a CONTAINER_KIND
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0], cls_id);
    }

    #[test]
    fn collect_containers_all_container_kinds_included() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/x.rs")).unwrap();

        let kinds_and_names = [
            (NodeKind::Class, "AClass"),
            (NodeKind::Struct, "AStruct"),
            (NodeKind::Enum, "AnEnum"),
            (NodeKind::Module, "AModule"),
            (NodeKind::Interface, "AnInterface"),
            (NodeKind::Trait, "ATrait"),
            (NodeKind::Service, "AService"),
            (NodeKind::Component, "AComponent"),
        ];

        let mut all_ids = Vec::new();
        for (kind, name) in &kinds_and_names {
            let nm = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(*kind, nm, file_id);
            let id = graph.nodes_mut().alloc(entry).unwrap();
            all_ids.push(id);
        }

        let snapshot = graph.snapshot();
        let containers = collect_containers(&all_ids, &snapshot);
        assert_eq!(containers.len(), kinds_and_names.len());
    }

    // ===== sort_containers_by_span tests =====

    #[test]
    fn sort_containers_by_span_smallest_first() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/x.rs")).unwrap();

        let nm_large = graph.strings_mut().intern("LargeClass").unwrap();
        let large_entry =
            NodeEntry::new(NodeKind::Class, nm_large, file_id).with_location(1, 0, 100, 0);
        let large_id = graph.nodes_mut().alloc(large_entry).unwrap();

        let nm_small = graph.strings_mut().intern("SmallStruct").unwrap();
        let small_entry =
            NodeEntry::new(NodeKind::Struct, nm_small, file_id).with_location(10, 0, 20, 0);
        let small_id = graph.nodes_mut().alloc(small_entry).unwrap();

        let snapshot = graph.snapshot();
        let mut ids = vec![large_id, small_id];
        sort_containers_by_span(&mut ids, &snapshot);

        // small span first
        assert_eq!(ids[0], small_id);
        assert_eq!(ids[1], large_id);
    }
}
