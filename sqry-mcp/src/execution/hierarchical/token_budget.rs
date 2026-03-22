//! Token budget enforcement for hierarchical search
//!
//! Implements all token-budget parameters from `HierarchicalSearchArgs`:
//! - `file_target_tokens`: Maximum tokens per file
//! - `container_target_tokens`: Maximum tokens per container
//! - `symbol_target_tokens`: Maximum tokens per symbol context
//! - `context_cluster_target_tokens`: Budget for clustering adjacent symbols
//! - `include_file_context`: Add file-level header context
//! - `include_container_context`: Add container-level code context

use anyhow::Result;

use crate::execution::CodeContext;
use crate::tools::HierarchicalSearchArgs;

use super::{ContainerGroup, FileGroup, HierarchicalSymbol, estimate_tokens};

/// Apply all token budgets to a file after grouping but before response
///
/// Order of operations:
/// 1. Apply `symbol_target_tokens` (trim large symbol contexts)
/// 2. Apply context clustering (`context_cluster_target_tokens`)
/// 3. Apply `container_target_tokens` (truncate oversized containers)
/// 4. Apply `file_target_tokens` (truncate oversized files)
/// 5. Add `include_file_context` / `include_container_context` if requested
pub fn apply_token_budgets(
    file: &mut FileGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
) -> Result<()> {
    // Step 1: Trim oversized symbol contexts
    apply_symbol_target_tokens(file, args);

    // Step 2: Cluster adjacent symbols sharing context
    // CRITICAL FIX (iter3 Finding 2): Pass file_content for actual context merging
    if args.context_cluster_target_tokens > 0 {
        apply_context_clustering(file, file_content, args)?;
    }

    // Step 3: Enforce container token budgets
    apply_container_target_tokens(file, args);

    // Step 4: Enforce file token budget
    apply_file_target_tokens(file, args);

    // Step 5: Add optional context levels
    if args.include_container_context {
        add_container_contexts(file, file_content, args)?;
    }
    if args.include_file_context {
        add_file_context(file, file_content, args)?;
    }

    Ok(())
}

/// Trim symbol contexts that exceed `symbol_target_tokens`.
///
/// When a symbol's context exceeds the target, progressively reduce
/// `context_lines` until under budget or at minimum (0 lines).
fn apply_symbol_target_tokens(file: &mut FileGroup, args: &HierarchicalSearchArgs) {
    let target = args.symbol_target_tokens;

    for container in &mut file.containers {
        apply_symbol_target_to_container(container, target);
    }

    for symbol in &mut file.top_level_symbols {
        trim_symbol_context(symbol, target);
    }
}

fn apply_symbol_target_to_container(container: &mut ContainerGroup, target: u64) {
    for nested in &mut container.nested_containers {
        apply_symbol_target_to_container(nested, target);
    }

    for symbol in &mut container.symbols {
        trim_symbol_context(symbol, target);
    }
}

/// Trim a single symbol's context to fit within token target
fn trim_symbol_context(symbol: &mut HierarchicalSymbol, target: u64) {
    if symbol.estimated_tokens <= target {
        return;
    }

    // Strategy: reduce context by trimming lines from the ends
    if let Some(ctx) = &mut symbol.context {
        let lines: Vec<&str> = ctx.code.lines().collect();

        // Context has lines_before and lines_after relative to symbol
        // Trim from context lines, preserving the symbol's core lines
        let mut trim_before = 0;
        let mut trim_after = 0;
        let max_trim_before = ctx.lines_before;
        let max_trim_after = ctx.lines_after;

        // Iteratively trim until under budget
        loop {
            let start_idx = trim_before;
            let end_idx = lines.len().saturating_sub(trim_after);

            if start_idx >= end_idx {
                break; // Can't trim more
            }

            let trimmed_code: String = lines[start_idx..end_idx].join("\n");
            let new_tokens = estimate_tokens(&trimmed_code);

            if new_tokens <= target {
                ctx.code = trimmed_code;
                ctx.lines_before = ctx.lines_before.saturating_sub(trim_before);
                ctx.lines_after = ctx.lines_after.saturating_sub(trim_after);
                symbol.estimated_tokens = new_tokens;
                break;
            }

            // Alternate trimming from before and after
            if trim_before < max_trim_before {
                trim_before += 1;
            } else if trim_after < max_trim_after {
                trim_after += 1;
            } else {
                break; // Can only keep core symbol lines
            }
        }
    }
}

/// Cluster adjacent symbols that share overlapping context
///
/// When symbols are close together and their combined context is under
/// `context_cluster_target_tokens`, merge them into a single context block.
///
/// CRITICAL FIX (iter3 Finding 2): Complete implementation that actually
/// merges contexts and removes clustered symbols.
fn apply_context_clustering(
    file: &mut FileGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
) -> Result<()> {
    // For each container, cluster adjacent symbols
    for container in &mut file.containers {
        apply_clustering_to_container(container, file_content, args.context_cluster_target_tokens)?;
    }
    Ok(())
}

fn apply_clustering_to_container(
    container: &mut ContainerGroup,
    file_content: &str,
    target: u64,
) -> Result<()> {
    // Process nested containers first (bottom-up)
    for nested in &mut container.nested_containers {
        apply_clustering_to_container(nested, file_content, target)?;
    }

    cluster_container_symbols(container, file_content, target)?;

    Ok(())
}

fn cluster_container_symbols(
    container: &mut ContainerGroup,
    file_content: &str,
    target: u64,
) -> Result<()> {
    // Cluster adjacent symbols in this container
    if container.symbols.len() < 2 {
        return Ok(());
    }

    // Sort by line first for adjacency detection
    container.symbols.sort_by_key(|s| s.range.start.line);

    // CRITICAL FIX (iter3 Finding 2): Actually merge symbols instead of just noting
    let clustered = cluster_symbols(&container.symbols, file_content, target)?;

    // Replace symbols with clustered version
    container.symbols = clustered;

    // Recalculate container estimated_tokens after clustering
    container.estimated_tokens = unmerged_symbol_tokens(&container.symbols)
        + container
            .nested_containers
            .iter()
            .map(|n| n.estimated_tokens)
            .sum::<u64>();

    Ok(())
}

fn cluster_symbols(
    symbols: &[HierarchicalSymbol],
    file_content: &str,
    target: u64,
) -> Result<Vec<HierarchicalSymbol>> {
    let mut clustered = Vec::new();
    let mut i = 0;

    while i < symbols.len() {
        let (cluster_end, _) = find_cluster_end(symbols, i, target);

        if cluster_end > i {
            if let Some(merged) =
                merge_cluster_if_possible(symbols, i, cluster_end, file_content, target)?
            {
                clustered.push(merged);
            } else {
                clustered.extend(symbols[i..=cluster_end].iter().cloned());
            }
        } else {
            clustered.push(symbols[i].clone());
        }

        i = cluster_end + 1;
    }

    Ok(clustered)
}

fn find_cluster_end(symbols: &[HierarchicalSymbol], start_idx: usize, target: u64) -> (usize, u64) {
    let mut cluster_end = start_idx;
    let mut cluster_tokens = symbols[start_idx].estimated_tokens;

    while cluster_end + 1 < symbols.len() {
        let current_symbol = &symbols[cluster_end];
        let next_symbol = &symbols[cluster_end + 1];

        if is_adjacent_symbol(current_symbol, next_symbol) {
            let combined = cluster_tokens + next_symbol.estimated_tokens;
            if combined <= target {
                cluster_tokens = combined;
                cluster_end += 1;
            } else {
                break; // Would exceed budget
            }
        } else {
            break; // Not adjacent
        }
    }

    (cluster_end, cluster_tokens)
}

fn is_adjacent_symbol(
    current_symbol: &HierarchicalSymbol,
    next_symbol: &HierarchicalSymbol,
) -> bool {
    next_symbol.range.start.line <= current_symbol.range.end.line + 5
}

fn merge_cluster_if_possible(
    symbols: &[HierarchicalSymbol],
    cluster_start: usize,
    cluster_end: usize,
    file_content: &str,
    target: u64,
) -> Result<Option<HierarchicalSymbol>> {
    let first = &symbols[cluster_start];
    let last = &symbols[cluster_end];

    // Compute merged context range using symbol ranges
    // First symbol's start line with its context before
    let first_ctx_before = first.context.as_ref().map_or(0, |c| c.lines_before);
    let merged_start_line = (first.range.start.line as usize).saturating_sub(first_ctx_before);
    // Last symbol's end line with its context after
    let last_ctx_after = last.context.as_ref().map_or(0, |c| c.lines_after);
    let merged_end_line = (last.range.end.line as usize) + last_ctx_after;

    // CRITICAL FIX: Actually extract merged content
    let merged_code = extract_line_range_with_trailing(
        file_content,
        merged_start_line.max(1), // Ensure 1-indexed
        merged_end_line,
    )?;
    let merged_tokens = estimate_tokens(&merged_code);

    // CRITICAL FIX (iter4 Finding 3): Post-merge check ensures actual merged
    // tokens don't exceed target. Pre-merge estimates (sum of estimated_tokens)
    // may differ from actual extraction due to gaps, trailing newlines, etc.
    if merged_tokens > target {
        return Ok(None);
    }

    // Create merged symbol (use first symbol as base)
    let mut merged = symbols[cluster_start].clone();
    // Calculate context lines relative to merged range
    let merged_lines_before =
        (first.range.start.line as usize).saturating_sub(merged_start_line.max(1));
    let merged_lines_after = merged_end_line.saturating_sub(last.range.end.line as usize);
    merged.context = Some(CodeContext {
        code: merged_code,
        lines_before: merged_lines_before,
        lines_after: merged_lines_after,
    });
    merged.estimated_tokens = merged_tokens;
    merged.range.end = last.range.end.clone();
    // Mark as clustered with count
    merged.clustered_count =
        Some(u32::try_from(cluster_end - cluster_start + 1).unwrap_or(u32::MAX));

    Ok(Some(merged))
}

/// Truncate containers that exceed `container_target_tokens`.
///
/// Removes lowest-scored symbols until under budget.
fn apply_container_target_tokens(file: &mut FileGroup, args: &HierarchicalSearchArgs) {
    let target = args.container_target_tokens;

    for container in &mut file.containers {
        enforce_container_budget(container, target);
    }
}

fn enforce_container_budget(container: &mut ContainerGroup, target: u64) {
    // First, process nested containers (bottom-up)
    for nested in &mut container.nested_containers {
        enforce_container_budget(nested, target);
    }

    // Calculate current container tokens (sum of all content)
    // CRITICAL FIX (iter3 Finding 1): Initialize estimated_tokens BEFORE while loop
    let current_tokens = container_tokens_with_merged(container);
    container.estimated_tokens = current_tokens;

    if current_tokens <= target {
        return;
    }

    trim_container_contents_to_budget(container, target);
}

fn container_tokens_with_merged(container: &ContainerGroup) -> u64 {
    nested_container_tokens(container)
        + unmerged_symbol_tokens(&container.symbols)
        + merged_container_token_contribution(container)
}

fn nested_container_tokens(container: &ContainerGroup) -> u64 {
    container
        .nested_containers
        .iter()
        .map(|n| n.estimated_tokens)
        .sum()
}

fn unmerged_symbol_tokens(symbols: &[HierarchicalSymbol]) -> u64 {
    symbols
        .iter()
        .filter(|s| !s.merged) // CRITICAL FIX (iter3 Finding 3): Exclude merged
        .map(|s| s.estimated_tokens)
        .sum()
}

fn merged_container_token_contribution(container: &ContainerGroup) -> u64 {
    if container.symbols.iter().any(|s| s.merged) {
        container.merged_container_tokens // Token cost of container code (set by merge phase)
    } else {
        0
    }
}

fn trim_container_contents_to_budget(container: &mut ContainerGroup, target: u64) {
    // Sort symbols by score descending to keep highest-scored
    sort_symbols_by_score_desc(&mut container.symbols);

    // Remove symbols from the end (lowest scored) until under budget
    // Only remove unmerged symbols - merged symbols are tied to container
    container.estimated_tokens = remove_unmerged_symbols_to_budget(
        &mut container.symbols,
        container.estimated_tokens,
        target,
    );

    // CRITICAL FIX (iter4 Finding 2): Also trim nested containers if still over budget
    // When a container's overage is entirely from nested containers, we must remove
    // nested containers to bring the parent under budget.
    if container.estimated_tokens > target && !container.nested_containers.is_empty() {
        // Sort nested containers by max score descending (keep highest-scored)
        sort_containers_by_max_score_desc(&mut container.nested_containers);

        // Remove nested containers from the end (lowest scored) until under budget
        container.estimated_tokens = remove_containers_to_budget(
            &mut container.nested_containers,
            container.estimated_tokens,
            target,
        );
    }

    // CRITICAL FIX (iter5 Finding 3): Update symbol_count after trimming
    // If symbols or nested containers were removed to pay for context, the metadata
    // must be updated to stay consistent with the trimmed contents.
    update_container_symbol_count(container);
}

fn sort_symbols_by_score_desc(symbols: &mut [HierarchicalSymbol]) {
    symbols.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn sort_containers_by_max_score_desc(containers: &mut [ContainerGroup]) {
    containers.sort_by(|a, b| {
        let a_max = container_max_score(a);
        let b_max = container_max_score(b);
        b_max
            .partial_cmp(&a_max)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn container_max_score(container: &ContainerGroup) -> f64 {
    container
        .symbols
        .iter()
        .map(|s| s.score)
        .fold(0.0, f64::max)
}

fn remove_unmerged_symbols_to_budget(
    symbols: &mut Vec<HierarchicalSymbol>,
    mut total: u64,
    target: u64,
) -> u64 {
    while total > target && !symbols.is_empty() {
        if let Some(pos) = symbols.iter().rposition(|s| !s.merged) {
            let removed = symbols.remove(pos);
            total = total.saturating_sub(removed.estimated_tokens);
        } else {
            break; // Only merged symbols remain, can't remove them individually
        }
    }
    total
}

fn remove_containers_to_budget(
    containers: &mut Vec<ContainerGroup>,
    mut total: u64,
    target: u64,
) -> u64 {
    while total > target && !containers.is_empty() {
        if let Some(removed) = containers.pop() {
            total = total.saturating_sub(removed.estimated_tokens);
        }
    }
    total
}

fn update_container_symbol_count(container: &mut ContainerGroup) {
    container.symbol_count = container.symbols.len() as u64
        + container
            .nested_containers
            .iter()
            .map(|n| n.symbol_count)
            .sum::<u64>();
}

/// Truncate files that exceed `file_target_tokens`.
///
/// Removes lowest-scored containers AND top-level symbols until under budget.
///
/// CRITICAL FIX (iter4 Finding 1): Include top-level symbols in trimming,
/// not just containers. Both are sorted by score and removed until budget met.
fn apply_file_target_tokens(file: &mut FileGroup, args: &HierarchicalSearchArgs) {
    let target = args.file_target_tokens;

    // Calculate current file tokens
    let current_tokens = file
        .containers
        .iter()
        .map(|c| c.estimated_tokens)
        .sum::<u64>()
        + file
            .top_level_symbols
            .iter()
            .map(|s| s.estimated_tokens)
            .sum::<u64>();

    if current_tokens <= target {
        file.estimated_tokens = current_tokens;
        return;
    }

    let running_total = trim_file_to_budget(file, target, current_tokens);
    file.estimated_tokens = running_total;
}

fn trim_file_to_budget(file: &mut FileGroup, target: u64, mut total: u64) -> u64 {
    // Sort containers by max score descending (keep highest-scored)
    sort_containers_by_max_score_desc(&mut file.containers);

    // CRITICAL FIX (iter4 Finding 1): Sort top-level symbols by score descending
    sort_symbols_by_score_desc(&mut file.top_level_symbols);

    // Remove containers from the end (lowest scored) until under budget
    total = remove_containers_to_budget(&mut file.containers, total, target);

    // CRITICAL FIX (iter4 Finding 1): Also remove top-level symbols if still over budget
    // This handles files dominated by top-level results (no containers or small containers)
    total = remove_symbols_to_budget(&mut file.top_level_symbols, total, target);

    // Update file metadata
    update_file_symbol_count(file);

    total
}

fn remove_symbols_to_budget(
    symbols: &mut Vec<HierarchicalSymbol>,
    mut total: u64,
    target: u64,
) -> u64 {
    while total > target && !symbols.is_empty() {
        if let Some(removed) = symbols.pop() {
            total = total.saturating_sub(removed.estimated_tokens);
        }
    }
    total
}

fn update_file_symbol_count(file: &mut FileGroup) {
    file.symbol_count = file.containers.iter().map(|c| c.symbol_count).sum::<u64>()
        + file.top_level_symbols.len() as u64;
}

/// Add container-level context when `include_container_context` is true.
///
/// CRITICAL FIX (iter4 Finding 4): Use byte-slicing to preserve trailing newlines
/// and revalidate budgets after adding context.
fn add_container_contexts(
    file: &mut FileGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
) -> Result<()> {
    for container in &mut file.containers {
        add_context_to_container(container, file_content, args.container_target_tokens)?;
    }

    // CRITICAL FIX (iter5 Finding 1): Recompute file.estimated_tokens from containers + top-level
    // BEFORE calling revalidate_file_budget. Container tokens were inflated but file total
    // wasn't updated, causing revalidate_file_budget's early return to miss budget overflow.
    file.estimated_tokens = file
        .containers
        .iter()
        .map(|c| c.estimated_tokens)
        .sum::<u64>()
        + file
            .top_level_symbols
            .iter()
            .map(|s| s.estimated_tokens)
            .sum::<u64>();

    // CRITICAL FIX (iter4 Finding 4): Revalidate file budget after adding container contexts
    // Adding contexts may push file over file_target_tokens
    revalidate_file_budget(file, args);

    Ok(())
}

fn add_context_to_container(
    container: &mut ContainerGroup,
    file_content: &str,
    container_target: u64,
) -> Result<()> {
    // Add context for nested containers first
    for nested in &mut container.nested_containers {
        add_context_to_container(nested, file_content, container_target)?;
    }

    // CRITICAL FIX (iter6 Finding 1): Recompute parent container tokens AFTER nested recursion
    // After the recursive calls above, nested containers now have inflated estimated_tokens
    // (due to their context being added). We must recompute the parent total from:
    // - nested containers (now with context tokens)
    // - unmerged symbols
    // - merged_container_tokens (container code counted once)
    // BEFORE adding our own context, so the budget check uses accurate totals.
    container.estimated_tokens = container_tokens_with_merged(container);

    // CRITICAL FIX (iter4 Finding 4): Use byte-slicing to preserve trailing newlines
    let (start_line, end_line) = container.byte_range;
    let container_code = extract_line_range_with_trailing(file_content, start_line, end_line)?;
    let context_tokens = estimate_tokens(&container_code);

    // Store in explicit container_context field (addresses iter3 Q1)
    container.container_context = Some(container_code);
    container.estimated_tokens += context_tokens;

    // CRITICAL FIX (iter4 Finding 4): Revalidate container budget after adding context
    // If adding context pushes container over budget, trim symbols to compensate
    if container.estimated_tokens > container_target {
        trim_container_contents_to_budget(container, container_target);
    }

    Ok(())
}

/// Add file-level context when `include_file_context` is true.
///
/// CRITICAL FIX (iter4 Finding 4): Use byte-slicing to preserve trailing newlines
/// and revalidate file budget after adding context.
fn add_file_context(
    file: &mut FileGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
) -> Result<()> {
    // Extract first N lines as file header/summary using byte-slicing
    let header_lines = 20; // Could be configurable

    // CRITICAL FIX (iter4 Finding 4): Use extract_line_range_with_trailing for byte fidelity
    let header = extract_line_range_with_trailing(
        file_content,
        1, // Start at line 1 (1-indexed)
        header_lines,
    )?;

    let header_tokens = estimate_tokens(&header);

    // Store in file_context field
    file.file_context = Some(header);
    file.estimated_tokens += header_tokens;

    // CRITICAL FIX (iter4 Finding 4): Revalidate file budget after adding file context
    revalidate_file_budget(file, args);

    Ok(())
}

/// Revalidate file budget after adding optional contexts.
///
/// CRITICAL FIX (iter4 Finding 4): Trim containers/symbols if file exceeds budget
/// after adding `include_file_context` or `include_container_context`.
fn revalidate_file_budget(file: &mut FileGroup, args: &HierarchicalSearchArgs) {
    let target = args.file_target_tokens;

    if file.estimated_tokens <= target {
        return;
    }

    let running_total = trim_file_to_budget(file, target, file.estimated_tokens);
    file.estimated_tokens = running_total;
}

/// Extract line range with trailing newline preservation
///
/// CRITICAL FIX (iter3 Finding 5): Preserves trailing newlines byte-for-byte
/// Uses byte slicing to ensure merged context matches source exactly.
pub fn extract_line_range_with_trailing(
    content: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    // Validation 1: 1-indexed check
    if start_line == 0 {
        anyhow::bail!("start_line must be >= 1 (1-indexed), got 0");
    }

    // Validation 2: Range order check
    if end_line < start_line {
        anyhow::bail!("Invalid line range: end_line ({end_line}) < start_line ({start_line})");
    }

    // CRITICAL FIX: Use byte positions to preserve trailing newlines
    // The .lines() iterator strips line terminators; we need byte slicing
    let mut line_starts: Vec<usize> = vec![0];
    for (i, ch) in content.char_indices() {
        if ch == '\n' {
            line_starts.push(i + 1);
        }
    }

    // Validation 3: Bounds check
    let line_count = line_starts.len();
    if start_line > line_count {
        anyhow::bail!("start_line {start_line} exceeds file length {line_count} lines");
    }

    // Calculate byte range (1-indexed lines to 0-indexed byte positions)
    let start_byte = line_starts[start_line - 1];
    let end_byte = if end_line >= line_count {
        content.len() // Include to end of file
    } else {
        // Include the newline at the end of end_line
        line_starts[end_line]
    };

    // Extract byte slice preserving trailing newlines
    Ok(content[start_byte..end_byte].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_line_range_basic() {
        let content = "line1\nline2\nline3\nline4\n";
        let result = extract_line_range_with_trailing(content, 2, 3).unwrap();
        assert_eq!(result, "line2\nline3\n");
    }

    #[test]
    fn test_extract_line_range_to_end() {
        let content = "line1\nline2\nline3";
        let result = extract_line_range_with_trailing(content, 2, 10).unwrap();
        assert_eq!(result, "line2\nline3");
    }

    #[test]
    fn test_extract_line_range_single_line() {
        let content = "line1\nline2\nline3\n";
        let result = extract_line_range_with_trailing(content, 2, 2).unwrap();
        assert_eq!(result, "line2\n");
    }

    #[test]
    fn test_extract_line_range_invalid_start() {
        let content = "line1\nline2\n";
        let result = extract_line_range_with_trailing(content, 0, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_line_range_invalid_order() {
        let content = "line1\nline2\n";
        let result = extract_line_range_with_trailing(content, 3, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_preserves_trailing_newline() {
        let content = "fn foo() {\n    bar();\n}\n";
        let result = extract_line_range_with_trailing(content, 1, 3).unwrap();
        assert!(result.ends_with('\n'));
        assert_eq!(result, "fn foo() {\n    bar();\n}\n");
    }
}
