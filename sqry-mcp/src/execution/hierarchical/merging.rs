//! Auto-merge logic with incremental token tracking
//!
//! Prevents token inflation when multiple symbols merge in same container.
//! When `auto_merge` is enabled and a symbol is below `merge_threshold`, the symbol's
//! context is replaced with the full container code.
//!
//! Key design decisions:
//! - Container code is extracted ONCE per container (not N times per symbol)
//! - Merged symbols share container code, counted ONCE at container level
//! - Merged symbols have `estimated_tokens` = 0 to prevent N× inflation
//! - Running token counter tracks accurate budget for merge gating

use anyhow::{Context, Result};

use crate::execution::CodeContext;
use crate::tools::HierarchicalSearchArgs;

use super::{ContainerGroup, FileGroup, HierarchicalSymbol, estimate_tokens};

/// Track merge state within a container to prevent token inflation
///
/// CRITICAL FIX (iter3 Finding 3 & 4): Track running totals for accurate gating
struct ContainerMergeState {
    /// Running count of merged symbols (for tracking if container code is active)
    merged_count: u32,
    /// Whether container code has been extracted (cached)
    container_code_cached: bool,
    /// Cached container code (extracted once, reused for all merges)
    container_code: Option<String>,
    /// Token cost of container code (calculated once)
    container_code_tokens: u64,
    /// Running total of container tokens for accurate merge gating
    /// CRITICAL FIX (iter3 Finding 4): Use running counter instead of stale field
    running_container_tokens: u64,
}

impl ContainerMergeState {
    fn new(initial_container_tokens: u64) -> Self {
        Self {
            merged_count: 0,
            container_code_cached: false,
            container_code: None,
            container_code_tokens: 0,
            // CRITICAL FIX (iter3 Finding 4): Initialize with current container tokens
            running_container_tokens: initial_container_tokens,
        }
    }
}

/// Apply auto-merge to all eligible symbols in a file
///
/// Error handling: Propagates all errors instead of silent skip
pub fn apply_auto_merge(
    file: &mut FileGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
) -> Result<()> {
    if !args.auto_merge {
        return Ok(());
    }

    for container in &mut file.containers {
        // CRITICAL FIX (iter3 Finding 4): Calculate initial tokens for running counter
        let initial_tokens: u64 = container
            .symbols
            .iter()
            .map(|s| s.estimated_tokens)
            .sum::<u64>()
            + container
                .nested_containers
                .iter()
                .map(|n| n.estimated_tokens)
                .sum::<u64>();

        let mut state = ContainerMergeState::new(initial_tokens);
        apply_merge_to_container(container, file_content, args, &mut state)?;
    }

    // Update token totals after all merges
    update_file_tokens(file);

    Ok(())
}

/// Recursively apply merge with incremental token tracking
fn apply_merge_to_container(
    container: &mut ContainerGroup,
    file_content: &str,
    args: &HierarchicalSearchArgs,
    state: &mut ContainerMergeState,
) -> Result<()> {
    // Process nested containers first (each gets own state)
    for nested in &mut container.nested_containers {
        // CRITICAL FIX (iter3 Finding 4): Calculate initial tokens for nested containers
        let nested_initial: u64 = nested
            .symbols
            .iter()
            .map(|s| s.estimated_tokens)
            .sum::<u64>()
            + nested
                .nested_containers
                .iter()
                .map(|n| n.estimated_tokens)
                .sum::<u64>();
        let mut nested_state = ContainerMergeState::new(nested_initial);
        apply_merge_to_container(nested, file_content, args, &mut nested_state)?;
    }

    // Lazily extract container code (once per container, not per symbol)
    let (start_line, end_line) = container.byte_range;
    if !state.container_code_cached {
        let code = extract_line_range_validated(file_content, start_line, end_line).with_context(
            || {
                format!(
                    "Failed to extract container code for '{name}' (lines {start_line}-{end_line})",
                    name = container.name.as_str(),
                    start_line = start_line,
                    end_line = end_line
                )
            },
        )?;
        let tokens = estimate_tokens(&code);
        state.container_code = Some(code);
        state.container_code_tokens = tokens;
        state.container_code_cached = true;
    }

    // Process symbols with incremental budget tracking
    // Clone the container code to avoid borrow checker issues
    let container_code = state.container_code.clone().unwrap();

    for symbol in &mut container.symbols {
        if should_merge_incremental(symbol, args, state) {
            merge_with_parent_incremental(symbol, &container_code, start_line, end_line, state);
        }
    }

    // Update container tokens to reflect actual merged content
    // NOT N × container_tokens, but container_tokens + unmerged symbols
    let unmerged_tokens: u64 = container
        .symbols
        .iter()
        .filter(|s| !s.merged)
        .map(|s| s.estimated_tokens)
        .sum();

    // Merged symbols share the container code, counted ONCE
    let merged_count = container.symbols.iter().filter(|s| s.merged).count();
    let merged_contribution = if merged_count > 0 {
        state.container_code_tokens // Container code counted once, not N times
    } else {
        0
    };

    // Store the merged container token cost for budget calculations
    container.merged_container_tokens = if merged_count > 0 {
        state.container_code_tokens
    } else {
        0
    };

    container.estimated_tokens = merged_contribution
        + unmerged_tokens
        + container
            .nested_containers
            .iter()
            .map(|n| n.estimated_tokens)
            .sum::<u64>();

    Ok(())
}

/// Check if symbol should merge with INCREMENTAL budget awareness
///
/// CRITICAL FIX (iter3 Finding 4): Use running counter instead of stale `container.estimated_tokens`
fn should_merge_incremental(
    symbol: &HierarchicalSymbol,
    args: &HierarchicalSearchArgs,
    state: &ContainerMergeState,
) -> bool {
    // Already merged
    if symbol.merged {
        return false;
    }

    // Condition 1: Symbol must be below merge threshold
    if symbol.estimated_tokens >= args.merge_threshold as u64 {
        return false;
    }

    // Condition 2: Calculate if merge would exceed budget using RUNNING counter
    // CRITICAL FIX (iter3 Finding 4): Use state.running_container_tokens not container.estimated_tokens

    // Calculate what the budget would be after this merge:
    // - Remove symbol's original tokens (symbol context goes away)
    // - If first merge, ADD container code tokens once
    // - Container code is NOT added again for subsequent merges

    let budget_after_merge = if state.merged_count == 0 {
        // First merge: swap symbol tokens for container code tokens
        state
            .running_container_tokens
            .saturating_sub(symbol.estimated_tokens)
            + state.container_code_tokens
    } else {
        // Subsequent merges: just remove symbol's tokens (container code already accounted for)
        state
            .running_container_tokens
            .saturating_sub(symbol.estimated_tokens)
    };

    budget_after_merge <= args.container_target_tokens
}

/// Perform merge with incremental state update
///
/// CRITICAL FIX (iter3 Finding 3): Merged symbols have `estimated_tokens=0`
/// The container code is counted ONCE at the container level, not per-symbol.
fn merge_with_parent_incremental(
    symbol: &mut HierarchicalSymbol,
    container_code: &str,
    start_line: usize,
    end_line: usize,
    state: &mut ContainerMergeState,
) {
    // Store original token count for metadata
    let original_tokens = symbol.estimated_tokens;

    // Calculate context lines relative to symbol position
    let symbol_start = symbol.range.start.line as usize;
    let symbol_end = symbol.range.end.line as usize;
    let lines_before = symbol_start.saturating_sub(start_line);
    let lines_after = end_line.saturating_sub(symbol_end);

    // Replace symbol context with container code
    symbol.context = Some(CodeContext {
        code: container_code.to_string(),
        lines_before,
        lines_after,
    });

    // CRITICAL FIX (iter3 Finding 3): Merged symbol's estimated_tokens = 0
    // NOT container_code_tokens! The container code is counted ONCE at container
    // level by checking if merged_count > 0, not per-symbol.
    // This prevents N× inflation when multiple symbols merge.
    symbol.estimated_tokens = 0;

    // Mark as merged
    symbol.merged = true;
    symbol.original_level = Some(format!("symbol:{original_tokens}"));

    // CRITICAL FIX (iter3 Finding 4): Update running counter for accurate gating
    // When first symbol merges: remove its tokens, add container code ONCE
    // When subsequent symbols merge: just remove their tokens
    if state.merged_count == 0 {
        // First merge: swap symbol tokens for container code
        state.running_container_tokens = state
            .running_container_tokens
            .saturating_sub(original_tokens)
            + state.container_code_tokens;
    } else {
        // Subsequent merges: just remove symbol's tokens
        state.running_container_tokens = state
            .running_container_tokens
            .saturating_sub(original_tokens);
    }

    // Increment merged count
    state.merged_count += 1;
}
/// Extract line range with full validation (Finding 6 fix)
fn extract_line_range_validated(
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

    // Get lines preserving structure
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = start_line.saturating_sub(1);
    let end_idx = end_line.min(lines.len());

    // Validation 3: Bounds check
    if start_idx >= lines.len() {
        anyhow::bail!(
            "start_line {start_line} exceeds file length {line_count} lines",
            line_count = lines.len()
        );
    }

    // Build result preserving line structure
    // Note: .lines() strips \n, we rejoin with \n
    // Original newlines at line ends are normalized to \n
    Ok(lines[start_idx..end_idx].join("\n"))
}

/// Update file tokens after all merges
fn update_file_tokens(file: &mut FileGroup) {
    let container_tokens: u64 = file.containers.iter().map(|c| c.estimated_tokens).sum();
    let top_level_tokens: u64 = file
        .top_level_symbols
        .iter()
        .map(|s| s.estimated_tokens)
        .sum();
    file.estimated_tokens = container_tokens + top_level_tokens;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{PositionData, RangeData};
    use crate::tools::{PaginationArgs, SearchFilters};

    fn make_symbol(
        name: &str,
        tokens: u64,
        score: f64,
        start_line: u32,
        end_line: u32,
    ) -> HierarchicalSymbol {
        HierarchicalSymbol {
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: "function".to_string(),
            range: RangeData {
                start: PositionData {
                    line: start_line,
                    character: 0,
                },
                end: PositionData {
                    line: end_line,
                    character: 0,
                },
            },
            score,
            estimated_tokens: tokens,
            context: Some(CodeContext {
                code: "fn test() {}".to_string(),
                lines_before: 1,
                lines_after: 1,
            }),
            signature: None,
            merged: false,
            original_level: None,
            clustered_count: None,
            macro_metadata: None,
        }
    }

    fn make_container(
        name: &str,
        symbols: Vec<HierarchicalSymbol>,
        byte_range: (usize, usize),
    ) -> ContainerGroup {
        let symbol_count = symbols.len() as u64;
        let estimated_tokens: u64 = symbols.iter().map(|s| s.estimated_tokens).sum();
        ContainerGroup {
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: "impl".to_string(),
            estimated_tokens,
            depth: 1,
            parent_path: Vec::new(),
            byte_range,
            symbols,
            nested_containers: Vec::new(),
            symbol_count,
            children_count: symbol_count,
            children_names: Vec::new(),
            container_context: None,
            merged_container_tokens: 0,
        }
    }

    fn make_test_args(
        merge_threshold: usize,
        container_target_tokens: u64,
    ) -> HierarchicalSearchArgs {
        HierarchicalSearchArgs {
            query: "test".to_string(),
            path: ".".to_string(),
            max_results: 100,
            max_total_symbols: 500,
            max_files: 20,
            max_containers_per_file: 50,
            max_symbols_per_container: 100,
            context_lines: 3,
            score_min: None,
            filters: SearchFilters {
                languages: Vec::new(),
                kinds: Vec::new(),
                visibility: None,
                min_score: None,
            },
            pagination: PaginationArgs {
                offset: 0,
                size: 10,
            },
            expand_files: Vec::new(),
            file_target_tokens: 2000,
            container_target_tokens,
            symbol_target_tokens: 500,
            context_cluster_target_tokens: 768,
            merge_threshold,
            auto_merge: true,
            include_file_context: false,
            include_container_context: false,
            budget_rows: None,
        }
    }

    #[test]
    fn test_merge_state_initialization() {
        let state = ContainerMergeState::new(500);
        assert_eq!(state.merged_count, 0);
        assert_eq!(state.running_container_tokens, 500);
        assert!(!state.container_code_cached);
    }

    #[test]
    fn test_should_merge_below_threshold() {
        let symbol = make_symbol("test", 100, 0.9, 1, 5);
        let args = make_test_args(256, 1500);
        let state = ContainerMergeState {
            merged_count: 0,
            container_code_cached: true,
            container_code: Some("impl Test {}".to_string()),
            container_code_tokens: 200,
            running_container_tokens: 500,
        };

        assert!(should_merge_incremental(&symbol, &args, &state));
    }

    #[test]
    fn test_should_merge_above_threshold() {
        let symbol = make_symbol("test", 300, 0.9, 1, 5);
        let args = make_test_args(256, 1500);
        let state = ContainerMergeState::new(500);

        assert!(!should_merge_incremental(&symbol, &args, &state));
    }

    #[test]
    fn test_should_merge_would_exceed_budget() {
        let symbol = make_symbol("test", 100, 0.9, 1, 5);
        let args = make_test_args(256, 500); // Low budget
        let state = ContainerMergeState {
            merged_count: 0,
            container_code_cached: true,
            container_code: Some("impl Test {}".to_string()),
            container_code_tokens: 600, // Would exceed budget
            running_container_tokens: 400,
        };

        assert!(!should_merge_incremental(&symbol, &args, &state));
    }

    #[test]
    fn test_extract_line_range_validated() {
        let content = "line1\nline2\nline3\nline4\n";
        let result = extract_line_range_validated(content, 2, 3).unwrap();
        assert_eq!(result, "line2\nline3");
    }

    #[test]
    fn test_extract_line_range_invalid_start() {
        let content = "line1\nline2\n";
        let result = extract_line_range_validated(content, 0, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_line_range_invalid_order() {
        let content = "line1\nline2\n";
        let result = extract_line_range_validated(content, 3, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_prevents_n_times_inflation() {
        // Create container with 3 small symbols
        let symbols = vec![
            make_symbol("fn1", 50, 0.9, 2, 4),
            make_symbol("fn2", 50, 0.8, 5, 7),
            make_symbol("fn3", 50, 0.7, 8, 10),
        ];
        let mut container = make_container("Test", symbols, (1, 11));

        let file_content = "impl Test {\n    fn fn1() {}\n    fn fn2() {}\n    fn fn3() {}\n}\n";
        let args = make_test_args(256, 1500);

        let initial_tokens: u64 = container.symbols.iter().map(|s| s.estimated_tokens).sum();
        let mut state = ContainerMergeState::new(initial_tokens);

        apply_merge_to_container(&mut container, file_content, &args, &mut state).unwrap();

        // All 3 symbols should be merged
        assert_eq!(container.symbols.iter().filter(|s| s.merged).count(), 3);

        // All merged symbols should have estimated_tokens = 0
        for symbol in &container.symbols {
            if symbol.merged {
                assert_eq!(symbol.estimated_tokens, 0);
            }
        }

        // Container should have merged_container_tokens set
        assert!(container.merged_container_tokens > 0);

        // Container estimated_tokens should NOT be 3 × container_code_tokens
        // It should be container_code_tokens (once) + 0 (merged symbols)
        assert_eq!(
            container.estimated_tokens,
            container.merged_container_tokens
        );
    }
}
