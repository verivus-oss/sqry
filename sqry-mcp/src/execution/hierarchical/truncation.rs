//! Response size enforcement for hierarchical search
//!
//! Implements truncation and pagination to keep responses within configured limits.

use std::collections::HashSet;

use crate::pagination::encode_cursor;
use crate::tools::HierarchicalSearchArgs;

use super::{ContainerGroup, FileGroup};

/// Enforce response size limits on files
///
/// This function modifies files in place to enforce:
/// - `max_containers_per_file`
/// - `max_symbols_per_container`
/// - `max_total_symbols`
///
/// Files beyond the limit are converted to stubs (metadata only, no symbols)
/// instead of being dropped entirely. Use the `expand_files` parameter to load
/// full details for specific stubs.
///
/// Returns true if any truncation occurred.
pub fn enforce_response_limits(files: &mut [FileGroup], args: &HierarchicalSearchArgs) -> bool {
    let mut truncated = false;
    let mut total_symbols: u64 = 0;
    let mut limit_hit_at_file: Option<usize> = None;

    // Process ALL files and track where limit is hit
    for (file_idx, file) in files.iter_mut().enumerate() {
        // Files after limit was hit become stubs
        if limit_hit_at_file.is_some() {
            convert_to_stub(file);
            truncated = true;
            continue;
        }

        // Enforce max_containers_per_file
        if file.containers.len() > args.max_containers_per_file {
            file.containers.truncate(args.max_containers_per_file);
            truncated = true;
        }

        // Enforce max_symbols_per_container (recursive)
        for container in &mut file.containers {
            if enforce_container_limits(container, args.max_symbols_per_container) {
                truncated = true;
            }
        }

        // Count symbols after per-container truncation
        let file_symbols = count_symbols_in_file(file);
        total_symbols += file_symbols;

        // Check max_total_symbols
        if total_symbols > args.max_total_symbols as u64 {
            // Calculate how many symbols to keep in this file
            let excess = total_symbols - args.max_total_symbols as u64;
            truncate_file_symbols(file, file_symbols.saturating_sub(excess));
            truncated = true;

            // Mark this as the limit file - subsequent files become stubs
            limit_hit_at_file = Some(file_idx);
        }
    }

    // Refresh FileGroup metadata after all truncation
    for file in files.iter_mut() {
        if !file.is_stub {
            refresh_file_metadata(file);
        }
    }

    truncated
}

/// Convert a `FileGroup` to a stub (metadata only, no symbols)
///
/// Preserves: `path`, `language`, `estimated_tokens`, `symbol_count`, `max_score`
/// Clears: `containers`, `top_level_symbols`
/// Sets: `is_stub` = true
fn convert_to_stub(file: &mut FileGroup) {
    // Keep original metadata for client to see what's available
    // symbol_count and estimated_tokens already reflect original values
    file.containers.clear();
    file.top_level_symbols.clear();
    file.is_stub = true;
}

/// Enforce symbol limit on a container (recursive)
fn enforce_container_limits(container: &mut ContainerGroup, max_symbols: usize) -> bool {
    let mut truncated = false;

    // Process nested containers first
    for nested in &mut container.nested_containers {
        if enforce_container_limits(nested, max_symbols) {
            truncated = true;
        }
    }

    // Truncate symbols in this container
    if container.symbols.len() > max_symbols {
        container.symbols.truncate(max_symbols);
        truncated = true;
    }

    truncated
}

/// Count total symbols in a file (recursive through containers)
fn count_symbols_in_file(file: &FileGroup) -> u64 {
    let container_symbols: u64 = file.containers.iter().map(count_symbols_in_container).sum();

    container_symbols + file.top_level_symbols.len() as u64
}

fn count_symbols_in_container(container: &ContainerGroup) -> u64 {
    let nested_symbols: u64 = container
        .nested_containers
        .iter()
        .map(count_symbols_in_container)
        .sum();

    container.symbols.len() as u64 + nested_symbols
}

/// Truncate symbols in a file to the target count
///
/// Uses unique container paths (`Vec<usize>`) to prevent nested container key conflicts.
/// Deterministic tie-breaking with original order.
fn truncate_file_symbols(file: &mut FileGroup, target: u64) {
    // Collect all symbols with their scores and source info
    // Key: (score, is_container_sym, container_path, symbol_idx, original_order)
    let mut all_symbols: Vec<(f64, bool, Vec<usize>, usize, usize)> = Vec::new();
    let mut original_order: usize = 0;

    // Collect container symbols with unique paths
    for (cidx, container) in file.containers.iter().enumerate() {
        let path = vec![cidx];
        collect_container_symbols_v5(container, &path, &mut all_symbols, &mut original_order);
    }

    // Collect top-level symbols
    for (sidx, sym) in file.top_level_symbols.iter().enumerate() {
        all_symbols.push((sym.score, false, vec![], sidx, original_order));
        original_order += 1;
    }

    // Sort by score DESC (highest first), then by original order for tie-breaking
    all_symbols.sort_by(
        |a, b| match b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal) {
            std::cmp::Ordering::Equal => a.4.cmp(&b.4),
            other => other,
        },
    );

    // Keep only target count
    let to_keep: HashSet<(bool, Vec<usize>, usize)> = all_symbols
        .into_iter()
        .take(usize::try_from(target).unwrap_or(usize::MAX))
        .map(|(_, is_container, path, sidx, _)| (is_container, path, sidx))
        .collect();

    // Prune container symbols with unique paths
    for (cidx, container) in file.containers.iter_mut().enumerate() {
        let path = vec![cidx];
        prune_container_symbols_v5(container, &path, &to_keep);
    }

    // Prune top-level symbols
    file.top_level_symbols = file
        .top_level_symbols
        .drain(..)
        .enumerate()
        .filter(|(sidx, _)| to_keep.contains(&(false, vec![], *sidx)))
        .map(|(_, sym)| sym)
        .collect();

    // Refresh container metadata after pruning
    for container in &mut file.containers {
        refresh_container_metadata(container);
    }
}

/// Collect symbols from container and nested containers with unique path
fn collect_container_symbols_v5(
    container: &ContainerGroup,
    container_path: &[usize],
    out: &mut Vec<(f64, bool, Vec<usize>, usize, usize)>,
    original_order: &mut usize,
) {
    let path_key = container_path.to_vec();
    for (sidx, sym) in container.symbols.iter().enumerate() {
        out.push((sym.score, true, path_key.clone(), sidx, *original_order));
        *original_order += 1;
    }

    // Recursively collect from nested containers with extended path
    for (nested_idx, nested) in container.nested_containers.iter().enumerate() {
        let mut nested_path = container_path.to_vec();
        nested_path.push(nested_idx);
        collect_container_symbols_v5(nested, &nested_path, out, original_order);
    }
}

/// Prune symbols from container using unique path
fn prune_container_symbols_v5(
    container: &mut ContainerGroup,
    container_path: &[usize],
    to_keep: &HashSet<(bool, Vec<usize>, usize)>,
) {
    let path_key = container_path.to_vec();
    container.symbols = container
        .symbols
        .drain(..)
        .enumerate()
        .filter(|(sidx, _)| to_keep.contains(&(true, path_key.clone(), *sidx)))
        .map(|(_, sym)| sym)
        .collect();

    // Recursively prune nested containers with extended path
    for (nested_idx, nested) in container.nested_containers.iter_mut().enumerate() {
        let mut nested_path = container_path.to_vec();
        nested_path.push(nested_idx);
        prune_container_symbols_v5(nested, &nested_path, to_keep);
    }
}

/// Refresh container metadata after truncation
///
/// Updates ALL metadata fields to match actual pruned data:
/// - `symbol_count` (recursive total)
/// - `children_count` (symbols + nested containers)
/// - `children_names` (symbol names + nested container names)
/// - `estimated_tokens` (recomputed from actual symbols)
fn refresh_container_metadata(container: &mut ContainerGroup) {
    // First refresh nested containers recursively (bottom-up)
    for nested in &mut container.nested_containers {
        refresh_container_metadata(nested);
    }

    // Update symbol_count to match actual symbols (recursive total)
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

    // children_names includes BOTH symbol names AND nested container names
    let symbol_names: Vec<String> = container.symbols.iter().map(|s| s.name.clone()).collect();
    let nested_names: Vec<String> = container
        .nested_containers
        .iter()
        .map(|n| n.name.clone())
        .collect();
    container.children_names = [symbol_names, nested_names].concat();

    // Recompute estimated_tokens from actual remaining symbols
    let direct_tokens: u64 = container.symbols.iter().map(|s| s.estimated_tokens).sum();
    let nested_tokens: u64 = container
        .nested_containers
        .iter()
        .map(|n| n.estimated_tokens)
        .sum();
    container.estimated_tokens = direct_tokens + nested_tokens;
}

/// Refresh `FileGroup` metadata after truncation
fn refresh_file_metadata(file: &mut FileGroup) {
    // First refresh all containers
    for container in &mut file.containers {
        refresh_container_metadata(container);
    }

    // Recompute file-level symbol_count
    let container_symbols: u64 = file.containers.iter().map(|c| c.symbol_count).sum();
    let top_level_symbols = file.top_level_symbols.len() as u64;
    file.symbol_count = container_symbols + top_level_symbols;

    // Recompute file-level estimated_tokens
    let container_tokens: u64 = file.containers.iter().map(|c| c.estimated_tokens).sum();
    let top_level_tokens: u64 = file
        .top_level_symbols
        .iter()
        .map(|s| s.estimated_tokens)
        .sum();
    file.estimated_tokens = container_tokens + top_level_tokens;
}

/// Paginate files based on pagination args
///
/// Returns (`paginated_files`, `next_page_token`, `truncated`).
pub fn paginate_files(
    files: Vec<FileGroup>,
    args: &HierarchicalSearchArgs,
) -> (Vec<FileGroup>, Option<String>, bool) {
    let total_files = files.len();
    let start_offset = args.pagination.offset;
    let page_size = args.pagination.size.min(args.max_files);

    if start_offset >= total_files {
        return (Vec::new(), None, false);
    }

    let end_offset = (start_offset + page_size).min(total_files);
    let paginated: Vec<FileGroup> = files
        .into_iter()
        .skip(start_offset)
        .take(page_size)
        .collect();

    // Generate next page token if more results exist
    let next_token = if end_offset < total_files {
        Some(encode_cursor(end_offset))
    } else {
        None
    };

    let truncated = next_token.is_some();

    (paginated, next_token, truncated)
}
