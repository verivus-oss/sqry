//! Help heading constants and taxonomy helpers for sqry CLI
//!
//! This module provides a single source of truth for help text organization,
//! ensuring consistent grouping across all commands and subcommands.
//!
//! Tested with clap 4.5.x - heading behavior may change in future versions.

use clap::Command;

// ===== Phase 1 Headings (Core Taxonomy) =====

/// Common options that appear across all commands
pub const COMMON_OPTIONS: &str = "Common Options";

/// Match behavior configuration (case, exact, regex)
pub const MATCH_BEHAVIOUR: &str = "Match Behaviour";

/// Search mode selection (fuzzy, semantic, text)
pub const SEARCH_MODES: &str = "Search Modes";

/// Fuzzy search configuration
pub const SEARCH_MODES_FUZZY: &str = "Search Modes — Fuzzy";

/// Output formatting and presentation
pub const OUTPUT_CONTROL: &str = "Output Control";

/// File filtering and traversal
pub const FILE_FILTERING: &str = "File Filtering & Traversal";

/// Index management operations
///
/// Reserved for Phase 2.2+ command grouping of `index`, `update`, and `cache`.
/// Suppress `dead_code` lint until integrated in root help layout.
#[allow(dead_code)]
pub const INDEX_MANAGEMENT: &str = "Index Management";

// ===== Phase 1.5 Headings (Utilities) =====

/// Utilities grouping for batch and completions
pub const UTILITIES: &str = "Utilities";

/// Batch command input sources
pub const BATCH_INPUTS: &str = "Batch Input";

/// Batch command output targets
pub const BATCH_OUTPUT_TARGETS: &str = "Batch Output Targets";

/// Batch session control and resilience
pub const BATCH_SESSION_CONTROL: &str = "Session Control & Resilience";

/// Completions shell targets
pub const COMPLETIONS_SHELL_TARGETS: &str = "Shell Targets";

// ===== Phase 2.2+ Headings (Core & Advanced Commands) =====

/// Index command input configuration
pub const INDEX_INPUT: &str = "Index Input";

/// Index build configuration options
pub const INDEX_CONFIGURATION: &str = "Index Configuration";

/// Performance tuning for indexing and queries
pub const PERFORMANCE_TUNING: &str = "Performance Tuning";

/// Advanced configuration (cache dirs, etc.)
pub const ADVANCED_CONFIGURATION: &str = "Advanced Configuration";

/// Query input specification
pub const QUERY_INPUT: &str = "Query Input";

/// Performance and debugging options
pub const PERFORMANCE_DEBUGGING: &str = "Performance & Debugging";

/// Search input specification
pub const SEARCH_INPUT: &str = "Search Input";

/// Watch command configuration
pub const WATCH_CONFIGURATION: &str = "Watch Configuration";

/// Update command configuration
pub const UPDATE_CONFIGURATION: &str = "Update Configuration";

/// Repair command options
pub const REPAIR_OPTIONS: &str = "Repair Options";

/// Shell command configuration
pub const SHELL_CONFIGURATION: &str = "Shell Configuration";

// ===== Phase 3 Headings (Specialized Commands) =====

/// Visualization input specification
pub const VISUALIZATION_INPUT: &str = "Visualization Input";

/// Diagram configuration and formatting
pub const DIAGRAM_CONFIGURATION: &str = "Diagram Configuration";

/// Graph traversal control
pub const TRAVERSAL_CONTROL: &str = "Traversal Control";

/// Cache input paths
pub const CACHE_INPUT: &str = "Cache Input";

/// Config command input specification
pub const CONFIG_INPUT: &str = "Config Input";

/// Safety controls for destructive operations
pub const SAFETY_CONTROL: &str = "Safety Control";

/// Workspace input paths
pub const WORKSPACE_INPUT: &str = "Workspace Input";

/// Workspace configuration options
pub const WORKSPACE_CONFIGURATION: &str = "Workspace Configuration";

// ===== Phase 4 Headings (Graph Command) =====

/// Graph command root-level configuration
pub const GRAPH_CONFIGURATION: &str = "Graph Configuration";

// ===== Query Persistence Headings (P2-11/P2-17) =====

/// Alias command input specification
pub const ALIAS_INPUT: &str = "Alias Input";

/// Alias command configuration options
pub const ALIAS_CONFIGURATION: &str = "Alias Configuration";

/// History command input specification
pub const HISTORY_INPUT: &str = "History Input";

/// History command configuration options
pub const HISTORY_CONFIGURATION: &str = "History Configuration";

/// Query persistence options (for --save-as on query/search)
pub const PERSISTENCE_OPTIONS: &str = "Persistence Options";

// ===== Natural Language Headings (P2-18/FR-2026-001) =====

/// Natural language query input
pub const NL_INPUT: &str = "Query Input";

/// Natural language translation configuration
pub const NL_CONFIGURATION: &str = "Translation Configuration";

/// Graph analysis input targets
pub const GRAPH_ANALYSIS_INPUT: &str = "Analysis Input";

/// Graph filtering and constraints
pub const GRAPH_FILTERING: &str = "Filtering & Constraints";

/// Graph analysis options
pub const GRAPH_ANALYSIS_OPTIONS: &str = "Analysis Options";

/// Graph output customization
pub const GRAPH_OUTPUT_OPTIONS: &str = "Output Options";

/// Security limits for query execution (CD Static Analysis)
pub const SECURITY_LIMITS: &str = "Security Limits";

// ===== Local Uses and Insights Headings (FR-2025-023) =====

/// Insights command configuration
pub const INSIGHTS_CONFIGURATION: &str = "Insights Configuration";

/// Insights output options
pub const INSIGHTS_OUTPUT: &str = "Output Options";

// ===== Code Analysis Headings (Duplicates, Cycles, Unused) =====

/// Duplicate detection options
pub const DUPLICATE_OPTIONS: &str = "Duplicate Detection";

/// Cycle detection options
pub const CYCLE_OPTIONS: &str = "Cycle Detection";

/// Unused code detection options
pub const UNUSED_OPTIONS: &str = "Unused Code Detection";

/// Export format options
pub const EXPORT_OPTIONS: &str = "Export Options";

// ===== Helper Functions =====

/// Apply root-level command layout by setting subcommand headings
///
/// This function mutates the root `Command` to assign the `UTILITIES` heading
/// to utility subcommands (batch, completions) while preserving existing
/// search workflow headings.
///
/// # Arguments
/// * `cmd` - The root command (typically from `Cli::command()`)
///
/// # Returns
/// The mutated command with updated subcommand headings
#[must_use]
pub fn apply_root_layout(cmd: Command) -> Command {
    cmd.mut_subcommand("batch", |sub| sub.next_help_heading(UTILITIES))
        .mut_subcommand("completions", |sub| sub.next_help_heading(UTILITIES))
}

/// Normalize command for deterministic help output in tests
///
/// Applies consistent ordering and formatting to make help snapshots
/// reproducible across environments.
///
/// # Arguments
/// * `cmd` - The command to normalize
///
/// # Returns
/// The normalized command
#[must_use]
pub fn normalize(cmd: Command) -> Command {
    // Currently a pass-through; future phases may enforce display_order
    cmd
}
