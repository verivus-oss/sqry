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

use super::{ContainerGroup, HierarchicalSymbol, estimate_tokens, node_kind_to_string};

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
            let name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let qualified_name = entry
                .qualified_name
                .and_then(|sid| strings.resolve(sid))
                .map_or_else(|| name.clone(), |s| s.to_string());
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
    let qualified_name = entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| name.clone(), |s| s.to_string());
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
        name,
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
        score,
        estimated_tokens,
        context,
        signature,
        merged: false,
        original_level: None,
        clustered_count: None,
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
