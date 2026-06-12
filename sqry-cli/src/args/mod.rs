//! Command-line argument parsing for sqry

pub mod headings;
mod sort;

use crate::output;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
pub use sort::SortField;
use sqry_lsp::LspOptions;
use std::path::PathBuf;

/// sqry - Semantic Query for Code
///
/// Search code by what it means, not just what it says.
/// Uses AST analysis to find functions, classes, and symbols with precision.
#[derive(Parser, Debug)]
#[command(
    name = "sqry",
    version,
    about = "Semantic code search tool",
    long_about = "sqry is a semantic code search tool that understands code structure through AST analysis.\n\
                  Find functions, classes, and symbols with precision using AST-aware queries.\n\n\
                  Search progress:\n  \
                  sqry <pattern> --verbose emits snapshot-load, lookup, and filter timing to stderr.\n  \
                  SQRY_LOG=info enables the same progress output without the flag.\n  \
                  Tier-1 search still loads in-process; when sqryd answers daemon/status, verbose mode notes that daemon-backed search is not yet attached.\n  \
                  Tier-2 daemon-backed search will attach to sqryd when reachable and fall through to in-process load otherwise.\n\n\
                  Examples:\n  \
                  sqry main              # Search for 'main' in current directory\n  \
                  sqry test src/         # Search for 'test' in src/\n  \
                  sqry --kind function .  # Find all functions\n  \
                  sqry --json main       # Output as JSON\n  \
                  sqry --csv --headers main  # Output as CSV with headers\n  \
                  sqry --preview main    # Show code context around matches",
    group = ArgGroup::new("output_format").args(["json", "csv", "tsv"]),
    verbatim_doc_comment
)]
// CLI flags are intentionally modeled as independent booleans for clarity.
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    /// Subcommand (optional - defaults to search if pattern provided)
    #[command(subcommand)]
    pub command: Option<Box<Command>>,

    /// Search pattern (shorthand for 'search' command)
    ///
    /// Treated as a regex by default. Invalid regex returns an error.
    /// Use `--exact` for literal matching against interned symbol names
    /// (same contract as the planner's `name:<literal>` predicate; native
    /// dot- and Ruby-`#` qualified display names also check graph-canonical
    /// `::`, and glob meta is matched as literal characters).
    ///
    /// This shorthand routes to `sqry search` (regex / literal). It does
    /// **not** accept planner predicates like `kind:function` or
    /// `name~=/regex/`. For anything beyond a single pattern, use
    /// `sqry search` (regex) or `sqry query` (structural planner).
    /// On workspaces with >50k symbols, prefer the explicit subcommand
    /// so you can scope with `--kind` / `--lang` / `path:` — see
    /// `docs/cli/scaling-large-codebases.md`.
    #[arg(required = false)]
    pub pattern: Option<String>,

    /// Search path (defaults to current directory)
    #[arg(required = false)]
    pub path: Option<String>,

    /// Output format as JSON
    #[arg(long, short = 'j', global = true, hide_long_help = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 10)]
    pub json: bool,

    /// Output format as CSV (comma-separated values)
    ///
    /// RFC 4180 compliant CSV output. Use with --headers to include column names.
    /// By default, formula-triggering characters are prefixed with single quote
    /// for Excel/LibreOffice safety. Use --raw-csv to disable this protection.
    #[arg(long, global = true, hide_long_help = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 12)]
    pub csv: bool,

    /// Output format as TSV (tab-separated values)
    ///
    /// Tab-delimited output for easy Unix pipeline processing.
    /// Newlines and tabs in field values are replaced with spaces.
    #[arg(long, global = true, hide_long_help = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 13)]
    pub tsv: bool,

    /// Include header row in CSV/TSV output
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::OUTPUT_CONTROL, display_order = 11)]
    pub headers: bool,

    /// Columns to include in CSV/TSV output (comma-separated)
    ///
    /// Available columns: `name`, `qualified_name`, `kind`, `file`, `line`, `column`,
    /// `end_line`, `end_column`, `language`, `preview`
    ///
    /// Example: --columns name,file,line
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, hide_long_help = true, value_name = "COLUMNS", help_heading = headings::OUTPUT_CONTROL, display_order = 12)]
    pub columns: Option<String>,

    /// Output raw CSV without formula injection protection
    ///
    /// By default, values starting with =, +, -, @, tab, or carriage return
    /// are prefixed with single quote to prevent Excel/LibreOffice formula
    /// injection attacks. Use this flag to disable protection for programmatic
    /// processing where raw values are needed.
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::OUTPUT_CONTROL, display_order = 13)]
    pub raw_csv: bool,

    /// Show code context around matches (number of lines before/after)
    #[arg(
        long, short = 'p', global = true, hide_long_help = true, value_name = "LINES",
        default_missing_value = "3", num_args = 0..=1,
        help_heading = headings::OUTPUT_CONTROL, display_order = 14,
        long_help = "Show code context around matches (number of lines before/after)\n\n\
                     Displays source code context around each match. Use -p or --preview\n\
                     for default 3 lines, or specify a number like --preview 5.\n\
                     Use --preview 0 to show only the matched line without context.\n\n\
                     Examples:\n  \
                     sqry --preview main      # 3 lines context (default)\n  \
                     sqry -p main             # Same as above\n  \
                     sqry --preview 5 main    # 5 lines context\n  \
                     sqry --preview 0 main    # No context, just matched line"
    )]
    pub preview: Option<usize>,

    /// Disable colored output
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::COMMON_OPTIONS, display_order = 14)]
    pub no_color: bool,

    /// Select output color theme (default, dark, light, none)
    #[arg(
        long,
        value_enum,
        default_value = "default",
        global = true,
        hide_long_help = true,
        help_heading = headings::COMMON_OPTIONS,
        display_order = 15
    )]
    pub theme: crate::output::ThemeName,

    /// Sort results (opt-in)
    #[arg(
        long,
        value_enum,
        global = true,
        hide_long_help = true,
        help_heading = headings::OUTPUT_CONTROL,
        display_order = 16
    )]
    pub sort: Option<SortField>,

    // ===== Pager Flags (P2-29) =====
    /// Enable pager for output (auto-detected by default)
    ///
    /// Forces output to be piped through a pager (like `less`).
    /// In auto mode (default), paging is enabled when:
    /// - Output exceeds terminal height
    /// - stdout is connected to an interactive terminal
    #[arg(
        long,
        global = true,
        hide_long_help = true,
        conflicts_with = "no_pager",
        help_heading = headings::OUTPUT_CONTROL,
        display_order = 17
    )]
    pub pager: bool,

    /// Disable pager (write directly to stdout)
    ///
    /// Disables auto-paging, writing all output directly to stdout.
    /// Useful for scripting or piping to other commands.
    #[arg(
        long,
        global = true,
        hide_long_help = true,
        conflicts_with = "pager",
        help_heading = headings::OUTPUT_CONTROL,
        display_order = 18
    )]
    pub no_pager: bool,

    /// Custom pager command (overrides `$SQRY_PAGER` and `$PAGER`)
    ///
    /// Specify a custom pager command. Supports quoted arguments.
    /// Examples:
    ///   --pager-cmd "less -R"
    ///   --pager-cmd "bat --style=plain"
    ///   --pager-cmd "more"
    #[arg(
        long,
        value_name = "COMMAND",
        global = true,
        hide_long_help = true,
        help_heading = headings::OUTPUT_CONTROL,
        display_order = 19
    )]
    pub pager_cmd: Option<String>,

    /// Filter by symbol type (function, class, struct, etc.)
    ///
    /// Applies to search mode (top-level shorthand and `sqry search`).
    /// For structured queries, use `sqry query "kind:function AND ..."` instead.
    #[arg(long, short = 'k', value_enum, help_heading = headings::MATCH_BEHAVIOUR, display_order = 20)]
    pub kind: Option<SymbolKind>,

    /// Filter by programming language
    ///
    /// Applies to search mode (top-level shorthand and `sqry search`).
    /// For structured queries, use `sqry query "lang:rust AND ..."` instead.
    #[arg(long, short = 'l', help_heading = headings::MATCH_BEHAVIOUR, display_order = 21)]
    pub lang: Option<String>,

    /// Case-insensitive search
    #[arg(long, short = 'i', help_heading = headings::MATCH_BEHAVIOUR, display_order = 11)]
    pub ignore_case: bool,

    /// Exact (literal-only) match against interned symbol name
    /// (disables regex).
    ///
    /// Applies to search mode (top-level shorthand and `sqry search`).
    /// Contract-bound to the structural query planner's
    /// `name:<literal>` predicate (`B1_ALIGN`): `sqry --exact NeedTags .`
    /// and `sqry query 'name:NeedTags' .` return identical sets — both
    /// look up the pattern against `entry.name` / `entry.qualified_name`
    /// byte-for-byte and also check dot- and Ruby-`#` qualified display form
    /// as graph-canonical `::`. Synthetic placeholder nodes are excluded.
    /// `--exact` does not accept glob meta
    /// (`*`, `?`, `[`); they are matched as literal characters. For glob
    /// matching against names use `sqry query 'name:parse_*'` instead. For
    /// regex matching, omit `--exact` and `sqry search` will treat the
    /// pattern as a regex over interned strings.
    #[arg(long, short = 'x', help_heading = headings::MATCH_BEHAVIOUR, display_order = 10)]
    pub exact: bool,

    /// Show count only (number of matches)
    #[arg(long, short = 'c', help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
    pub count: bool,

    /// Maximum directory depth to search
    #[arg(long, default_value = "32", help_heading = headings::FILE_FILTERING, display_order = 20)]
    pub max_depth: usize,

    /// Include hidden files and directories
    #[arg(long, help_heading = headings::FILE_FILTERING, display_order = 10)]
    pub hidden: bool,

    /// Follow symlinks
    #[arg(long, help_heading = headings::FILE_FILTERING, display_order = 11)]
    pub follow: bool,

    /// Enable fuzzy search (requires index)
    ///
    /// Applies to search mode (top-level shorthand and `sqry search`).
    #[arg(long, help_heading = headings::SEARCH_MODES_FUZZY, display_order = 20)]
    pub fuzzy: bool,

    /// Fuzzy matching algorithm (jaro-winkler or levenshtein)
    #[arg(long, default_value = "jaro-winkler", value_name = "ALGORITHM", help_heading = headings::SEARCH_MODES_FUZZY, display_order = 30)]
    pub fuzzy_algorithm: String,

    /// Minimum similarity score for fuzzy matches (0.0-1.0)
    #[arg(long, default_value = "0.6", value_name = "SCORE", help_heading = headings::SEARCH_MODES_FUZZY, display_order = 31)]
    pub fuzzy_threshold: f64,

    /// Maximum number of fuzzy candidates to consider
    #[arg(long, default_value = "1000", value_name = "COUNT", help_heading = headings::SEARCH_MODES_FUZZY, display_order = 40)]
    pub fuzzy_max_candidates: usize,

    /// Enable JSON streaming mode for fuzzy search
    ///
    /// Emits results as JSON-lines (newline-delimited JSON).
    /// Each line is a `StreamEvent` with either a partial result or final summary.
    /// Requires --fuzzy (fuzzy search) and is inherently JSON output.
    #[arg(long, requires = "fuzzy", help_heading = headings::SEARCH_MODES_FUZZY, display_order = 51)]
    pub json_stream: bool,

    /// Allow fuzzy matching for query field names (opt-in).
    /// Applies typo correction to field names (e.g., "knd" → "kind").
    /// Ambiguous corrections are rejected with an error.
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::SEARCH_MODES_FUZZY, display_order = 52)]
    pub fuzzy_fields: bool,

    /// Maximum edit distance for fuzzy field correction
    #[arg(
        long,
        default_value_t = 2,
        global = true,
        hide_long_help = true,
        help_heading = headings::SEARCH_MODES_FUZZY,
        display_order = 53
    )]
    pub fuzzy_field_distance: usize,

    /// Maximum number of results to return
    ///
    /// Limits the output to a manageable size for downstream consumers.
    /// Defaults: search=100, query=1000, fuzzy=50
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::OUTPUT_CONTROL, display_order = 20)]
    pub limit: Option<usize>,

    /// List enabled languages and exit
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::COMMON_OPTIONS, display_order = 30)]
    pub list_languages: bool,

    /// Print cache telemetry to stderr after the command completes
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::COMMON_OPTIONS, display_order = 40)]
    pub debug_cache: bool,

    /// Operate against a logical workspace defined by a `.sqry-workspace` or
    /// `.code-workspace` file (`STEP_8`).
    ///
    /// When set, every subcommand resolves its target through the
    /// `LogicalWorkspace` referenced by `<PATH>`. Path-scoped subcommands
    /// (`sqry index <PATH>`, `sqry query <PATH> …`) still take their explicit
    /// positional argument first; this flag is the fallback when no positional
    /// is provided.
    ///
    /// The `SQRY_WORKSPACE_FILE` environment variable resolves identically;
    /// when both are present, the explicit `--workspace` flag wins.
    ///
    /// Conflicts with the `sqry workspace …` subcommand (which has its own
    /// positional `<workspace>` argument): combining them is a hard error,
    /// raised by `main.rs` at dispatch time. The clap `id` is namespaced as
    /// `global_workspace_path` so it does not collide with the `workspace`
    /// positional that lives on each `WorkspaceCommand` variant.
    #[arg(
        id = "global_workspace_path",
        long = "workspace",
        global = true,
        hide_long_help = true,
        value_name = "PATH",
        env = "SQRY_WORKSPACE_FILE",
        help_heading = headings::COMMON_OPTIONS,
        display_order = 41
    )]
    pub workspace: Option<PathBuf>,

    /// Display fully qualified symbol names in CLI output.
    ///
    /// Helpful for disambiguating relation queries (callers/callees) where
    /// multiple namespaces define the same method name.
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::OUTPUT_CONTROL, display_order = 30)]
    pub qualified_names: bool,

    // ===== Index Validation Flags (P1-14) =====
    /// Index validation strictness level (off, warn, fail)
    ///
    /// Controls how to handle index corruption during load:
    /// - off: Skip validation entirely (fastest)
    /// - warn: Log warnings but continue (default)
    /// - fail: Abort on validation errors
    #[arg(long, value_enum, default_value = "warn", global = true, hide_long_help = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 40)]
    pub validate: ValidationMode,

    /// Automatically rebuild index if validation fails
    ///
    /// When set, if index validation fails in strict mode, sqry will
    /// automatically rebuild the index once and retry. Useful for
    /// recovering from transient corruption without manual intervention.
    ///
    /// Requires `--validate` to be set to either `warn` or `fail`; the
    /// rebuild is only triggered when validation actually evaluates the
    /// index. With `--validate off` the flag is a no-op.
    #[arg(long, global = true, hide_long_help = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 41)]
    pub auto_rebuild: bool,

    /// Maximum ratio of dangling references before rebuild (0.0-1.0)
    ///
    /// Sets the threshold for dangling reference errors during validation.
    /// Default: 0.05 (5%). If more than this ratio of symbols have dangling
    /// references, validation will fail in strict mode.
    #[arg(long, value_name = "RATIO", global = true, hide_long_help = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 42)]
    pub threshold_dangling_refs: Option<f64>,

    /// Maximum ratio of orphaned files before rebuild (0.0-1.0)
    ///
    /// Sets the threshold for orphaned file errors during validation.
    /// Default: 0.20 (20%). If more than this ratio of indexed files are
    /// orphaned (no longer exist on disk), validation will fail.
    #[arg(long, value_name = "RATIO", global = true, hide_long_help = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 43)]
    pub threshold_orphaned_files: Option<f64>,

    /// Maximum ratio of ID gaps before warning (0.0-1.0)
    ///
    /// Sets the threshold for ID gap warnings during validation.
    /// Default: 0.10 (10%). If more than this ratio of symbol IDs have gaps,
    /// validation will warn or fail depending on strictness.
    #[arg(long, value_name = "RATIO", global = true, hide_long_help = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 44)]
    pub threshold_id_gaps: Option<f64>,

    // ===== Hybrid Search Flags =====
    /// Force text search mode (skip semantic, use ripgrep)
    #[arg(long, short = 't', conflicts_with = "semantic", help_heading = headings::SEARCH_MODES, display_order = 10)]
    pub text: bool,

    /// Force semantic search mode (skip text fallback)
    #[arg(long, short = 's', conflicts_with = "text", help_heading = headings::SEARCH_MODES, display_order = 11)]
    pub semantic: bool,

    /// Disable automatic fallback to text search
    #[arg(long, conflicts_with_all = ["text", "semantic"], help_heading = headings::SEARCH_MODES, display_order = 20)]
    pub no_fallback: bool,

    /// Number of context lines for text search results
    #[arg(long, default_value = "2", help_heading = headings::SEARCH_MODES, display_order = 30)]
    pub context: usize,

    /// Maximum text search results
    #[arg(long, default_value = "1000", help_heading = headings::SEARCH_MODES, display_order = 31)]
    pub max_text_results: usize,

    /// Show verbose progress output (stages + timing) to stderr.
    ///
    /// Honoured by the top-level shorthand `sqry <pat>` search path. For the
    /// explicit `sqry search` subcommand, use the per-subcommand `--verbose`
    /// flag instead — this top-level flag is not propagated to subcommands.
    ///
    /// As an env-driven equivalent, set `SQRY_LOG=info` (or `RUST_LOG=info`)
    /// before invocation; either form enables verbose. Explicit `--verbose`
    /// wins over env when both agree, and is required when env is unset.
    #[arg(long, short = 'v', help_heading = headings::OUTPUT_CONTROL, display_order = 50)]
    pub verbose: bool,
}

/// Plugin-selection controls shared by indexing and selected read paths.
#[derive(Args, Debug, Clone, Default)]
pub struct PluginSelectionArgs {
    /// Enable all compiled non-default plugins.
    ///
    /// This includes `high_wall_clock` plugins and optional plugins compiled
    /// into the shared plugin registry.
    #[arg(long, conflicts_with = "exclude_high_cost", help_heading = headings::PLUGIN_SELECTION, display_order = 10)]
    pub include_high_cost: bool,

    /// Exclude all compiled non-default plugins.
    ///
    /// This is mainly useful to override `SQRY_INCLUDE_HIGH_COST=1`.
    #[arg(long, conflicts_with = "include_high_cost", help_heading = headings::PLUGIN_SELECTION, display_order = 20)]
    pub exclude_high_cost: bool,

    /// Force-enable a plugin by id.
    ///
    /// Repeat this flag to enable multiple plugins. Explicit enable beats the
    /// global include mode unless the same plugin is also explicitly disabled.
    #[arg(long = "enable-plugin", alias = "enable-language", value_name = "ID", help_heading = headings::PLUGIN_SELECTION, display_order = 30)]
    pub enable_plugins: Vec<String>,

    /// Force-disable a plugin by id.
    ///
    /// Repeat this flag to disable multiple plugins. Explicit disable wins over
    /// explicit enable and global include mode.
    #[arg(long = "disable-plugin", alias = "disable-language", value_name = "ID", help_heading = headings::PLUGIN_SELECTION, display_order = 40)]
    pub disable_plugins: Vec<String>,
}

/// Batch command arguments with taxonomy headings and workflow ordering
#[derive(Args, Debug, Clone)]
pub struct BatchCommand {
    /// Directory containing the indexed codebase (`.sqry/graph/snapshot.sqry`).
    #[arg(value_name = "PATH", help_heading = headings::BATCH_INPUTS, display_order = 10)]
    pub path: Option<String>,

    /// File containing queries (one per line).
    #[arg(long, value_name = "FILE", help_heading = headings::BATCH_INPUTS, display_order = 20)]
    pub queries: PathBuf,

    /// Set output format for results.
    #[arg(long, value_name = "FORMAT", default_value = "text", value_enum, help_heading = headings::BATCH_OUTPUT_TARGETS, display_order = 10)]
    pub output: BatchFormat,

    /// Write results to specified file instead of stdout.
    #[arg(long, value_name = "FILE", help_heading = headings::BATCH_OUTPUT_TARGETS, display_order = 20)]
    pub output_file: Option<PathBuf>,

    /// Continue processing if a query fails.
    #[arg(long, help_heading = headings::BATCH_SESSION_CONTROL, display_order = 10)]
    pub continue_on_error: bool,

    /// Print aggregate statistics after completion.
    #[arg(long, help_heading = headings::BATCH_SESSION_CONTROL, display_order = 20)]
    pub stats: bool,

    /// Use sequential execution instead of parallel (for debugging).
    ///
    /// By default, batch queries execute in parallel for better performance.
    /// Use this flag to force sequential execution for debugging or profiling.
    #[arg(long, help_heading = headings::BATCH_SESSION_CONTROL, display_order = 30)]
    pub sequential: bool,
}

/// Completions command arguments with taxonomy headings and workflow ordering
#[derive(Args, Debug, Clone)]
pub struct CompletionsCommand {
    /// Shell to generate completions for.
    #[arg(value_enum, help_heading = headings::COMPLETIONS_SHELL_TARGETS, display_order = 10)]
    pub shell: Shell,
}

/// Available subcommands
#[derive(Subcommand, Debug, Clone)]
#[command(verbatim_doc_comment)]
pub enum Command {
    /// Visualize code relationships as diagrams
    #[command(display_order = 30)]
    Visualize(VisualizeCommand),

    /// Search for symbols by name pattern (regex / literal matching)
    ///
    /// Pattern-based search. The pattern is treated as a Rust regex by
    /// default, or as a byte-literal symbol-name match with `--exact`.
    /// This is **not** the structural planner — predicates like
    /// `kind:function`, `lang:rust`, `name~=/.../`, or boolean
    /// `AND` / `OR` are NOT accepted here. Use `sqry query` for those.
    ///
    /// On large workspaces (>50k nodes), narrow with `--lang` /
    /// `--kind` to keep latency bounded. See
    /// `docs/cli/scaling-large-codebases.md` for the pairing rule.
    ///
    /// Examples:
    ///   sqry search "test.*"           # regex match on names
    ///   sqry search "main" --exact     # byte-literal name match
    ///   sqry search "test" --kind function --lang rust
    ///   sqry search "test" --save-as find-tests  # save as alias
    ///   sqry search "test" --validate fail       # strict index validation
    ///   sqry search "test" --verbose             # show snapshot-load + lookup timing on stderr
    ///
    /// For kind/language/fuzzy filtering, the top-level shorthand also
    /// works:
    ///   sqry --kind function "test"    # Filter by kind
    ///   sqry --exact "main"            # Exact match
    ///   sqry --fuzzy "config"          # Fuzzy search
    ///
    /// Progress and timing visibility:
    ///   `--verbose` (or `-v`) emits stage events to stderr — `load snapshot`,
    ///   `exact name lookup` or `regex scan`, `apply filters`. Set
    ///   `SQRY_LOG=info` for env-driven enablement, or `SQRY_OUTPUT_FORMAT=json`
    ///   for line-delimited JSON events instead of `[sqry] ...` plain text.
    ///   In Tier 1, `sqry search` still loads the graph in-process; when sqryd
    ///   answers `daemon/status`, verbose mode emits one note explaining that
    ///   daemon-backed search is not yet attached. Tier 2 will attach to sqryd
    ///   when reachable and fall through to in-process load otherwise.
    ///
    /// See also: 'sqry query' for structured AST-aware queries.
    #[command(display_order = 1, verbatim_doc_comment)]
    Search {
        /// Search pattern (regex by default; literal byte-exact
        /// symbol-name match with `--exact`).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        pattern: String,

        /// Search path. For fuzzy search, walks up directory tree to find nearest .sqry-index if needed.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 20)]
        path: Option<String>,

        /// Save this search as a named alias for later reuse.
        ///
        /// The alias can be invoked with @name syntax:
        ///   sqry search "test" --save-as find-tests
        ///   sqry @find-tests src/
        #[arg(long, value_name = "NAME", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 10)]
        save_as: Option<String>,

        /// Save alias to global storage (~/.config/sqry/) instead of local.
        ///
        /// Global aliases are available across all projects.
        /// Local aliases (default) are project-specific.
        #[arg(long, requires = "save_as", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 20)]
        global: bool,

        /// Optional description for the saved alias.
        #[arg(long, requires = "save_as", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 30)]
        description: Option<String>,

        /// Index validation mode before search execution.
        ///
        /// Controls how sqry handles stale indices (files removed since indexing):
        /// - `warn`: Log warning but continue (default)
        /// - `fail`: Exit with code 2 if >20% of indexed files are missing
        /// - `off`: Skip validation entirely
        ///
        /// Examples:
        ///   sqry search "test" --validate fail  # Strict mode
        ///   sqry search "test" --validate off   # Fast mode
        #[arg(long, value_enum, default_value = "warn", help_heading = headings::SECURITY_LIMITS, display_order = 30)]
        validate: ValidationMode,

        /// Only show symbols active under given cfg predicate.
        ///
        /// Filters search results to symbols matching the specified cfg condition.
        /// Example: --cfg-filter test only shows symbols gated by #[cfg(test)].
        #[arg(long, value_name = "PREDICATE", help_heading = headings::SEARCH_INPUT, display_order = 30)]
        cfg_filter: Option<String>,

        /// Include macro-generated symbols in results (default: excluded).
        ///
        /// By default, symbols generated by macro expansion (e.g., derive impls)
        /// are excluded from search results. Use this flag to include them.
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 31)]
        include_generated: bool,

        /// Show macro boundary metadata in output.
        ///
        /// When enabled, search results include macro boundary information
        /// such as cfg conditions, macro source, and generated-symbol markers.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 40)]
        macro_boundaries: bool,

        /// Show verbose progress output (stages + timing) to stderr.
        ///
        /// Emits stage events for snapshot load, exact-name lookup, regex
        /// scan, fuzzy match, and filter application. Set
        /// `SQRY_OUTPUT_FORMAT=json` to emit JSON-line events instead of
        /// `[sqry] ...` plain text.
        ///
        /// Tier 1 still uses the in-process search path. When sqryd answers
        /// `daemon/status`, verbose mode emits one note that daemon-backed
        /// search is not yet attached; Tier 2 will attach to sqryd when
        /// reachable and fall through to in-process load otherwise.
        ///
        /// As an env-driven equivalent, set `SQRY_LOG=info` (or
        /// `RUST_LOG=info`) before invocation.
        #[arg(long, short = 'v', help_heading = headings::OUTPUT_CONTROL, display_order = 50)]
        verbose: bool,
    },

    /// Execute a structural AST-aware query (sqry-core query parser)
    ///
    /// Routes through the `sqry-core` query parser. Accepts `kind:`,
    /// `lang:`, `path:` / `file:`, `name:`, `name~=/regex/`,
    /// `visibility:`, `async:`, `callers:`, `callees:`, `imports:`,
    /// `exports:`, plus boolean `AND` / `OR` / `NOT`. This is **not**
    /// the regex / literal pattern surface — `sqry search` is — and it
    /// is **not** the planner-DAG grammar; for joins / subqueries /
    /// fusion use `sqry plan-query` instead. The two grammars are NOT
    /// predicate-equivalent: the planner does not accept `name~=`.
    ///
    /// On large workspaces (>50k nodes), every `name~=/regex/` must be
    /// paired with at least one of `lang:`, `path:`, or `kind:` to
    /// avoid the cost gate's `query_too_broad` rejection. See
    /// `docs/cli/scaling-large-codebases.md`.
    ///
    /// Predicate examples:
    ///   - kind:function                  # Find functions
    ///   - name:test                      # Name contains 'test'
    ///   - name~=/_set$/ kind:method      # Regex paired with kind
    ///   - lang:rust                      # Rust files only
    ///   - visibility:public              # Public symbols
    ///   - async:true                     # Async functions
    ///
    /// Boolean logic:
    ///   - kind:function AND name:test    # Functions with 'test' in name
    ///   - kind:class OR kind:struct      # All classes or structs
    ///   - lang:rust AND visibility:public # Public Rust symbols
    ///
    /// Relation queries (28 languages with full support):
    ///   - callers:authenticate           # Who calls authenticate?
    ///   - callees:processData            # What does processData call?
    ///   - exports:UserService            # What does `UserService` export?
    ///   - imports:database               # What imports database?
    ///
    /// Supported for: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML,
    /// Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala,
    /// Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig
    ///
    /// Saving as alias:
    ///   sqry query "kind:function AND name:test" --save-as test-funcs
    ///   sqry @test-funcs src/
    ///
    /// See also: 'sqry search' for regex / literal name matching.
    #[command(display_order = 2, verbatim_doc_comment)]
    Query {
        /// Query expression with predicates.
        #[arg(help_heading = headings::QUERY_INPUT, display_order = 10)]
        query: String,

        /// Search path. If no index exists here, walks up directory tree to find nearest .sqry-index.
        #[arg(help_heading = headings::QUERY_INPUT, display_order = 20)]
        path: Option<String>,

        /// Use persistent session (keeps .sqry-index hot for repeated queries).
        #[arg(long, help_heading = headings::PERFORMANCE_DEBUGGING, display_order = 10)]
        session: bool,

        /// Explain query execution (debug mode).
        #[arg(long, help_heading = headings::PERFORMANCE_DEBUGGING, display_order = 20)]
        explain: bool,

        /// Disable parallel query execution (for A/B performance testing).
        ///
        /// By default, OR branches (3+) and symbol filtering (100+) use parallel execution.
        /// Use this flag to force sequential execution for performance comparison.
        #[arg(long, help_heading = headings::PERFORMANCE_DEBUGGING, display_order = 30)]
        no_parallel: bool,

        /// Show verbose output including cache statistics.
        #[arg(long, short = 'v', help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        verbose: bool,

        /// Maximum query execution time in seconds (default: 30s, max: 30s).
        ///
        /// Queries exceeding this limit will be terminated with partial results.
        /// The 30-second ceiling is a NON-NEGOTIABLE security requirement.
        /// Specify lower values for faster feedback on interactive queries.
        ///
        /// Examples:
        ///   sqry query --timeout 10 "impl:Debug"    # 10 second timeout
        ///   sqry query --timeout 5 "kind:function"  # 5 second timeout
        #[arg(long, value_name = "SECS", help_heading = headings::SECURITY_LIMITS, display_order = 10)]
        timeout: Option<u64>,

        /// Maximum number of results to return (default: 10000).
        ///
        /// Queries returning more results will be truncated.
        /// Use this to limit memory usage for large result sets.
        ///
        /// Examples:
        ///   sqry query --limit 100 "kind:function"  # First 100 functions
        ///   sqry query --limit 1000 "impl:Debug"    # First 1000 Debug impls
        #[arg(long, value_name = "N", help_heading = headings::SECURITY_LIMITS, display_order = 20)]
        limit: Option<usize>,

        /// Save this query as a named alias for later reuse.
        ///
        /// The alias can be invoked with @name syntax:
        ///   sqry query "kind:function" --save-as all-funcs
        ///   sqry @all-funcs src/
        #[arg(long, value_name = "NAME", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 10)]
        save_as: Option<String>,

        /// Save alias to global storage (~/.config/sqry/) instead of local.
        ///
        /// Global aliases are available across all projects.
        /// Local aliases (default) are project-specific.
        #[arg(long, requires = "save_as", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 20)]
        global: bool,

        /// Optional description for the saved alias.
        #[arg(long, requires = "save_as", help_heading = headings::PERSISTENCE_OPTIONS, display_order = 30)]
        description: Option<String>,

        /// Index validation mode before query execution.
        ///
        /// Controls how sqry handles stale indices (files removed since indexing):
        /// - `warn`: Log warning but continue (default)
        /// - `fail`: Exit with code 2 if >20% of indexed files are missing
        /// - `off`: Skip validation entirely
        ///
        /// Examples:
        ///   sqry query "kind:function" --validate fail  # Strict mode
        ///   sqry query "kind:function" --validate off   # Fast mode
        #[arg(long, value_enum, default_value = "warn", help_heading = headings::SECURITY_LIMITS, display_order = 30)]
        validate: ValidationMode,

        /// Substitute variables in the query expression.
        ///
        /// Variables are referenced as $name in queries and resolved before execution.
        /// Specify as KEY=VALUE pairs; can be repeated.
        ///
        /// Examples:
        ///   sqry query "kind:\$type" --var type=function
        ///   sqry query "kind:\$k AND lang:\$l" --var k=function --var l=rust
        #[arg(long = "var", value_name = "KEY=VALUE", help_heading = headings::QUERY_INPUT, display_order = 30)]
        var: Vec<String>,

        #[command(flatten)]
        plugin_selection: PluginSelectionArgs,
    },

    /// Execute a structural query through the sqry-db planner (DB13).
    ///
    /// Uses the new salsa-style planner pipeline (parse → compile → fuse →
    /// execute) instead of the legacy `query` engine. Accepts the same text
    /// syntax documented in `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md`
    /// (§3 — Text Syntax Frontend).
    ///
    /// Predicate examples:
    ///   - `kind:function`                          Find every function
    ///   - `kind:function has:caller`               Functions that have at least one caller
    ///   - `kind:function callers:main`             Functions called by `main`
    ///   - `kind:function traverse:reverse(calls,3)`  Callers up to 3 hops deep
    ///   - `kind:function in:src/api/**`            Functions under src/api
    ///   - `kind:function references ~= /handle_.*/i`  Regex-matched references
    ///   - `kind:struct implements:Visitor`         Structs implementing `Visitor`
    ///
    /// Subqueries nest via parentheses:
    ///   - `kind:function callees:(kind:method name:visit_*)`
    ///
    /// DB13 scope note: this subcommand is parallel to the legacy `query`;
    /// DB14+ will migrate the legacy handlers and eventually replace
    /// `sqry query` with the planner path.
    #[command(name = "plan-query", display_order = 3, verbatim_doc_comment)]
    PlanQuery {
        /// Text query to parse and execute.
        #[arg(help_heading = headings::QUERY_INPUT, display_order = 10)]
        query: String,

        /// Search path (defaults to current directory). If no index exists
        /// here, walks up to find the nearest `.sqry-index`.
        #[arg(help_heading = headings::QUERY_INPUT, display_order = 20)]
        path: Option<String>,

        /// Maximum number of results to print (default: 1000).
        #[arg(long, value_name = "N", default_value = "1000", help_heading = headings::SECURITY_LIMITS, display_order = 10)]
        limit: usize,
    },

    /// Context-propagation analysis for Go code (T3.7).
    ///
    /// Surfaces `context.Context` plumbing breaks: sync callers that drop a
    /// caller's ctx, `go callee(...)` paths with no ctx, and HTTP-handler
    /// callers (`func(http.ResponseWriter, *http.Request)`) that fail to
    /// thread `r.Context()` into their callee.
    ///
    /// Routes through `sqry_db::queries::context_propagation::ContextPropagationQuery`
    /// via the standard `make_query_db_cold` cache path. Read-only.
    ///
    /// Exit codes:
    ///   0  success (zero leaks is a valid finding, NOT an error)
    ///   2  invalid `--scope` or `--mode`
    ///   3  no `.sqry-index` discoverable from the working directory
    #[command(name = "context-propagation", display_order = 4, verbatim_doc_comment)]
    ContextPropagation {
        /// Workspace path (defaults to current directory). If no
        /// `.sqry-index` exists here, walks up to find the nearest.
        #[arg(help_heading = headings::QUERY_INPUT, display_order = 10)]
        path: Option<String>,

        /// Scope selector. `global` (default) scans the whole workspace;
        /// `file:<path>` restricts to leaks whose caller function lives
        /// in the named file.
        #[arg(long, value_name = "SCOPE", default_value = "global", help_heading = headings::QUERY_INPUT, display_order = 20)]
        scope: String,

        /// Mode filter (default: `all`).
        #[arg(long, value_enum, default_value_t = ContextPropagationMode::All, help_heading = headings::QUERY_INPUT, display_order = 30)]
        mode: ContextPropagationMode,

        /// Maximum number of leak records to print.
        #[arg(long, value_name = "N", default_value = "1000", help_heading = headings::SECURITY_LIMITS, display_order = 10)]
        limit: usize,
    },

    /// Graph-based queries and analysis
    ///
    /// Advanced graph operations using the unified graph architecture.
    /// All subcommands are noun-based and represent different analysis types.
    ///
    /// Available analyses:
    ///   - `trace-path <from> <to>`           # Find shortest path between symbols
    ///   - `call-chain-depth <symbol>`        # Calculate maximum call depth
    ///   - `dependency-tree <module>`         # Show transitive dependencies
    ///   - nodes                             # List unified graph nodes
    ///   - edges                             # List unified graph edges
    ///   - cross-language                   # List cross-language relationships
    ///   - stats                            # Show graph statistics
    ///   - cycles                           # Detect circular dependencies
    ///   - complexity                       # Calculate code complexity
    ///
    /// All commands support --format json for programmatic use.
    #[command(display_order = 20)]
    Graph {
        #[command(subcommand)]
        operation: GraphOperation,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::GRAPH_CONFIGURATION, display_order = 10)]
        path: Option<String>,

        /// Output format (json, text, dot, mermaid, d2).
        ///
        /// Defaults to `text` when neither `--format` nor the global
        /// `--json` flag is supplied. The global `--json` flag is
        /// accepted on every `graph *` subcommand as an alias for
        /// `--format json`; passing both `--format text` (or any
        /// non-`json` value) and `--json` is an error so silent
        /// disagreement between the two flags can never occur.
        #[arg(long, short = 'f', help_heading = headings::GRAPH_CONFIGURATION, display_order = 20)]
        format: Option<String>,

        /// Show verbose output with detailed metadata.
        #[arg(long, short = 'v', help_heading = headings::GRAPH_CONFIGURATION, display_order = 30)]
        verbose: bool,
    },

    /// Start an interactive shell that keeps the session cache warm
    #[command(display_order = 60)]
    Shell {
        /// Directory containing the `.sqry-index` file.
        #[arg(value_name = "PATH", help_heading = headings::SHELL_CONFIGURATION, display_order = 10)]
        path: Option<String>,
    },

    /// Execute multiple queries from a batch file using a warm session
    #[command(display_order = 61)]
    Batch(BatchCommand),

    /// Build symbol index and graph analyses for fast queries
    ///
    /// Creates a persistent index of all symbols in the specified directory.
    /// The index is saved to .sqry/ and includes precomputed graph analyses
    /// for cycle detection, reachability, and path queries.
    /// Uses parallel processing by default for faster indexing.
    ///
    /// Upgrade-rebuild requirement: when sqry's in-format graph semantics
    /// change between releases (e.g. the v10.0.x Cluster C field-edge source
    /// migration), an existing `.sqry/graph/snapshot.sqry` keeps loading but
    /// returns the legacy shape until rebuilt. Run `sqry index --force` once
    /// after upgrading across such releases. Release notes call out which
    /// versions need the rebuild.
    #[command(display_order = 10)]
    Index {
        /// Directory to index (defaults to current directory).
        #[arg(help_heading = headings::INDEX_INPUT, display_order = 10)]
        path: Option<String>,

        /// Force rebuild even if index exists.
        ///
        /// Required once after upgrading across a release that changes
        /// in-format graph semantics (e.g. v10.0.x Cluster C field-edge
        /// source migration). Without `--force`, the existing snapshot
        /// loads but returns the pre-upgrade graph shape.
        #[arg(long, short = 'f', alias = "rebuild", help_heading = headings::INDEX_CONFIGURATION, display_order = 10)]
        force: bool,

        /// Show index status without building.
        ///
        /// Returns metadata about the existing index (age, symbol count, languages).
        /// Useful for programmatic consumers to check if indexing is needed.
        #[arg(long, short = 's', help_heading = headings::INDEX_CONFIGURATION, display_order = 20)]
        status: bool,

        /// Automatically add .sqry-index/ to .gitignore if not already present.
        #[arg(long, help_heading = headings::INDEX_CONFIGURATION, display_order = 30)]
        add_to_gitignore: bool,

        /// Number of threads for parallel indexing (default: auto-detect).
        ///
        /// Set to 1 for single-threaded (useful for debugging).
        /// Defaults to number of CPU cores.
        #[arg(long, short = 't', help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,

        /// Disable incremental indexing (hash-based change detection).
        ///
        /// When set, indexing will skip the persistent hash index and avoid
        /// hash-based change detection entirely. Useful for debugging or
        /// forcing metadata-only evaluation.
        #[arg(long = "no-incremental", help_heading = headings::PERFORMANCE_TUNING, display_order = 20)]
        no_incremental: bool,

        /// Override cache directory for incremental indexing (default: .sqry-cache).
        ///
        /// Points sqry at an alternate cache location for the hash index.
        /// Handy for ephemeral or sandboxed environments.
        #[arg(long = "cache-dir", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 10)]
        cache_dir: Option<String>,

        /// Metrics export format for validation status (json or prometheus).
        ///
        /// Used with --status --json to export validation metrics in different
        /// formats. Prometheus format outputs OpenMetrics-compatible text for
        /// monitoring systems. JSON format (default) provides structured data.
        #[arg(long, short = 'M', value_enum, default_value = "json", requires = "status", help_heading = headings::OUTPUT_CONTROL, display_order = 30)]
        metrics_format: MetricsFormat,

        /// Enable live macro expansion during indexing (executes cargo expand — security opt-in).
        ///
        /// When enabled, sqry runs `cargo expand` to capture macro-generated symbols.
        /// This executes build scripts and proc macros, so only use on trusted codebases.
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 30)]
        enable_macro_expansion: bool,

        /// Set active cfg flags for conditional compilation analysis.
        ///
        /// Can be specified multiple times (e.g., --cfg test --cfg unix).
        /// Symbols gated by `#[cfg()]` will be marked active/inactive based on these flags.
        #[arg(long = "cfg", value_name = "PREDICATE", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 31)]
        cfg_flags: Vec<String>,

        /// Use pre-generated expand cache instead of live expansion.
        ///
        /// Points to a directory containing cached macro expansion output
        /// (generated by `sqry cache expand`). Avoids executing cargo expand
        /// during indexing.
        #[arg(long, value_name = "DIR", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 32)]
        expand_cache: Option<PathBuf>,

        /// Enable JVM classpath analysis.
        ///
        /// Detects the project's build system (Gradle, Maven, Bazel, sbt),
        /// resolves dependency JARs, parses bytecode into class stubs, and
        /// emits synthetic graph nodes for classpath types. Enables cross-
        /// reference resolution from workspace source to library classes.
        ///
        /// Requires the `jvm-classpath` feature at compile time.
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 40)]
        classpath: bool,

        /// Disable classpath analysis (overrides config defaults).
        #[arg(long, conflicts_with = "classpath", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 41)]
        no_classpath: bool,

        /// Classpath analysis depth.
        ///
        /// `full` (default): include all transitive dependencies.
        /// `shallow`: only direct (compile-scope) dependencies.
        #[arg(long, value_enum, default_value = "full", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 42)]
        classpath_depth: ClasspathDepthArg,

        /// Manual classpath file (one JAR path per line).
        ///
        /// When provided, skips build system detection and resolution entirely.
        /// Lines starting with `#` are treated as comments.
        #[arg(long, value_name = "FILE", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 43)]
        classpath_file: Option<PathBuf>,

        /// Override build system detection for classpath analysis.
        ///
        /// Valid values: gradle, maven, bazel, sbt (case-insensitive).
        #[arg(long, value_name = "SYSTEM", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 44)]
        build_system: Option<String>,

        /// Force classpath re-resolution (ignore cached classpath).
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 45)]
        force_classpath: bool,

        /// Allow creating a nested `.sqry/` index inside an outer
        /// project that already has one (cluster-E §E.3).
        ///
        /// By default `sqry index` refuses to create a second graph
        /// inside the same project boundary so accidental nested
        /// artifacts are caught early. Pass this flag when the nested
        /// directory is intentionally a sub-project with its own
        /// graph.
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 50)]
        allow_nested: bool,

        #[command(flatten)]
        plugin_selection: PluginSelectionArgs,
    },

    /// Build precomputed graph analyses for fast query performance
    ///
    /// Computes CSR adjacency, SCC (Strongly Connected Components), condensation DAGs,
    /// and 2-hop interval labels to eliminate O(V+E) query-time costs. Analysis files
    /// are persisted to .sqry/analysis/ and enable fast cycle detection, reachability
    /// queries, and path finding.
    ///
    /// Note: `sqry index` already builds a ready graph with analysis artifacts.
    /// Run `sqry analyze` when you want to rebuild analyses with explicit
    /// tuning controls or after changing analysis configuration.
    ///
    /// Examples:
    ///   sqry analyze                 # Rebuild analyses for current index
    ///   sqry analyze --force         # Force analysis rebuild
    #[command(display_order = 13, verbatim_doc_comment)]
    Analyze {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Force rebuild even if analysis files exist.
        #[arg(long, short = 'f', help_heading = headings::INDEX_CONFIGURATION, display_order = 10)]
        force: bool,

        /// Number of threads for parallel analysis (default: auto-detect).
        ///
        /// Controls the rayon thread pool size for SCC/condensation DAG
        /// computation. Set to 1 for single-threaded (useful for debugging).
        /// Defaults to number of CPU cores.
        #[arg(long, short = 't', help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,

        /// Override maximum 2-hop label intervals per edge kind.
        ///
        /// Controls the maximum number of reachability intervals computed
        /// per edge kind. Larger budgets enable O(1) reachability queries
        /// but use more memory. Default: from config or 15,000,000.
        #[arg(long = "label-budget", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 30)]
        label_budget: Option<u64>,

        /// Override density gate threshold.
        ///
        /// Skip 2-hop label computation when `condensation_edges > threshold * scc_count`.
        /// Prevents multi-minute hangs on dense import/reference graphs.
        /// 0 = disabled. Default: from config or 64.
        #[arg(long = "density-threshold", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 31)]
        density_threshold: Option<u64>,

        /// Override budget-exceeded policy: `"degrade"` (BFS fallback) or `"fail"`.
        ///
        /// When the label budget is exceeded for an edge kind:
        /// - `"degrade"`: Fall back to BFS on the condensation DAG (default)
        /// - "fail": Return an error and abort analysis
        #[arg(long = "budget-exceeded-policy", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 32, value_parser = clap::builder::PossibleValuesParser::new(["degrade", "fail"]))]
        budget_exceeded_policy: Option<String>,

        /// Skip 2-hop interval label computation entirely.
        ///
        /// When set, the analysis builds CSR + SCC + Condensation DAG but skips
        /// the expensive 2-hop label phase. Reachability queries fall back to BFS
        /// on the condensation DAG (~10-50ms per query instead of O(1)).
        /// Useful for very large codebases where label computation is too slow.
        #[arg(long = "no-labels", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 33)]
        no_labels: bool,
    },

    /// Start the sqry Language Server Protocol endpoint
    #[command(display_order = 50)]
    Lsp {
        #[command(flatten)]
        options: LspOptions,
    },

    /// Update existing symbol index
    ///
    /// Incrementally updates the index by re-indexing only changed files.
    /// Much faster than a full rebuild for large codebases.
    #[command(display_order = 11)]
    Update {
        /// Directory with existing index (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Number of threads for parallel indexing (default: auto-detect).
        ///
        /// Set to 1 for single-threaded (useful for debugging).
        /// Defaults to number of CPU cores.
        #[arg(long, short = 't', help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,

        /// Disable incremental indexing (force metadata-only or full updates).
        ///
        /// When set, the update process will not use the hash index and will
        /// rely on metadata-only checks for staleness.
        #[arg(long = "no-incremental", help_heading = headings::UPDATE_CONFIGURATION, display_order = 10)]
        no_incremental: bool,

        /// Override cache directory for incremental indexing (default: .sqry-cache).
        ///
        /// Points sqry at an alternate cache location for the hash index.
        #[arg(long = "cache-dir", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 10)]
        cache_dir: Option<String>,

        /// Show statistics about the update.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        stats: bool,

        /// Enable JVM classpath analysis.
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 40)]
        classpath: bool,

        /// Disable classpath analysis (overrides config defaults).
        #[arg(long, conflicts_with = "classpath", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 41)]
        no_classpath: bool,

        /// Classpath analysis depth.
        #[arg(long, value_enum, default_value = "full", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 42)]
        classpath_depth: ClasspathDepthArg,

        /// Manual classpath file (one JAR path per line).
        #[arg(long, value_name = "FILE", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 43)]
        classpath_file: Option<PathBuf>,

        /// Override build system detection for classpath analysis.
        #[arg(long, value_name = "SYSTEM", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 44)]
        build_system: Option<String>,

        /// Force classpath re-resolution (ignore cached classpath).
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 45)]
        force_classpath: bool,

        #[command(flatten)]
        plugin_selection: PluginSelectionArgs,
    },

    /// Watch directory and auto-update index on file changes
    ///
    /// Monitors the directory for file system changes and automatically updates
    /// the index in real-time. Uses OS-level file monitoring (inotify/FSEvents/Windows)
    /// for <1ms change detection latency.
    ///
    /// Press Ctrl+C to stop watching.
    #[command(display_order = 12)]
    Watch {
        /// Directory to watch (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Number of threads for parallel indexing (default: auto-detect).
        ///
        /// Set to 1 for single-threaded (useful for debugging).
        /// Defaults to number of CPU cores.
        #[arg(long, short = 't', help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,

        /// Build initial index if it doesn't exist.
        #[arg(long, help_heading = headings::WATCH_CONFIGURATION, display_order = 10)]
        build: bool,

        /// Debounce duration in milliseconds.
        ///
        /// Wait time after detecting a change before processing to collect
        /// rapid-fire changes (e.g., from editor saves).
        ///
        /// Default is platform-aware: 400ms on macOS, 100ms on Linux/Windows.
        /// Can also be set via `SQRY_LIMITS__WATCH__DEBOUNCE_MS` env var.
        #[arg(long, short = 'd', help_heading = headings::WATCH_CONFIGURATION, display_order = 20)]
        debounce: Option<u64>,

        /// Show detailed statistics for each update.
        #[arg(long, short = 's', help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        stats: bool,

        /// Enable JVM classpath analysis.
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 40)]
        classpath: bool,

        /// Disable classpath analysis (overrides config defaults).
        #[arg(long, conflicts_with = "classpath", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 41)]
        no_classpath: bool,

        /// Classpath analysis depth.
        #[arg(long, value_enum, default_value = "full", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 42)]
        classpath_depth: ClasspathDepthArg,

        /// Manual classpath file (one JAR path per line).
        #[arg(long, value_name = "FILE", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 43)]
        classpath_file: Option<PathBuf>,

        /// Override build system detection for classpath analysis.
        #[arg(long, value_name = "SYSTEM", help_heading = headings::ADVANCED_CONFIGURATION, display_order = 44)]
        build_system: Option<String>,

        /// Force classpath re-resolution (ignore cached classpath).
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 45)]
        force_classpath: bool,

        #[command(flatten)]
        plugin_selection: PluginSelectionArgs,
    },

    /// Repair corrupted index by fixing common issues
    ///
    /// Automatically detects and fixes common index corruption issues:
    /// - Orphaned symbols (files no longer exist)
    /// - Dangling references (symbols reference non-existent dependencies)
    /// - Invalid checksums
    ///
    /// Use --dry-run to preview changes without modifying the index.
    #[command(display_order = 14)]
    Repair {
        /// Directory with existing index (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Remove symbols for files that no longer exist on disk.
        #[arg(long, help_heading = headings::REPAIR_OPTIONS, display_order = 10)]
        fix_orphans: bool,

        /// Remove dangling references to non-existent symbols.
        #[arg(long, help_heading = headings::REPAIR_OPTIONS, display_order = 20)]
        fix_dangling: bool,

        /// Recompute index checksum after repairs.
        #[arg(long, help_heading = headings::REPAIR_OPTIONS, display_order = 30)]
        recompute_checksum: bool,

        /// Fix all detected issues (combines all repair options).
        #[arg(long, conflicts_with_all = ["fix_orphans", "fix_dangling", "recompute_checksum"], help_heading = headings::REPAIR_OPTIONS, display_order = 5)]
        fix_all: bool,

        /// Preview changes without modifying the index (dry run).
        #[arg(long, help_heading = headings::REPAIR_OPTIONS, display_order = 40)]
        dry_run: bool,
    },

    /// Manage AST cache
    ///
    /// Control the disk-persisted AST cache that speeds up queries by avoiding
    /// expensive tree-sitter parsing. The cache is stored in .sqry-cache/ and
    /// is shared across all sqry processes.
    #[command(display_order = 41)]
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Manage graph config (.sqry/graph/config/config.json)
    ///
    /// Configure sqry behavior through the unified config partition.
    /// All settings are stored in `.sqry/graph/config/config.json`.
    ///
    /// Examples:
    ///   sqry config init                     # Initialize config with defaults
    ///   sqry config show                     # Display effective config
    ///   sqry config set `limits.max_results` 10000  # Update a setting
    ///   sqry config get `limits.max_results`   # Get a single value
    ///   sqry config validate                 # Validate config file
    ///   sqry config alias set my-funcs "kind:function"  # Create alias
    ///   sqry config alias list               # List all aliases
    #[command(display_order = 40, verbatim_doc_comment)]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Generate shell completions
    ///
    /// Generate shell completion scripts for bash, zsh, fish, `PowerShell`, or elvish.
    /// Install by redirecting output to the appropriate location for your shell.
    ///
    /// Examples:
    ///   sqry completions bash > /`etc/bash_completion.d/sqry`
    ///   sqry completions zsh > ~/.zfunc/_sqry
    ///   sqry completions fish > ~/.config/fish/completions/sqry.fish
    ///   sqry completions elvish > ~/.config/elvish/lib/sqry.elv
    #[command(display_order = 45, verbatim_doc_comment)]
    Completions(CompletionsCommand),

    /// Manage multi-repository workspaces
    #[command(display_order = 42)]
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCommand,
    },

    /// Manage saved query aliases
    ///
    /// Save frequently used queries as named aliases for easy reuse.
    /// Aliases can be stored globally (~/.config/sqry/) or locally (.sqry-index.user).
    ///
    /// Examples:
    ///   sqry alias list                  # List all aliases
    ///   sqry alias show my-funcs         # Show alias details
    ///   sqry alias delete my-funcs       # Delete an alias
    ///   sqry alias rename old-name new   # Rename an alias
    ///
    /// To create an alias, use --save-as with search/query commands:
    ///   sqry query "kind:function" --save-as my-funcs
    ///   sqry search "test" --save-as find-tests --global
    ///
    /// To execute an alias, use @name syntax:
    ///   sqry @my-funcs
    ///   sqry @find-tests src/
    #[command(display_order = 43, verbatim_doc_comment)]
    Alias {
        #[command(subcommand)]
        action: AliasAction,
    },

    /// Manage query history
    ///
    /// View and manage your query history. History is recorded automatically
    /// for search and query commands (unless disabled via `SQRY_NO_HISTORY=1`).
    ///
    /// Examples:
    ///   sqry history list                # List recent queries
    ///   sqry history list --limit 50     # Show last 50 queries
    ///   sqry history search "function"   # Search history
    ///   sqry history clear               # Clear all history
    ///   sqry history clear --older 30d   # Clear entries older than 30 days
    ///   sqry history stats               # Show history statistics
    ///
    /// Sensitive data (API keys, tokens) is automatically redacted.
    #[command(display_order = 44, verbatim_doc_comment)]
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },

    /// Natural language interface for sqry queries
    ///
    /// Translate natural language descriptions into sqry commands.
    /// Uses a safety-focused translation pipeline that validates all
    /// generated commands before execution.
    ///
    /// Response tiers based on confidence:
    /// - Execute (≥85%): Run command automatically
    /// - Confirm (65-85%): Ask for user confirmation
    /// - Disambiguate (<65%): Present options to choose from
    /// - Reject: Cannot safely translate
    ///
    /// Examples:
    ///   sqry ask "find all public functions in rust"
    ///   sqry ask "who calls authenticate"
    ///   sqry ask "trace path from main to database"
    ///   sqry ask --auto-execute "find all classes"
    ///
    /// Safety: Commands are validated against a whitelist and checked
    /// for shell injection, path traversal, and other attacks.
    #[command(display_order = 3, verbatim_doc_comment)]
    Ask {
        /// Natural language query to translate.
        #[arg(help_heading = headings::NL_INPUT, display_order = 10)]
        query: String,

        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::NL_INPUT, display_order = 20)]
        path: Option<String>,

        /// Auto-execute high-confidence commands without confirmation.
        ///
        /// When enabled, commands with ≥85% confidence will execute
        /// immediately. Otherwise, all commands require confirmation.
        #[arg(long, help_heading = headings::NL_CONFIGURATION, display_order = 10)]
        auto_execute: bool,

        /// Show the translated command without executing.
        ///
        /// Useful for understanding what command would be generated
        /// from your natural language query.
        #[arg(long, help_heading = headings::NL_CONFIGURATION, display_order = 20)]
        dry_run: bool,

        /// Minimum confidence threshold for auto-execution (0.0-1.0).
        ///
        /// Commands with confidence below this threshold will always
        /// require confirmation, even with --auto-execute.
        #[arg(long, default_value = "0.85", help_heading = headings::NL_CONFIGURATION, display_order = 30)]
        threshold: f32,

        /// Override the intent-classifier model directory (NL02 resolver level 1).
        ///
        /// Bypasses the legacy `model_dir` config field, the
        /// `SQRY_NL_MODEL_DIR` environment variable, the XDG cache, and
        /// the next-to-binary fallback. The directory must contain a
        /// `manifest.json`; otherwise this candidate is skipped.
        #[arg(long, value_name = "PATH", help_heading = headings::NL_CONFIGURATION, display_order = 40)]
        model_dir: Option<std::path::PathBuf>,

        /// Allow loading a classifier whose checksums cannot be verified.
        ///
        /// Defaults to `false`. Also honoured via the
        /// `SQRY_NL_ALLOW_UNVERIFIED_MODEL=1` environment variable.
        #[arg(long, env = "SQRY_NL_ALLOW_UNVERIFIED_MODEL", help_heading = headings::NL_CONFIGURATION, display_order = 50)]
        allow_unverified_model: bool,

        /// Permit fetching the classifier model from the network when not present locally.
        ///
        /// Defaults to `false`. Also honoured via the
        /// `SQRY_NL_ALLOW_DOWNLOAD=1` environment variable.
        #[arg(long, env = "SQRY_NL_ALLOW_DOWNLOAD", help_heading = headings::NL_CONFIGURATION, display_order = 60)]
        allow_model_download: bool,
    },

    /// View usage insights and manage local diagnostics
    ///
    /// sqry captures anonymous behavioral patterns locally to help you
    /// understand your usage and improve the tool. All data stays on
    /// your machine unless you explicitly choose to share.
    ///
    /// Examples:
    ///   sqry insights show                    # Show current week's summary
    ///   sqry insights show --week 2025-W50    # Show specific week
    ///   sqry insights config                  # Show configuration
    ///   sqry insights config --disable        # Disable uses capture
    ///   sqry insights status                  # Show storage status
    ///   sqry insights prune --older 90d       # Clean up old data
    ///
    /// Privacy: All data is stored locally. No network calls are made
    /// unless you explicitly invoke the `share` subcommand (which generates
    /// a file, not a network request). The `share` subcommand is gated
    /// behind the `insights-share` Cargo feature; it is omitted from the
    /// CLI surface when the feature is not enabled.
    #[command(display_order = 62, verbatim_doc_comment)]
    Insights {
        #[command(subcommand)]
        action: InsightsAction,
    },

    /// Generate a troubleshooting bundle for issue reporting
    ///
    /// Creates a structured bundle containing diagnostic information
    /// that can be shared with the sqry team. All data is sanitized -
    /// no code content, file paths, or secrets are included.
    ///
    /// The bundle includes:
    /// - System information (OS, architecture)
    /// - sqry version and build type
    /// - Sanitized configuration
    /// - Recent use events (last 24h)
    /// - Recent errors
    ///
    /// Examples:
    ///   sqry troubleshoot                     # Generate to stdout
    ///   sqry troubleshoot -o bundle.json      # Save to file
    ///   sqry troubleshoot --dry-run           # Preview without generating
    ///   sqry troubleshoot --include-trace     # Include workflow trace
    ///
    /// Privacy: No paths, code content, or secrets are included.
    /// Review the output before sharing if you have concerns.
    #[command(display_order = 63, verbatim_doc_comment)]
    Troubleshoot {
        /// Output file path (default: stdout)
        #[arg(short = 'o', long, value_name = "FILE", help_heading = headings::INSIGHTS_OUTPUT, display_order = 10)]
        output: Option<String>,

        /// Preview bundle contents without generating
        #[arg(long = "dry-run", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 10)]
        dry_run: bool,

        /// Include workflow trace (opt-in, requires explicit consent)
        ///
        /// Adds a sequence of recent workflow steps to the bundle.
        /// The trace helps understand how operations were performed
        /// but reveals more behavioral patterns than the default bundle.
        #[arg(long, help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 20)]
        include_trace: bool,

        /// Time window for events to include (e.g., 24h, 7d)
        ///
        /// Defaults to 24 hours. Longer windows provide more context
        /// but may include older events.
        #[arg(long, default_value = "24h", value_name = "DURATION", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 30)]
        window: String,
    },

    /// Find duplicate code in the codebase
    ///
    /// Detects similar or identical code patterns using structural analysis.
    /// Supports different duplicate types:
    /// - body: Functions with identical/similar bodies
    /// - signature: Functions with identical signatures
    /// - struct: Structs with similar field layouts
    ///
    /// Examples:
    ///   sqry duplicates                        # Find body duplicates
    ///   sqry duplicates --type signature       # Find signature duplicates
    ///   sqry duplicates --threshold 90         # 90% similarity threshold
    ///   sqry duplicates --exact                # Exact matches only
    #[command(display_order = 21, verbatim_doc_comment)]
    Duplicates {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Type of duplicate detection.
        ///
        /// - body: Functions with identical/similar bodies (default)
        /// - signature: Functions with identical signatures
        /// - struct: Structs with similar field layouts
        #[arg(long, short = 't', default_value = "body", help_heading = headings::DUPLICATE_OPTIONS, display_order = 10)]
        r#type: String,

        /// Similarity threshold (0-100, default: 80).
        ///
        /// Higher values require more similarity to be considered duplicates.
        /// 100 means exact matches only.
        #[arg(long, default_value = "80", help_heading = headings::DUPLICATE_OPTIONS, display_order = 20)]
        threshold: u32,

        /// Maximum results to return.
        #[arg(long, default_value = "100", help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        max_results: usize,

        /// Exact matches only (equivalent to --threshold 100).
        #[arg(long, help_heading = headings::DUPLICATE_OPTIONS, display_order = 30)]
        exact: bool,
    },

    /// Find circular dependencies in the codebase
    ///
    /// Detects cycles in call graphs, import graphs, or module dependencies.
    /// Uses Tarjan's SCC algorithm for efficient O(V+E) detection.
    ///
    /// Examples:
    ///   sqry cycles                            # Find call cycles
    ///   sqry cycles --type imports             # Find import cycles
    ///   sqry cycles --min-depth 3              # Cycles with 3+ nodes
    ///   sqry cycles --include-self             # Include self-loops
    #[command(display_order = 22, verbatim_doc_comment)]
    Cycles {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Type of cycle detection.
        ///
        /// - calls: Function/method call cycles (default)
        /// - imports: File import cycles
        /// - modules: Module-level cycles
        #[arg(long, short = 't', default_value = "calls", help_heading = headings::CYCLE_OPTIONS, display_order = 10)]
        r#type: String,

        /// Minimum cycle depth (default: 2).
        #[arg(long, default_value = "2", help_heading = headings::CYCLE_OPTIONS, display_order = 20)]
        min_depth: usize,

        /// Maximum cycle depth (default: unlimited).
        #[arg(long, help_heading = headings::CYCLE_OPTIONS, display_order = 30)]
        max_depth: Option<usize>,

        /// Include self-loops (A → A).
        #[arg(long, help_heading = headings::CYCLE_OPTIONS, display_order = 40)]
        include_self: bool,

        /// Maximum results to return.
        #[arg(long, default_value = "100", help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        max_results: usize,
    },

    /// Find unused/dead code in the codebase
    ///
    /// Detects symbols that are never referenced using reachability analysis.
    /// Entry points (main, public lib exports, tests) are considered reachable.
    ///
    /// Examples:
    ///   sqry unused                            # Find all unused symbols
    ///   sqry unused --scope public             # Only public unused symbols
    ///   sqry unused --scope function           # Only unused functions
    ///   sqry unused --lang rust                # Only in Rust files
    #[command(display_order = 23, verbatim_doc_comment)]
    Unused {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Scope of unused detection.
        ///
        /// - all: All unused symbols (default)
        /// - public: Public symbols with no external references
        /// - private: Private symbols with no references
        /// - function: Unused functions only
        /// - struct: Unused structs/types only
        #[arg(long, short = 's', default_value = "all", help_heading = headings::UNUSED_OPTIONS, display_order = 10)]
        scope: String,

        /// Filter by language.
        #[arg(long, help_heading = headings::UNUSED_OPTIONS, display_order = 20)]
        lang: Option<String>,

        /// Filter by symbol kind.
        #[arg(long, help_heading = headings::UNUSED_OPTIONS, display_order = 30)]
        kind: Option<String>,

        /// Maximum results to return.
        #[arg(long, default_value = "100", help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        max_results: usize,
    },

    /// Export the code graph in various formats
    ///
    /// Exports the unified code graph to DOT, D2, Mermaid, or JSON formats
    /// for visualization or further analysis.
    ///
    /// Examples:
    ///   sqry export                            # DOT format to stdout
    ///   sqry export --format mermaid           # Mermaid format
    ///   sqry export --format d2 -o graph.d2    # D2 format to file
    ///   sqry export --highlight-cross          # Highlight cross-language edges
    ///   sqry export --filter-lang rust,python  # Filter languages
    #[command(display_order = 31, verbatim_doc_comment)]
    Export {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Output format.
        ///
        /// - dot: Graphviz DOT format (default)
        /// - d2: D2 diagram format
        /// - mermaid: Mermaid markdown format
        /// - json: JSON format for programmatic use
        #[arg(long, short = 'f', default_value = "dot", help_heading = headings::EXPORT_OPTIONS, display_order = 10)]
        format: String,

        /// Graph layout direction.
        ///
        /// - lr: Left to right (default)
        /// - tb: Top to bottom
        #[arg(long, short = 'd', default_value = "lr", help_heading = headings::EXPORT_OPTIONS, display_order = 20)]
        direction: String,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::EXPORT_OPTIONS, display_order = 30)]
        filter_lang: Option<String>,

        /// Filter by edge types (comma-separated: calls,imports,exports).
        #[arg(long, help_heading = headings::EXPORT_OPTIONS, display_order = 40)]
        filter_edge: Option<String>,

        /// Highlight cross-language edges.
        #[arg(long, help_heading = headings::EXPORT_OPTIONS, display_order = 50)]
        highlight_cross: bool,

        /// Show node details (signatures, docs).
        #[arg(long, help_heading = headings::EXPORT_OPTIONS, display_order = 60)]
        show_details: bool,

        /// Show edge labels.
        #[arg(long, help_heading = headings::EXPORT_OPTIONS, display_order = 70)]
        show_labels: bool,

        /// Output file (default: stdout).
        #[arg(long, short = 'o', help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        output: Option<String>,
    },

    /// Explain a symbol with context and relations
    ///
    /// Get detailed information about a symbol including its code context,
    /// callers, callees, and other relationships.
    ///
    /// Examples:
    ///   sqry explain src/main.rs main           # Explain main function
    ///   sqry explain src/lib.rs `MyStruct`        # Explain a struct
    ///   sqry explain --no-context file.rs func  # Skip code context
    ///   sqry explain --no-relations file.rs fn  # Skip relations
    #[command(alias = "exp", display_order = 26, verbatim_doc_comment)]
    Explain {
        /// File containing the symbol.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        file: String,

        /// Symbol name to explain.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 20)]
        symbol: String,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 30)]
        path: Option<String>,

        /// Skip code context in output.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        no_context: bool,

        /// Skip relation information in output.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 20)]
        no_relations: bool,
    },

    /// Find symbols similar to a reference symbol
    ///
    /// Uses fuzzy name matching to find symbols that are similar
    /// to a given reference symbol.
    ///
    /// Examples:
    ///   sqry similar src/lib.rs processData     # Find similar to processData
    ///   sqry similar --threshold 0.8 file.rs fn # 80% similarity threshold
    ///   sqry similar --limit 20 file.rs func    # Limit to 20 results
    #[command(alias = "sim", display_order = 27, verbatim_doc_comment)]
    Similar {
        /// File containing the reference symbol.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        file: String,

        /// Reference symbol name.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 20)]
        symbol: String,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 30)]
        path: Option<String>,

        /// Minimum similarity threshold (0.0 to 1.0, default: 0.7).
        #[arg(long, short = 't', default_value = "0.7", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        threshold: f64,

        /// Maximum results to return (default: 20).
        #[arg(long, short = 'l', default_value = "20", help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        limit: usize,
    },

    /// Extract a focused subgraph around seed symbols
    ///
    /// Collects nodes and edges within a specified depth from seed symbols,
    /// useful for understanding local code structure.
    ///
    /// Examples:
    ///   sqry subgraph main                      # Subgraph around main
    ///   sqry subgraph -d 3 func1 func2          # Depth 3, multiple seeds
    ///   sqry subgraph --no-callers main         # Only callees
    ///   sqry subgraph --include-imports main    # Include import edges
    #[command(alias = "sub", display_order = 28, verbatim_doc_comment)]
    Subgraph {
        /// Seed symbol names (at least one required).
        #[arg(required = true, help_heading = headings::SEARCH_INPUT, display_order = 10)]
        symbols: Vec<String>,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 20)]
        path: Option<String>,

        /// Maximum traversal depth from seeds (default: 2).
        #[arg(long, short = 'd', default_value = "2", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        depth: usize,

        /// Maximum nodes to include (default: 50).
        #[arg(long, short = 'n', default_value = "50", help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        max_nodes: usize,

        /// Exclude callers (incoming edges).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        no_callers: bool,

        /// Exclude callees (outgoing edges).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 40)]
        no_callees: bool,

        /// Include import relationships.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 50)]
        include_imports: bool,
    },

    /// Analyze what would break if a symbol changes
    ///
    /// Performs reverse dependency analysis to find all symbols
    /// that directly or indirectly depend on the target.
    ///
    /// Examples:
    ///   sqry impact authenticate                # Impact of changing authenticate
    ///   sqry impact -d 5 `MyClass`                # Deep analysis (5 levels)
    ///   sqry impact --direct-only func          # Only direct dependents
    ///   sqry impact --show-files func           # Show affected files
    ///   sqry impact `do_exit` --in kernel/exit.c  # Disambiguate by file
    #[command(alias = "imp", display_order = 24, verbatim_doc_comment)]
    Impact {
        /// Symbol to analyze.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        symbol: String,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 20)]
        path: Option<String>,

        /// File the target symbol is defined in (disambiguator for ambiguous
        /// names — equivalent to the MCP `dependency_impact.file_path`
        /// argument). Accepts repo-relative or absolute paths.
        #[arg(long = "in", help_heading = headings::SEARCH_INPUT, display_order = 25, value_name = "FILE")]
        in_file: Option<String>,

        /// Maximum analysis depth (default: 3).
        #[arg(long, short = 'd', default_value = "3", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        depth: usize,

        /// Maximum results to return (default: 100).
        #[arg(long, short = 'l', default_value = "100", help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        limit: usize,

        /// Only show direct dependents (depth 1).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        direct_only: bool,

        /// Show list of affected files.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        show_files: bool,
    },

    /// Compare semantic changes between git refs
    ///
    /// Analyzes AST differences between two git refs to detect added, removed,
    /// modified, and renamed symbols. Provides structured output showing what
    /// changed semantically, not just textually.
    ///
    /// Examples:
    ///   sqry diff main HEAD                          # Compare branches
    ///   sqry diff v1.0.0 v2.0.0 --json              # Release comparison
    ///   sqry diff HEAD~5 HEAD --kind function       # Functions only
    ///   sqry diff main feature --change-type added  # New symbols only
    #[command(alias = "sdiff", display_order = 25, verbatim_doc_comment)]
    Diff {
        /// Base git ref (commit, branch, or tag).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        base: String,

        /// Target git ref (commit, branch, or tag).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 20)]
        target: String,

        /// Path to git repository (defaults to current directory).
        ///
        /// Can be the repository root or any path within it - sqry will walk up
        /// the directory tree to find the .git directory.
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 30)]
        path: Option<String>,

        /// Maximum total results to display (default: 100).
        #[arg(long, short = 'l', default_value = "100", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        limit: usize,

        /// Filter by symbol kinds (comma-separated).
        #[arg(long, short = 'k', help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        kind: Option<String>,

        /// Filter by change types (comma-separated).
        ///
        /// Valid values: `added`, `removed`, `modified`, `renamed`, `signature_changed`
        ///
        /// Example: --change-type added,modified
        #[arg(long, short = 'c', help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        change_type: Option<String>,
    },

    /// Hierarchical semantic search (RAG-optimized)
    ///
    /// Performs semantic search with results grouped by file and container,
    /// optimized for retrieval-augmented generation (RAG) workflows.
    ///
    /// Examples:
    ///   sqry hier "kind:function"               # All functions, grouped
    ///   sqry hier "auth" --max-files 10         # Limit file groups
    ///   sqry hier --kind function "test"        # Filter by kind
    ///   sqry hier --context 5 "validate"        # More context lines
    #[command(display_order = 4, verbatim_doc_comment)]
    Hier {
        /// Search query.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        query: String,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 20)]
        path: Option<String>,

        /// Maximum symbols before grouping (default: 200).
        #[arg(long, short = 'l', default_value = "200", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        limit: usize,

        /// Maximum files in output (default: 20).
        #[arg(long, default_value = "20", help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        max_files: usize,

        /// Context lines around matches (default: 3).
        #[arg(long, short = 'c', default_value = "3", help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        context: usize,

        /// Filter by symbol kinds (comma-separated).
        #[arg(long, short = 'k', help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        kind: Option<String>,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 40)]
        lang: Option<String>,
    },

    /// Configure MCP server integration for AI coding tools
    ///
    /// Auto-detect and configure sqry MCP for Claude Code, Codex, and Gemini CLI.
    /// The setup command writes tool-specific configuration so AI coding assistants
    /// can use sqry's semantic code search capabilities.
    ///
    /// Examples:
    ///   sqry mcp setup                            # Auto-configure all detected tools
    ///   sqry mcp setup --tool claude               # Configure Claude Code only
    ///   sqry mcp setup --scope global --dry-run    # Preview global config changes
    ///   sqry mcp status                            # Show current MCP configuration
    ///   sqry mcp status --json                     # Machine-readable status
    #[command(display_order = 51, verbatim_doc_comment)]
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },

    /// Manage the sqry daemon (sqryd).
    ///
    /// The daemon provides persistent, shared code-graph indexing for
    /// faster queries across concurrent editor sessions.
    ///
    /// Examples:
    ///   sqry daemon start              # Start the daemon in the background
    ///   sqry daemon stop               # Stop the running daemon
    ///   sqry daemon status             # Show daemon health and workspaces
    ///   sqry daemon status --json      # Machine-readable status
    ///   sqry daemon logs --follow      # Tail the daemon log
    #[command(display_order = 35, verbatim_doc_comment)]
    Daemon {
        #[command(subcommand)]
        action: Box<DaemonAction>,
    },
}

/// Daemon management subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum DaemonAction {
    /// Start the sqry daemon in the background.
    ///
    /// Locates the `sqryd` binary (sibling to `sqry` or on PATH),
    /// spawns it with `sqryd start --detach`, and waits for readiness.
    Start {
        /// Path to the sqryd binary (default: auto-detect).
        #[arg(long)]
        sqryd_path: Option<PathBuf>,
        /// Maximum seconds to wait for daemon readiness.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// Stop the running sqry daemon.
    Stop {
        /// Maximum seconds to wait for graceful shutdown.
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// Show daemon status (version, uptime, memory, workspaces).
    Status {
        /// Emit machine-readable JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Tail the daemon log file.
    Logs {
        /// Number of lines to show from the end of the log.
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
        /// Follow the log file for new output (like `tail -f`).
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Load a workspace into the running daemon.
    ///
    /// Connects to the daemon and sends a `daemon/load` request with the
    /// canonicalized path. The daemon's `WorkspaceManager` indexes the
    /// workspace, caches the graph in memory, and starts watching for
    /// file changes to rebuild incrementally.
    Load {
        /// Workspace root directory to load.
        path: PathBuf,
    },
    /// Trigger an in-place graph rebuild for a loaded workspace.
    ///
    /// Sends a `daemon/rebuild` request to the running daemon for the specified
    /// workspace root. Once wired (`CLI_REBUILD_3`), the daemon will re-index the
    /// workspace and replace the in-memory graph atomically on completion.
    ///
    /// Use `--force` to discard any incremental state and perform a full rebuild
    /// from scratch (equivalent to dropping and re-loading the workspace).
    ///
    /// The command will wait up to `--timeout` seconds for the rebuild to finish
    /// and report the result as human-readable text or, with `--json`, as a
    /// machine-readable JSON object.
    #[command(verbatim_doc_comment)]
    Rebuild {
        /// Workspace root directory to rebuild.
        path: PathBuf,
        /// Force a full rebuild from scratch, discarding incremental state.
        #[arg(long)]
        force: bool,
        /// Maximum seconds to wait for the rebuild to complete.
        /// Default is 1800 seconds (30 minutes). Pass 0 to fire-and-forget.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Emit machine-readable JSON output instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Reset a loaded workspace to `Unloaded` state without touching disk.
    ///
    /// Cluster-G §3.2 — non-destructive recovery primitive. Drops the
    /// in-memory graph and refunds admission bytes, but PRESERVES the
    /// workspace's manager-map entry, `pinned` bit, and `last_error`.
    /// Files under `<root>/.sqry/` are untouched — destructive cleanup
    /// is owned by `sqry workspace clean`.
    ///
    /// Use this to recover a workspace stuck in `Failed` or `Evicted`
    /// state (e.g. after a post-build oversize rejection) without
    /// stopping the daemon and without re-walking gitignore. The next
    /// `sqry daemon load <path>` is cheap because the prior snapshot
    /// is still on disk.
    ///
    /// Pass `--force` to reset a `pinned` workspace (refused by default).
    ///
    /// Mappings:
    ///
    ///   Loaded / Failed / Evicted → Unloaded
    ///   Rebuilding → cancellation dispatched (-32009; retry after 250ms)
    ///   Loading    → -32008 `ResetWhileLoading`
    #[command(verbatim_doc_comment)]
    Reset {
        /// Workspace root directory to reset.
        path: PathBuf,
        /// Reset even if the workspace is `pinned` in `daemon.toml`.
        #[arg(long)]
        force: bool,
    },
}

/// MCP server integration subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum McpCommand {
    /// Auto-configure sqry MCP for detected AI tools (Claude Code, Codex, Gemini)
    ///
    /// Detects installed AI coding tools and writes configuration entries
    /// pointing to the sqry-mcp binary. Uses tool-appropriate scoping:
    /// - Claude Code: per-project entries with pinned workspace root (default)
    /// - Codex/Gemini: global entries using CWD-based workspace discovery
    ///
    /// Note: Codex and Gemini only support global MCP configs.
    /// They rely on being launched from within a project directory
    /// for sqry-mcp's CWD discovery to resolve the correct workspace.
    Setup {
        /// Target tool(s) to configure.
        #[arg(long, value_enum, default_value = "all")]
        tool: ToolTarget,

        /// Configuration scope.
        ///
        /// - auto: project scope for Claude (when inside a repo), global for Codex/Gemini
        /// - project: per-project Claude entry with pinned workspace root
        /// - global: global entries for all tools (CWD-dependent for workspace resolution)
        ///
        /// Note: For Codex and Gemini, --scope project and --scope global behave
        /// identically because these tools only support global MCP configs.
        #[arg(long, value_enum, default_value = "auto")]
        scope: SetupScope,

        /// Explicit workspace root path (overrides auto-detection).
        ///
        /// Only applicable for Claude Code project scope. Rejected for
        /// Codex/Gemini because setting a workspace root in their global
        /// config would pin to one repo and break multi-repo workflows.
        #[arg(long)]
        workspace_root: Option<PathBuf>,

        /// Overwrite existing sqry configuration.
        #[arg(long)]
        force: bool,

        /// Preview changes without writing.
        #[arg(long)]
        dry_run: bool,

        /// Skip creating .bak backup files.
        #[arg(long)]
        no_backup: bool,
    },

    /// Show current MCP configuration status across all tools
    ///
    /// Reports the sqry-mcp binary location and configuration state
    /// for each supported AI tool, including scope, workspace root,
    /// and any detected issues (shim usage, drift, missing config).
    Status {
        /// Output as JSON for programmatic use.
        #[arg(long)]
        json: bool,
    },
}

/// Target AI tool(s) for MCP configuration
#[derive(Debug, Clone, ValueEnum)]
pub enum ToolTarget {
    /// Configure Claude Code only
    Claude,
    /// Configure Codex only
    Codex,
    /// Configure Gemini CLI only
    Gemini,
    /// Configure all detected tools (default)
    All,
}

/// Configuration scope for MCP setup
#[derive(Debug, Clone, ValueEnum)]
pub enum SetupScope {
    /// Per-project for Claude, global for Codex/Gemini (auto-detect)
    Auto,
    /// Per-project entries with pinned workspace root
    Project,
    /// Global entries (CWD-dependent workspace resolution)
    Global,
}

/// Graph-based query operations
#[derive(Subcommand, Debug, Clone)]
pub enum GraphOperation {
    /// Find shortest path between two symbols
    ///
    /// Traces the shortest execution path from one symbol to another,
    /// following Call, `HTTPRequest`, and `FFICall` edges.
    ///
    /// Example: sqry graph trace-path main processData
    TracePath {
        /// Source symbol name (e.g., "main", "User.authenticate").
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        from: String,

        /// Target symbol name.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 20)]
        to: String,

        /// Filter by languages (comma-separated, e.g., "javascript,python").
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        languages: Option<String>,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        full_paths: bool,
    },

    /// Calculate maximum call chain depth from a symbol
    ///
    /// Computes the longest call chain starting from the given symbol,
    /// useful for complexity analysis and recursion detection.
    ///
    /// Example: sqry graph call-chain-depth main
    CallChainDepth {
        /// Symbol name to analyze.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        languages: Option<String>,

        /// Show the actual call chain, not just the depth.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        show_chain: bool,
    },

    /// Show transitive dependencies for a module
    ///
    /// Analyzes all imports transitively to build a complete dependency tree,
    /// including circular dependency detection.
    ///
    /// Example: sqry graph dependency-tree src/main.js
    #[command(alias = "deps")]
    DependencyTree {
        /// Module path or name.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        module: String,

        /// Maximum depth to traverse (default: unlimited).
        #[arg(long, help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 10)]
        max_depth: Option<usize>,

        /// Show circular dependencies only.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        cycles_only: bool,
    },

    /// List all cross-language relationships
    ///
    /// Finds edges connecting symbols in different programming languages,
    /// such as TypeScript→JavaScript imports, Python→C FFI calls, SQL table
    /// access, Dart `MethodChannel` invocations, and Flutter widget hierarchies.
    ///
    /// Supported languages for --from-lang/--to-lang:
    ///   js, ts, py, cpp, c, csharp (cs), java, go, ruby, php,
    ///   swift, kotlin, scala, sql, dart, lua, perl, shell (bash),
    ///   groovy, http
    ///
    /// Examples:
    ///   sqry graph cross-language --from-lang dart --edge-type `channel_invoke`
    ///   sqry graph cross-language --from-lang sql  --edge-type `table_read`
    ///   sqry graph cross-language --edge-type `widget_child`
    #[command(verbatim_doc_comment)]
    CrossLanguage {
        /// Filter by source language.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        from_lang: Option<String>,

        /// Filter by target language.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        to_lang: Option<String>,

        /// Edge type filter.
        ///
        /// Supported values:
        ///   call, import, http, ffi,
        ///   `table_read`, `table_write`, `triggered_by`,
        ///   `channel_invoke`, `widget_child`
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        edge_type: Option<String>,

        /// Minimum confidence threshold (0.0-1.0).
        #[arg(long, default_value = "0.0", help_heading = headings::GRAPH_FILTERING, display_order = 40)]
        min_confidence: f64,
    },

    /// List unified graph nodes
    ///
    /// Enumerates nodes from the unified graph snapshot and applies filters.
    /// Useful for inspecting graph coverage and metadata details.
    Nodes {
        /// Filter by node kind(s) (comma-separated: function,method,macro).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        kind: Option<String>,

        /// Filter by language(s) (comma-separated: rust,python).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        languages: Option<String>,

        /// Filter by file path substring (case-insensitive).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        file: Option<String>,

        /// Filter by name substring (case-sensitive).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 40)]
        name: Option<String>,

        /// Filter by qualified name substring (case-sensitive).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 50)]
        qualified_name: Option<String>,

        /// Maximum results (default: 1000, max: 10000; use 0 for default).
        #[arg(long, default_value = "1000", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        limit: usize,

        /// Skip N results.
        #[arg(long, default_value = "0", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        offset: usize,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 30)]
        full_paths: bool,
    },

    /// List unified graph edges
    ///
    /// Enumerates edges from the unified graph snapshot and applies filters.
    /// Useful for inspecting relationships and cross-cutting metadata.
    Edges {
        /// Filter by edge kind tag(s) (comma-separated: calls,imports).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        kind: Option<String>,

        /// Filter by source label substring (case-sensitive).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        from: Option<String>,

        /// Filter by target label substring (case-sensitive).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 30)]
        to: Option<String>,

        /// Filter by source language.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 40)]
        from_lang: Option<String>,

        /// Filter by target language.
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 50)]
        to_lang: Option<String>,

        /// Filter by file path substring (case-insensitive, source file only).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 60)]
        file: Option<String>,

        /// Maximum results (default: 1000, max: 10000; use 0 for default).
        #[arg(long, default_value = "1000", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        limit: usize,

        /// Skip N results.
        #[arg(long, default_value = "0", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        offset: usize,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 30)]
        full_paths: bool,
    },

    /// Show graph statistics and summary
    ///
    /// Displays overall graph metrics including node counts by language,
    /// edge counts by type, and cross-language relationship statistics.
    ///
    /// Example: sqry graph stats
    Stats {
        /// Show detailed breakdown by file.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        by_file: bool,

        /// Show detailed breakdown by language.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        by_language: bool,
    },

    /// Show unified graph snapshot status
    ///
    /// Reports on the state of the unified graph snapshot stored in
    /// `.sqry/graph/` directory. Displays build timestamp, node/edge counts,
    /// and snapshot age.
    ///
    /// Example: sqry graph status
    Status,

    /// Show Phase 1 fact-layer provenance for a symbol
    ///
    /// Prints the snapshot's fact epoch, node provenance (first/last seen
    /// epoch, content hash), file provenance, and an edge-provenance summary
    /// for the matched symbol. This is the end-to-end proof that the V8
    /// save → load → accessor → CLI path is wired.
    ///
    /// Example: sqry graph provenance `my_function`
    #[command(alias = "prov")]
    Provenance {
        /// Symbol name to inspect (qualified or unqualified).
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Output as JSON.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        json: bool,
    },

    /// Resolve a symbol through the Phase 2 binding plane
    ///
    /// Loads the snapshot, constructs a `BindingPlane` facade, runs
    /// `BindingPlane::resolve()` for the given symbol, and prints the outcome
    /// along with the list of matched bindings. This is the end-to-end proof
    /// point for the Phase 2 binding plane (FR9).
    ///
    /// With `--explain` the ordered witness step trace is printed below the
    /// binding list, showing every bucket probe, candidate considered, and
    /// the terminal Chose/Ambiguous/Unresolved step.
    ///
    /// Example: sqry graph resolve `my_function`
    /// Example: sqry graph resolve `my_function` --explain
    /// Example: sqry graph resolve `my_function` --explain --json
    #[command(alias = "res")]
    Resolve {
        /// Symbol name to resolve (qualified or unqualified).
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Print the ordered witness step trace (bucket probes, candidate
        /// evaluations, and the terminal Chose/Ambiguous/Unresolved step).
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        explain: bool,

        /// Emit a stable JSON document instead of human-readable text.
        /// The JSON shape (symbol/outcome/bindings/explain) is the documented
        /// stable external contract for scripting and tool integration.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        json: bool,
    },

    /// Detect circular dependencies in the codebase
    ///
    /// Finds all cycles in the call and import graphs, which can indicate
    /// potential design issues or circular dependency problems.
    ///
    /// Example: sqry graph cycles
    #[command(alias = "cyc")]
    Cycles {
        /// Minimum cycle length to report (default: 2).
        #[arg(long, default_value = "2", help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 10)]
        min_length: usize,

        /// Maximum cycle length to report (default: unlimited).
        #[arg(long, help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 20)]
        max_length: Option<usize>,

        /// Only analyze import edges (ignore calls).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        imports_only: bool,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        languages: Option<String>,
    },

    /// Calculate code complexity metrics
    ///
    /// Analyzes cyclomatic complexity, call graph depth, and other
    /// complexity metrics for functions and modules.
    ///
    /// Example: sqry graph complexity
    #[command(alias = "cx")]
    Complexity {
        /// Target symbol or module (default: analyze all).
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        target: Option<String>,

        /// Sort by complexity score.
        #[arg(long = "sort-complexity", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        sort_complexity: bool,

        /// Show only items above this complexity threshold.
        #[arg(long, default_value = "0", help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        min_complexity: usize,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 20)]
        languages: Option<String>,
    },

    /// Find direct callers of a symbol
    ///
    /// Lists all symbols that directly call the specified function, method,
    /// or other callable. Useful for understanding symbol usage and impact analysis.
    ///
    /// Example: sqry graph direct-callers authenticate
    #[command(alias = "callers")]
    DirectCallers {
        /// Symbol name to find callers for.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Maximum results (default: 100).
        #[arg(long, short = 'l', default_value = "100", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        limit: usize,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        languages: Option<String>,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        full_paths: bool,
    },

    /// Find direct callees of a symbol
    ///
    /// Lists all symbols that are directly called by the specified function
    /// or method. Useful for understanding dependencies and refactoring scope.
    ///
    /// Example: sqry graph direct-callees processData
    #[command(alias = "callees")]
    DirectCallees {
        /// Symbol name to find callees for.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Maximum results (default: 100).
        #[arg(long, short = 'l', default_value = "100", help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        limit: usize,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        languages: Option<String>,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 20)]
        full_paths: bool,
    },

    /// Show call hierarchy for a symbol
    ///
    /// Displays incoming and/or outgoing call relationships in a tree format.
    /// Useful for understanding code flow and impact of changes.
    ///
    /// Example: sqry graph call-hierarchy main --depth 3
    #[command(alias = "ch")]
    CallHierarchy {
        /// Symbol name to show hierarchy for.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Maximum depth to traverse (default: 3).
        #[arg(long, short = 'd', default_value = "3", help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 10)]
        depth: usize,

        /// Direction: incoming, outgoing, or both (default: both).
        #[arg(long, default_value = "both", help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 20)]
        direction: String,

        /// Filter by languages (comma-separated).
        #[arg(long, help_heading = headings::GRAPH_FILTERING, display_order = 10)]
        languages: Option<String>,

        /// Show full file paths in output.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        full_paths: bool,
    },

    /// Check if a symbol is in a cycle
    ///
    /// Determines whether a specific symbol participates in any circular
    /// dependency chains. Can optionally show the cycle path.
    ///
    /// Example: sqry graph is-in-cycle `UserService` --show-cycle
    #[command(alias = "incycle")]
    IsInCycle {
        /// Symbol name to check.
        #[arg(help_heading = headings::GRAPH_ANALYSIS_INPUT, display_order = 10)]
        symbol: String,

        /// Cycle type to check: calls, imports, or all (default: calls).
        #[arg(long, default_value = "calls", help_heading = headings::GRAPH_ANALYSIS_OPTIONS, display_order = 10)]
        cycle_type: String,

        /// Show the full cycle path if found.
        #[arg(long, help_heading = headings::GRAPH_OUTPUT_OPTIONS, display_order = 10)]
        show_cycle: bool,
    },
}

/// Output format choices for `sqry batch`.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum BatchFormat {
    /// Human-readable text output (default)
    Text,
    /// Aggregated JSON output containing all query results
    Json,
    /// Newline-delimited JSON objects (one per query)
    Jsonl,
    /// Comma-separated summary per query
    Csv,
}

/// Cache management actions
#[derive(Subcommand, Debug, Clone)]
pub enum CacheAction {
    /// Show cache statistics
    ///
    /// Display hit rate, size, and entry count for the AST cache.
    Stats {
        /// Path to check cache for (defaults to current directory).
        #[arg(help_heading = headings::CACHE_INPUT, display_order = 10)]
        path: Option<String>,
    },

    /// Clear the cache
    ///
    /// Remove all cached AST data. Next queries will re-parse files.
    Clear {
        /// Path to clear cache for (defaults to current directory).
        #[arg(help_heading = headings::CACHE_INPUT, display_order = 10)]
        path: Option<String>,

        /// Confirm deletion (required for safety).
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        confirm: bool,
    },

    /// Prune the cache
    ///
    /// Remove old or excessive cache entries to reclaim disk space.
    /// Supports time-based (--days) and size-based (--size) retention policies.
    Prune {
        /// Target cache directory (defaults to user cache dir).
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 10)]
        path: Option<String>,

        /// Remove entries older than N days.
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 20)]
        days: Option<u64>,

        /// Cap cache to maximum size (e.g., "1GB", "500MB").
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 30)]
        size: Option<String>,

        /// Preview deletions without removing files.
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        dry_run: bool,
    },

    /// Generate or refresh the macro expansion cache
    ///
    /// Runs `cargo expand` to generate expanded macro output, then caches
    /// the results for use during indexing. Requires `cargo-expand` installed.
    ///
    /// # Security
    ///
    /// This executes build scripts and proc macros. Only use on trusted codebases.
    Expand {
        /// Force regeneration even if cache is fresh.
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 40)]
        refresh: bool,

        /// Only expand a specific crate (default: all workspace crates).
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 50)]
        crate_name: Option<String>,

        /// Show what would be expanded without actually running cargo expand.
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 20)]
        dry_run: bool,

        /// Cache output directory (default: .sqry/expand-cache/).
        #[arg(long, help_heading = headings::CACHE_INPUT, display_order = 60)]
        output: Option<PathBuf>,
    },
}

/// Config action subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum ConfigAction {
    /// Initialize config with defaults
    ///
    /// Creates `.sqry/graph/config/config.json` with default settings.
    /// Use --force to overwrite existing config.
    ///
    /// Examples:
    ///   sqry config init
    ///   sqry config init --force
    #[command(verbatim_doc_comment)]
    Init {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Overwrite existing config.
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 20)]
        force: bool,
    },

    /// Show effective config
    ///
    /// Displays the complete config with source annotations.
    /// Use --key to show a single value.
    ///
    /// Examples:
    ///   sqry config show
    ///   sqry config show --json
    ///   sqry config show --key `limits.max_results`
    #[command(verbatim_doc_comment)]
    Show {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Output as JSON.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        json: bool,

        /// Show only this config key (e.g., `limits.max_results`).
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 20)]
        key: Option<String>,
    },

    /// Set a config value
    ///
    /// Updates a config key and persists to disk.
    /// Shows a diff before applying (use --yes to skip).
    ///
    /// Examples:
    ///   sqry config set `limits.max_results` 10000
    ///   sqry config set `locking.stale_takeover_policy` warn
    ///   sqry config set `output.page_size` 100 --yes
    #[command(verbatim_doc_comment)]
    Set {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Config key (e.g., `limits.max_results`).
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 20)]
        key: String,

        /// New value.
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 30)]
        value: String,

        /// Skip confirmation prompt.
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 40)]
        yes: bool,
    },

    /// Get a config value
    ///
    /// Retrieves a single config value.
    ///
    /// Examples:
    ///   sqry config get `limits.max_results`
    ///   sqry config get `locking.stale_takeover_policy`
    #[command(verbatim_doc_comment)]
    Get {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Config key (e.g., `limits.max_results`).
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 20)]
        key: String,
    },

    /// Validate config file
    ///
    /// Checks config syntax and schema validity.
    ///
    /// Examples:
    ///   sqry config validate
    #[command(verbatim_doc_comment)]
    Validate {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,
    },

    /// Manage query aliases
    #[command(subcommand)]
    Alias(ConfigAliasAction),
}

/// Config alias subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum ConfigAliasAction {
    /// Create or update an alias
    ///
    /// Examples:
    ///   sqry config alias set my-funcs "kind:function"
    ///   sqry config alias set my-funcs "kind:function" --description "All functions"
    #[command(verbatim_doc_comment)]
    Set {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Alias name.
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 20)]
        name: String,

        /// Query expression.
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 30)]
        query: String,

        /// Optional description.
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 40)]
        description: Option<String>,
    },

    /// List all aliases
    ///
    /// Examples:
    ///   sqry config alias list
    ///   sqry config alias list --json
    #[command(verbatim_doc_comment)]
    List {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Output as JSON.
        #[arg(long, help_heading = headings::OUTPUT_CONTROL, display_order = 10)]
        json: bool,
    },

    /// Remove an alias
    ///
    /// Examples:
    ///   sqry config alias remove my-funcs
    #[command(verbatim_doc_comment)]
    Remove {
        /// Project root path (defaults to current directory).
        // Path defaults to current directory if not specified
        #[arg(long, help_heading = headings::CONFIG_INPUT, display_order = 5)]
        path: Option<String>,

        /// Alias name to remove.
        #[arg(help_heading = headings::CONFIG_INPUT, display_order = 20)]
        name: String,
    },
}

/// Visualize code relationships from relation queries.
///
/// Examples:
///   sqry visualize "callers:main" --format mermaid
///   sqry visualize "imports:std" --format graphviz --output-file deps.dot
///   sqry visualize "callees:process" --depth 5 --max-nodes 200
#[derive(Debug, Args, Clone)]
#[command(
    about = "Visualize code relationships as diagrams",
    long_about = "Visualize code relationships as diagrams.\n\n\
Examples:\n  sqry visualize \"callers:main\" --format mermaid\n  \
sqry visualize \"imports:std\" --format graphviz --output-file deps.dot\n  \
sqry visualize \"callees:process\" --depth 5 --max-nodes 200",
    after_help = "Examples:\n  sqry visualize \"callers:main\" --format mermaid\n  \
sqry visualize \"imports:std\" --format graphviz --output-file deps.dot\n  \
sqry visualize \"callees:process\" --depth 5 --max-nodes 200"
)]
pub struct VisualizeCommand {
    /// Relation query (e.g., callers:main, callees:helper).
    #[arg(help_heading = headings::VISUALIZATION_INPUT, display_order = 10)]
    pub query: String,

    /// Target path (defaults to CLI positional path).
    #[arg(long, help_heading = headings::VISUALIZATION_INPUT, display_order = 20)]
    pub path: Option<String>,

    /// Diagram syntax format (mermaid, graphviz, d2).
    ///
    /// Specifies the diagram language/syntax to generate.
    /// Output will be plain text in the chosen format.
    #[arg(long, short = 'f', value_enum, default_value = "mermaid", help_heading = headings::DIAGRAM_CONFIGURATION, display_order = 10)]
    pub format: DiagramFormatArg,

    /// Layout direction for the graph.
    #[arg(long, value_enum, default_value = "top-down", help_heading = headings::DIAGRAM_CONFIGURATION, display_order = 20)]
    pub direction: DirectionArg,

    /// File path to save the output (stdout when omitted).
    #[arg(long, help_heading = headings::DIAGRAM_CONFIGURATION, display_order = 30)]
    pub output_file: Option<PathBuf>,

    /// Maximum traversal depth for graph expansion.
    #[arg(long, short = 'd', default_value_t = 3, help_heading = headings::TRAVERSAL_CONTROL, display_order = 10)]
    pub depth: usize,

    /// Maximum number of nodes to include in the diagram (1-500).
    #[arg(long, default_value_t = 100, help_heading = headings::TRAVERSAL_CONTROL, display_order = 20)]
    pub max_nodes: usize,
}

/// `sqry context-propagation --mode` filter (T3.7, Cluster G-ext).
///
/// Mirrors `sqry_db::queries::context_propagation::ContextModeFilter` while
/// keeping the CLI surface in kebab-case for end-user ergonomics.
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum ContextPropagationMode {
    /// Every classified leak (default).
    #[default]
    All,
    /// Only `BreakSite` leaks (sync caller with `ctx` + ctx-accepting callee
    /// + call passes 0 context args).
    BreakSite,
    /// Only `UnthreadedGoroutine` leaks (`go callee(...)` paths).
    UnthreadedGoroutine,
    /// Only HTTP-handler leaks (`func(http.ResponseWriter, *http.Request)`
    /// callers).
    HttpHandlerLeak,
}

impl From<ContextPropagationMode> for sqry_db::queries::context_propagation::ContextModeFilter {
    fn from(mode: ContextPropagationMode) -> Self {
        use sqry_db::queries::context_propagation::ContextModeFilter as Cmf;
        match mode {
            ContextPropagationMode::All => Cmf::All,
            ContextPropagationMode::BreakSite => Cmf::BreakSite,
            ContextPropagationMode::UnthreadedGoroutine => Cmf::UnthreadedGoroutine,
            ContextPropagationMode::HttpHandlerLeak => Cmf::HttpHandlerLeak,
        }
    }
}

/// Supported diagram text formats.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DiagramFormatArg {
    Mermaid,
    Graphviz,
    D2,
}

/// Diagram layout direction.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum DirectionArg {
    TopDown,
    BottomUp,
    LeftRight,
    RightLeft,
}

/// Workspace management subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum WorkspaceCommand {
    /// Initialise a new workspace registry
    Init {
        /// Directory that will contain the workspace registry.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Preferred discovery mode for initial scans.
        #[arg(long, value_enum, default_value_t = WorkspaceDiscoveryMode::IndexFiles, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 10)]
        mode: WorkspaceDiscoveryMode,

        /// Friendly workspace name stored in the registry metadata.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 20)]
        name: Option<String>,
    },

    /// Scan for repositories inside the workspace root
    Scan {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Discovery mode to use when scanning for repositories.
        #[arg(long, value_enum, default_value_t = WorkspaceDiscoveryMode::IndexFiles, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 10)]
        mode: WorkspaceDiscoveryMode,

        /// Remove entries whose indexes are no longer present.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 20)]
        prune_stale: bool,
    },

    /// Add a repository to the workspace manually
    Add {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Path to the repository root (must contain .sqry-index).
        #[arg(value_name = "REPO", help_heading = headings::WORKSPACE_INPUT, display_order = 20)]
        repo: String,

        /// Optional friendly name for the repository.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 10)]
        name: Option<String>,
    },

    /// Remove a repository from the workspace
    Remove {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Repository identifier (workspace-relative path).
        #[arg(value_name = "REPO_ID", help_heading = headings::WORKSPACE_INPUT, display_order = 20)]
        repo_id: String,
    },

    /// Run a workspace-level query across registered repositories
    Query {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Query expression (supports repo: predicates).
        #[arg(value_name = "QUERY", help_heading = headings::WORKSPACE_INPUT, display_order = 20)]
        query: String,

        /// Override parallel query threads.
        #[arg(long, help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,
    },

    /// Emit aggregate statistics for the workspace
    Stats {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,
    },

    /// Print the aggregate index status for every source root in the workspace
    Status {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,

        /// Emit machine-readable JSON instead of the human-friendly summary.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 10)]
        json: bool,

        /// Bypass the 60-second aggregate-status cache and force a recompute.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 20)]
        no_cache: bool,
    },

    /// Discover and (optionally) remove stale .sqry artifacts under a path
    ///
    /// Cluster-E §E.4 — emits a `WorkspaceCleanReport` listing every
    /// `.sqry/`, `.sqry-cache`, `.sqry-prof`, legacy `.sqry-index`,
    /// `.sqry-index.user`, and stranded nested-`.sqry/` artifact found
    /// under the root, classifies each, and prints a dry-run plan.
    /// Pass `--apply` to actually remove the planned-for-removal set.
    Clean {
        /// Root to scan. Defaults to CWD.
        #[arg(value_name = "ROOT", default_value = ".", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        root: String,

        /// Actually remove the planned artifacts. Without this flag, the
        /// command prints what *would* be removed and exits without
        /// touching the filesystem.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 10)]
        apply: bool,

        /// Skip the active-daemon-artifact safety check. Required to
        /// remove a `.sqry/graph/` that the running daemon currently
        /// has loaded.
        #[arg(long, requires = "apply", help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 20)]
        force: bool,

        /// Also remove `.sqry-index.user` (user-curated state — aliases,
        /// recent queries). Off by default.
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 30)]
        include_user_state: bool,

        /// Emit the report as JSON (the `WorkspaceCleanReport` shape).
        #[arg(long, help_heading = headings::WORKSPACE_CONFIGURATION, display_order = 40)]
        json: bool,
    },
}

/// CLI discovery modes converted to workspace `DiscoveryMode` values
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WorkspaceDiscoveryMode {
    #[value(name = "index-files", alias = "index")]
    IndexFiles,
    #[value(name = "git-roots", alias = "git")]
    GitRoots,
}

/// Alias management subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum AliasAction {
    /// List all saved aliases
    ///
    /// Shows aliases from both global (~/.config/sqry/) and local (.sqry-index.user)
    /// storage. Local aliases take precedence over global ones with the same name.
    ///
    /// Examples:
    ///   sqry alias list              # List all aliases
    ///   sqry alias list --local      # Only local aliases
    ///   sqry alias list --global     # Only global aliases
    ///   sqry alias list --json       # JSON output
    #[command(verbatim_doc_comment)]
    List {
        /// Show only local aliases (project-specific).
        #[arg(long, conflicts_with = "global", help_heading = headings::ALIAS_CONFIGURATION, display_order = 10)]
        local: bool,

        /// Show only global aliases (cross-project).
        #[arg(long, conflicts_with = "local", help_heading = headings::ALIAS_CONFIGURATION, display_order = 20)]
        global: bool,
    },

    /// Show details of a specific alias
    ///
    /// Displays the command, arguments, description, and storage location
    /// for the named alias.
    ///
    /// Example: sqry alias show my-funcs
    Show {
        /// Name of the alias to show.
        #[arg(value_name = "NAME", help_heading = headings::ALIAS_INPUT, display_order = 10)]
        name: String,
    },

    /// Delete a saved alias
    ///
    /// Removes an alias from storage. If the alias exists in both local
    /// and global storage, specify --local or --global to delete from
    /// a specific location.
    ///
    /// Examples:
    ///   sqry alias delete my-funcs           # Delete (prefers local)
    ///   sqry alias delete my-funcs --global  # Delete from global only
    ///   sqry alias delete my-funcs --force   # Skip confirmation
    #[command(verbatim_doc_comment)]
    Delete {
        /// Name of the alias to delete.
        #[arg(value_name = "NAME", help_heading = headings::ALIAS_INPUT, display_order = 10)]
        name: String,

        /// Delete from local storage only.
        #[arg(long, conflicts_with = "global", help_heading = headings::ALIAS_CONFIGURATION, display_order = 10)]
        local: bool,

        /// Delete from global storage only.
        #[arg(long, conflicts_with = "local", help_heading = headings::ALIAS_CONFIGURATION, display_order = 20)]
        global: bool,

        /// Skip confirmation prompt.
        #[arg(long, short = 'f', help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        force: bool,
    },

    /// Rename an existing alias
    ///
    /// Changes the name of an alias while preserving its command and arguments.
    /// The alias is renamed in the same storage location where it was found.
    ///
    /// Example: sqry alias rename old-name new-name
    Rename {
        /// Current name of the alias.
        #[arg(value_name = "OLD_NAME", help_heading = headings::ALIAS_INPUT, display_order = 10)]
        old_name: String,

        /// New name for the alias.
        #[arg(value_name = "NEW_NAME", help_heading = headings::ALIAS_INPUT, display_order = 20)]
        new_name: String,

        /// Rename in local storage only.
        #[arg(long, conflicts_with = "global", help_heading = headings::ALIAS_CONFIGURATION, display_order = 10)]
        local: bool,

        /// Rename in global storage only.
        #[arg(long, conflicts_with = "local", help_heading = headings::ALIAS_CONFIGURATION, display_order = 20)]
        global: bool,
    },

    /// Export aliases to a JSON file
    ///
    /// Exports aliases for backup or sharing. The export format is compatible
    /// with the import command for easy restoration.
    ///
    /// Examples:
    ///   sqry alias export aliases.json          # Export all
    ///   sqry alias export aliases.json --local  # Export local only
    #[command(verbatim_doc_comment)]
    Export {
        /// Output file path (use - for stdout).
        #[arg(value_name = "FILE", help_heading = headings::ALIAS_INPUT, display_order = 10)]
        file: String,

        /// Export only local aliases.
        #[arg(long, conflicts_with = "global", help_heading = headings::ALIAS_CONFIGURATION, display_order = 10)]
        local: bool,

        /// Export only global aliases.
        #[arg(long, conflicts_with = "local", help_heading = headings::ALIAS_CONFIGURATION, display_order = 20)]
        global: bool,
    },

    /// Import aliases from a JSON file
    ///
    /// Imports aliases from an export file. Handles conflicts with existing
    /// aliases using the specified strategy.
    ///
    /// Examples:
    ///   sqry alias import aliases.json                  # Import to local
    ///   sqry alias import aliases.json --global         # Import to global
    ///   sqry alias import aliases.json --on-conflict skip
    #[command(verbatim_doc_comment)]
    Import {
        /// Input file path (use - for stdin).
        #[arg(value_name = "FILE", help_heading = headings::ALIAS_INPUT, display_order = 10)]
        file: String,

        /// Import to local storage (default).
        #[arg(long, conflicts_with = "global", help_heading = headings::ALIAS_CONFIGURATION, display_order = 10)]
        local: bool,

        /// Import to global storage.
        #[arg(long, conflicts_with = "local", help_heading = headings::ALIAS_CONFIGURATION, display_order = 20)]
        global: bool,

        /// How to handle conflicts with existing aliases.
        #[arg(long, value_enum, default_value = "error", help_heading = headings::ALIAS_CONFIGURATION, display_order = 30)]
        on_conflict: ImportConflictArg,

        /// Preview import without making changes.
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        dry_run: bool,
    },
}

/// History management subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum HistoryAction {
    /// List recent query history
    ///
    /// Shows recently executed queries with their timestamps, commands,
    /// and execution status.
    ///
    /// Examples:
    ///   sqry history list              # List recent (default 100)
    ///   sqry history list --limit 50   # Last 50 entries
    ///   sqry history list --json       # JSON output
    #[command(verbatim_doc_comment)]
    List {
        /// Maximum number of entries to show.
        #[arg(long, short = 'n', default_value = "100", help_heading = headings::HISTORY_CONFIGURATION, display_order = 10)]
        limit: usize,
    },

    /// Search query history
    ///
    /// Searches history entries by pattern. The pattern is matched
    /// against command names and arguments.
    ///
    /// Examples:
    ///   sqry history search "function"    # Find queries with "function"
    ///   sqry history search "callers:"    # Find caller queries
    #[command(verbatim_doc_comment)]
    Search {
        /// Search pattern (matched against command and args).
        #[arg(value_name = "PATTERN", help_heading = headings::HISTORY_INPUT, display_order = 10)]
        pattern: String,

        /// Maximum number of results.
        #[arg(long, short = 'n', default_value = "100", help_heading = headings::HISTORY_CONFIGURATION, display_order = 10)]
        limit: usize,
    },

    /// Clear query history
    ///
    /// Removes history entries. Can clear all entries or only those
    /// older than a specified duration.
    ///
    /// Examples:
    ///   sqry history clear               # Clear all (requires --confirm)
    ///   sqry history clear --older 30d   # Clear entries older than 30 days
    ///   sqry history clear --older 1w    # Clear entries older than 1 week
    #[command(verbatim_doc_comment)]
    Clear {
        /// Remove only entries older than this duration (e.g., 30d, 1w, 24h).
        #[arg(long, value_name = "DURATION", help_heading = headings::HISTORY_CONFIGURATION, display_order = 10)]
        older: Option<String>,

        /// Confirm clearing history (required when clearing all).
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        confirm: bool,
    },

    /// Show history statistics
    ///
    /// Displays aggregate statistics about query history including
    /// total entries, most used commands, and storage information.
    Stats,
}

/// Insights management subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum InsightsAction {
    /// Show usage summary for a time period
    ///
    /// Displays aggregated usage statistics including query counts,
    /// timing metrics, and workflow patterns.
    ///
    /// Examples:
    ///   sqry insights show                    # Current week
    ///   sqry insights show --week 2025-W50    # Specific week
    ///   sqry insights show --json             # JSON output
    #[command(verbatim_doc_comment)]
    Show {
        /// ISO week to display (e.g., 2025-W50). Defaults to current week.
        #[arg(long, short = 'w', value_name = "WEEK", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 10)]
        week: Option<String>,
    },

    /// Show or modify uses configuration
    ///
    /// View the current configuration or change settings like
    /// enabling/disabling uses capture.
    ///
    /// Examples:
    ///   sqry insights config                  # Show current config
    ///   sqry insights config --enable         # Enable uses capture
    ///   sqry insights config --disable        # Disable uses capture
    ///   sqry insights config --retention 90   # Set retention to 90 days
    #[command(verbatim_doc_comment)]
    Config {
        /// Enable uses capture.
        #[arg(long, conflicts_with = "disable", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 10)]
        enable: bool,

        /// Disable uses capture.
        #[arg(long, conflicts_with = "enable", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 20)]
        disable: bool,

        /// Set retention period in days.
        #[arg(long, value_name = "DAYS", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 30)]
        retention: Option<u32>,
    },

    /// Show storage status and statistics
    ///
    /// Displays information about the uses storage including
    /// total size, file count, and date range of stored events.
    ///
    /// Example:
    ///   sqry insights status
    Status,

    /// Clean up old event data
    ///
    /// Removes event logs older than the specified duration.
    /// Uses the configured retention period if --older is not specified.
    ///
    /// Examples:
    ///   sqry insights prune                   # Use configured retention
    ///   sqry insights prune --older 90d       # Prune older than 90 days
    ///   sqry insights prune --dry-run         # Preview without deleting
    #[command(verbatim_doc_comment)]
    Prune {
        /// Remove entries older than this duration (e.g., 30d, 90d).
        /// Defaults to configured retention period.
        #[arg(long, value_name = "DURATION", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 10)]
        older: Option<String>,

        /// Preview deletions without removing files.
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 10)]
        dry_run: bool,
    },

    /// Generate an anonymous usage snapshot for sharing
    ///
    /// Creates a privacy-safe snapshot of your usage patterns that you can
    /// share with the sqry community or attach to bug reports.  All fields
    /// are strongly-typed enums and numerics — no code content, paths, or
    /// identifiers are ever included.
    ///
    /// Uses are disabled → exits 1.  Empty weeks produce a valid snapshot
    /// with total_uses: 0 (not an error).
    ///
    /// JSON output is controlled by the global --json flag.
    ///
    /// Examples:
    ///   sqry insights share                        # Current week, human-readable
    ///   sqry --json insights share                 # JSON to stdout
    ///   sqry insights share --output snap.json     # Write JSON to file
    ///   sqry insights share --week 2026-W09        # Specific week
    ///   sqry insights share --from 2026-W07 --to 2026-W09   # Merge 3 weeks
    ///   sqry insights share --dry-run              # Preview without writing
    #[cfg(feature = "share")]
    #[command(verbatim_doc_comment)]
    Share {
        /// Specific ISO week to share (e.g., 2026-W09). Defaults to current week.
        /// Conflicts with --from / --to.
        #[arg(long, value_name = "WEEK", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 10,
              conflicts_with_all = ["from", "to"])]
        week: Option<String>,

        /// Start of multi-week range (e.g., 2026-W07). Requires --to.
        #[arg(long, value_name = "WEEK", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 11,
              conflicts_with = "week", requires = "to")]
        from: Option<String>,

        /// End of multi-week range (e.g., 2026-W09). Requires --from.
        #[arg(long, value_name = "WEEK", help_heading = headings::INSIGHTS_CONFIGURATION, display_order = 12,
              conflicts_with = "week", requires = "from")]
        to: Option<String>,

        /// Write JSON snapshot to this file.
        #[arg(long, short = 'o', value_name = "FILE", help_heading = headings::INSIGHTS_OUTPUT, display_order = 20,
              conflicts_with = "dry_run")]
        output: Option<PathBuf>,

        /// Preview what would be shared without writing a file.
        #[arg(long, help_heading = headings::SAFETY_CONTROL, display_order = 30,
              conflicts_with = "output")]
        dry_run: bool,
    },
}

/// Import conflict resolution strategies
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum ImportConflictArg {
    /// Fail on any conflict (default)
    Error,
    /// Skip conflicting aliases
    Skip,
    /// Overwrite existing aliases
    Overwrite,
}

/// Shell types for completions
#[derive(Debug, Clone, Copy, ValueEnum)]
#[allow(missing_docs)]
#[allow(clippy::enum_variant_names)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell")]
    PowerShell,
    Elvish,
}

/// Symbol types for filtering
#[derive(Debug, Clone, Copy, ValueEnum)]
#[allow(missing_docs)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Struct,
    Enum,
    Interface,
    Trait,
    Variable,
    Constant,
    Type,
    Module,
    Namespace,
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolKind::Function => write!(f, "function"),
            SymbolKind::Class => write!(f, "class"),
            SymbolKind::Method => write!(f, "method"),
            SymbolKind::Struct => write!(f, "struct"),
            SymbolKind::Enum => write!(f, "enum"),
            SymbolKind::Interface => write!(f, "interface"),
            SymbolKind::Trait => write!(f, "trait"),
            SymbolKind::Variable => write!(f, "variable"),
            SymbolKind::Constant => write!(f, "constant"),
            SymbolKind::Type => write!(f, "type"),
            SymbolKind::Module => write!(f, "module"),
            SymbolKind::Namespace => write!(f, "namespace"),
        }
    }
}

/// Index validation strictness modes
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum ValidationMode {
    /// Skip validation entirely (fastest)
    Off,
    /// Log warnings but continue (default)
    Warn,
    /// Abort on validation errors
    Fail,
}

/// Metrics export format for validation status
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum MetricsFormat {
    /// JSON format (default, structured data)
    #[value(alias = "jsn")]
    Json,
    /// Prometheus `OpenMetrics` text format
    #[value(alias = "prom")]
    Prometheus,
}

/// Classpath analysis depth for the `--classpath-depth` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ClasspathDepthArg {
    /// Include all transitive dependencies.
    Full,
    /// Only direct (compile-scope) dependencies.
    Shallow,
}

// Helper function to get the command with applied taxonomy
impl Cli {
    /// Get the command with taxonomy headings applied
    #[must_use]
    pub fn command_with_taxonomy() -> clap::Command {
        use clap::CommandFactory;
        let cmd = Self::command();
        headings::apply_root_layout(cmd)
    }

    /// Validate CLI arguments that have dependencies not enforceable via clap
    ///
    /// Returns an error message if validation fails, None if valid.
    #[must_use]
    pub fn validate(&self) -> Option<&'static str> {
        let tabular_mode = self.csv || self.tsv;

        // --headers, --columns, and --raw-csv require CSV or TSV mode
        if self.headers && !tabular_mode {
            return Some("--headers requires --csv or --tsv");
        }
        if self.columns.is_some() && !tabular_mode {
            return Some("--columns requires --csv or --tsv");
        }
        if self.raw_csv && !tabular_mode {
            return Some("--raw-csv requires --csv or --tsv");
        }

        if tabular_mode && let Err(msg) = output::parse_columns(self.columns.as_ref()) {
            return Some(Box::leak(msg.into_boxed_str()));
        }

        None
    }

    /// Get the search path, defaulting to current directory if not specified
    #[must_use]
    pub fn search_path(&self) -> &str {
        self.path.as_deref().unwrap_or(".")
    }

    /// Resolve the path-scoped subcommand path, applying the global
    /// `--workspace` / `SQRY_WORKSPACE_FILE` fallback (`STEP_8`).
    ///
    /// Precedence (least-surprise, codified in
    /// `docs/development/workspace-aware-cross-repo/03_IMPLEMENTATION_PLAN.md`
    /// Step 8):
    ///   1. Explicit positional `<path>` on the subcommand wins.
    ///   2. The global `--workspace <PATH>` flag (or `SQRY_WORKSPACE_FILE`
    ///      environment variable; CLI flag wins on conflict) is the fallback.
    ///   3. Otherwise, the top-level `cli.path` shorthand or `"."`.
    ///
    /// Callers pass `positional` from the subcommand's own positional argument.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace fallback (from `--workspace` or
    /// `SQRY_WORKSPACE_FILE`) is set but contains non-UTF-8 bytes. The
    /// downstream CLI pipeline (positional `<path>` arguments and
    /// `commands::run_index` / `commands::run_query` signatures) operates on
    /// `&str`, so a non-UTF-8 workspace path cannot be propagated faithfully —
    /// silently falling back to `"."` (or the top-level `cli.path`) would
    /// violate the documented precedence semantics. Surface the failure
    /// instead so the operator can supply a UTF-8 path. (`STEP_8` codex iter1
    /// fix.)
    pub fn resolve_subcommand_path<'a>(
        &'a self,
        positional: Option<&'a str>,
    ) -> anyhow::Result<&'a str> {
        if let Some(p) = positional {
            return Ok(p);
        }
        if let Some(ws) = self.workspace.as_deref() {
            return ws.to_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "--workspace / SQRY_WORKSPACE_FILE path is not valid UTF-8: {}. \
                     sqry's path-scoped subcommands require UTF-8 paths; supply a \
                     valid UTF-8 workspace path or pass an explicit positional \
                     argument.",
                    ws.display()
                )
            });
        }
        Ok(self.search_path())
    }

    /// Returns the workspace path supplied via `--workspace` /
    /// `SQRY_WORKSPACE_FILE`, if any (`STEP_8`).
    ///
    /// Surfaced for downstream consumers (LSP/MCP/test harnesses); the
    /// CLI binary itself currently routes through `resolve_subcommand_path`,
    /// so the binary build flags this as unused.
    #[allow(dead_code)]
    #[must_use]
    pub fn workspace_path(&self) -> Option<&std::path::Path> {
        self.workspace.as_deref()
    }

    /// Return the plugin-selection arguments for the active subcommand.
    #[must_use]
    pub fn plugin_selection_args(&self) -> PluginSelectionArgs {
        match self.command.as_deref() {
            Some(
                Command::Query {
                    plugin_selection, ..
                }
                | Command::Index {
                    plugin_selection, ..
                }
                | Command::Update {
                    plugin_selection, ..
                }
                | Command::Watch {
                    plugin_selection, ..
                },
            ) => plugin_selection.clone(),
            _ => PluginSelectionArgs::default(),
        }
    }

    /// Check if tabular output mode is enabled
    #[allow(dead_code)]
    #[must_use]
    pub fn is_tabular_output(&self) -> bool {
        self.csv || self.tsv
    }

    /// Create pager configuration from CLI flags
    ///
    /// Returns `PagerConfig` based on `--pager`, `--no-pager`, and `--pager-cmd` flags.
    ///
    /// # Structured Output Handling
    ///
    /// For machine-readable formats (JSON, CSV, TSV), paging is disabled by default
    /// to avoid breaking pipelines. Use `--pager` to explicitly enable paging for
    /// these formats.
    #[must_use]
    pub fn pager_config(&self) -> crate::output::PagerConfig {
        // Structured output bypasses pager unless --pager is explicit
        let is_structured_output = self.json || self.csv || self.tsv;
        let effective_no_pager = self.no_pager || (is_structured_output && !self.pager);

        crate::output::PagerConfig::from_cli_flags(
            self.pager,
            effective_no_pager,
            self.pager_cmd.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;

    /// Guard: keep the `Command` enum from silently ballooning.
    /// If this fails, consider extracting the largest variant into a Box<T>.
    #[test]
    fn test_command_enum_size() {
        let size = std::mem::size_of::<Command>();
        assert!(
            size <= 256,
            "Command enum is {size} bytes, should be <= 256"
        );
    }

    large_stack_test! {
    #[test]
    fn test_cli_parse_basic_search() {
        let cli = Cli::parse_from(["sqry", "main"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.pattern, Some("main".to_string()));
        assert_eq!(cli.path, None); // Defaults to None, use cli.search_path() to get "."
        assert_eq!(cli.search_path(), ".");
    }
    }

    large_stack_test! {
    #[test]
    fn test_cli_parse_with_path() {
        let cli = Cli::parse_from(["sqry", "test", "src/"]);
        assert_eq!(cli.pattern, Some("test".to_string()));
        assert_eq!(cli.path, Some("src/".to_string()));
        assert_eq!(cli.search_path(), "src/");
    }
    }

    large_stack_test! {
    #[test]
    fn test_cli_parse_search_subcommand() {
        let cli = Cli::parse_from(["sqry", "search", "main"]);
        assert!(matches!(cli.command.as_deref(), Some(Command::Search { .. })));
    }
    }

    large_stack_test! {
    #[test]
    fn test_cli_parse_query_subcommand() {
        let cli = Cli::parse_from(["sqry", "query", "kind:function"]);
        assert!(matches!(cli.command.as_deref(), Some(Command::Query { .. })));
    }
    }

    large_stack_test! {
    #[test]
    fn test_cli_flags() {
        let cli = Cli::parse_from(["sqry", "main", "--json", "--no-color", "--ignore-case"]);
        assert!(cli.json);
        assert!(cli.no_color);
        assert!(cli.ignore_case);
    }
    }

    large_stack_test! {
    #[test]
    fn test_validation_mode_default() {
        let cli = Cli::parse_from(["sqry", "index"]);
        assert_eq!(cli.validate, ValidationMode::Warn);
        assert!(!cli.auto_rebuild);
    }
    }

    large_stack_test! {
    #[test]
    fn test_validation_mode_flags() {
        let cli = Cli::parse_from(["sqry", "index", "--validate", "fail", "--auto-rebuild"]);
        assert_eq!(cli.validate, ValidationMode::Fail);
        assert!(cli.auto_rebuild);
    }
    }

    large_stack_test! {
    #[test]
    fn test_plugin_selection_flags_parse() {
        let cli = Cli::parse_from([
            "sqry",
            "index",
            "--include-high-cost",
            "--enable-plugin",
            "json",
            "--disable-plugin",
            "rust",
        ]);
        let plugin_selection = cli.plugin_selection_args();
        assert!(plugin_selection.include_high_cost);
        assert_eq!(plugin_selection.enable_plugins, vec!["json".to_string()]);
        assert_eq!(plugin_selection.disable_plugins, vec!["rust".to_string()]);
    }
    }

    large_stack_test! {
    #[test]
    fn test_plugin_selection_language_aliases_parse() {
        let cli = Cli::parse_from([
            "sqry",
            "index",
            "--enable-language",
            "json",
            "--disable-language",
            "rust",
        ]);
        let plugin_selection = cli.plugin_selection_args();
        assert_eq!(plugin_selection.enable_plugins, vec!["json".to_string()]);
        assert_eq!(plugin_selection.disable_plugins, vec!["rust".to_string()]);
    }
    }

    large_stack_test! {
    #[test]
    fn test_validate_rejects_invalid_columns() {
        let cli = Cli::parse_from([
            "sqry",
            "--csv",
            "--columns",
            "name,unknown",
            "query",
            "path",
        ]);
        let msg = cli.validate().expect("validation should fail");
        assert!(msg.contains("Unknown column"), "Unexpected message: {msg}");
    }
    }

    large_stack_test! {
    #[test]
    fn test_index_rebuild_alias_sets_force() {
        // Verify --rebuild is an alias for --force
        let cli = Cli::parse_from(["sqry", "index", "--rebuild", "."]);
        if let Some(Command::Index { force, .. }) = cli.command.as_deref() {
            assert!(force, "--rebuild should set force=true");
        } else {
            panic!("Expected Index command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_index_force_still_works() {
        // Ensure --force continues to work (backward compat)
        let cli = Cli::parse_from(["sqry", "index", "--force", "."]);
        if let Some(Command::Index { force, .. }) = cli.command.as_deref() {
            assert!(force, "--force should set force=true");
        } else {
            panic!("Expected Index command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_graph_deps_alias() {
        // Verify "deps" is an alias for dependency-tree
        let cli = Cli::parse_from(["sqry", "graph", "deps", "main"]);
        assert!(matches!(
            cli.command.as_deref(),
            Some(Command::Graph {
                operation: GraphOperation::DependencyTree { .. },
                ..
            })
        ));
    }
    }

    large_stack_test! {
    #[test]
    fn test_graph_cyc_alias() {
        let cli = Cli::parse_from(["sqry", "graph", "cyc"]);
        assert!(matches!(
            cli.command.as_deref(),
            Some(Command::Graph {
                operation: GraphOperation::Cycles { .. },
                ..
            })
        ));
    }
    }

    large_stack_test! {
    #[test]
    fn test_graph_cx_alias() {
        let cli = Cli::parse_from(["sqry", "graph", "cx"]);
        assert!(matches!(
            cli.command.as_deref(),
            Some(Command::Graph {
                operation: GraphOperation::Complexity { .. },
                ..
            })
        ));
    }
    }

    large_stack_test! {
    #[test]
    fn test_graph_nodes_args() {
        let cli = Cli::parse_from([
            "sqry",
            "graph",
            "nodes",
            "--kind",
            "function",
            "--languages",
            "rust",
            "--file",
            "src/",
            "--name",
            "main",
            "--qualified-name",
            "crate::main",
            "--limit",
            "5",
            "--offset",
            "2",
            "--full-paths",
        ]);
        if let Some(Command::Graph {
            operation:
                GraphOperation::Nodes {
                    kind,
                    languages,
                    file,
                    name,
                    qualified_name,
                    limit,
                    offset,
                    full_paths,
                },
            ..
        }) = cli.command.as_deref()
        {
            assert_eq!(kind, &Some("function".to_string()));
            assert_eq!(languages, &Some("rust".to_string()));
            assert_eq!(file, &Some("src/".to_string()));
            assert_eq!(name, &Some("main".to_string()));
            assert_eq!(qualified_name, &Some("crate::main".to_string()));
            assert_eq!(*limit, 5);
            assert_eq!(*offset, 2);
            assert!(full_paths);
        } else {
            panic!("Expected Graph Nodes command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_graph_edges_args() {
        let cli = Cli::parse_from([
            "sqry",
            "graph",
            "edges",
            "--kind",
            "calls",
            "--from",
            "main",
            "--to",
            "worker",
            "--from-lang",
            "rust",
            "--to-lang",
            "python",
            "--file",
            "src/main.rs",
            "--limit",
            "10",
            "--offset",
            "1",
            "--full-paths",
        ]);
        if let Some(Command::Graph {
            operation:
                GraphOperation::Edges {
                    kind,
                    from,
                    to,
                    from_lang,
                    to_lang,
                    file,
                    limit,
                    offset,
                    full_paths,
                },
            ..
        }) = cli.command.as_deref()
        {
            assert_eq!(kind, &Some("calls".to_string()));
            assert_eq!(from, &Some("main".to_string()));
            assert_eq!(to, &Some("worker".to_string()));
            assert_eq!(from_lang, &Some("rust".to_string()));
            assert_eq!(to_lang, &Some("python".to_string()));
            assert_eq!(file, &Some("src/main.rs".to_string()));
            assert_eq!(*limit, 10);
            assert_eq!(*offset, 1);
            assert!(full_paths);
        } else {
            panic!("Expected Graph Edges command");
        }
    }
    }

    // ===== Pager Tests (P2-29) =====

    large_stack_test! {
    #[test]
    fn test_pager_flag_default() {
        let cli = Cli::parse_from(["sqry", "query", "kind:function"]);
        assert!(!cli.pager);
        assert!(!cli.no_pager);
        assert!(cli.pager_cmd.is_none());
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_flag() {
        let cli = Cli::parse_from(["sqry", "--pager", "query", "kind:function"]);
        assert!(cli.pager);
        assert!(!cli.no_pager);
    }
    }

    large_stack_test! {
    #[test]
    fn test_no_pager_flag() {
        let cli = Cli::parse_from(["sqry", "--no-pager", "query", "kind:function"]);
        assert!(!cli.pager);
        assert!(cli.no_pager);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_cmd_flag() {
        let cli = Cli::parse_from([
            "sqry",
            "--pager-cmd",
            "bat --style=plain",
            "query",
            "kind:function",
        ]);
        assert_eq!(cli.pager_cmd, Some("bat --style=plain".to_string()));
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_and_no_pager_conflict() {
        // These flags conflict and clap should reject
        let result =
            Cli::try_parse_from(["sqry", "--pager", "--no-pager", "query", "kind:function"]);
        assert!(result.is_err());
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_flags_global() {
        // Pager flags work with any subcommand
        let cli = Cli::parse_from(["sqry", "--no-pager", "search", "test"]);
        assert!(cli.no_pager);

        let cli = Cli::parse_from(["sqry", "--pager", "index"]);
        assert!(cli.pager);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_config_json_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // JSON output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--json", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_config_csv_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // CSV output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--csv", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_config_tsv_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // TSV output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--tsv", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_config_json_with_explicit_pager() {
        use crate::output::pager::PagerMode;

        // JSON with explicit --pager should enable pager
        let cli = Cli::parse_from(["sqry", "--json", "--pager", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Always);
    }
    }

    large_stack_test! {
    #[test]
    fn test_pager_config_text_output_auto() {
        use crate::output::pager::PagerMode;

        // Text output (default) should use auto pager mode
        let cli = Cli::parse_from(["sqry", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Auto);
    }
    }

    // ===== Macro boundary CLI tests =====

    large_stack_test! {
    #[test]
    fn test_cache_expand_args_parsing() {
        let cli = Cli::parse_from([
            "sqry", "cache", "expand",
            "--refresh",
            "--crate-name", "my_crate",
            "--dry-run",
            "--output", "/tmp/expand-out",
        ]);
        if let Some(Command::Cache { action }) = cli.command.as_deref() {
            match action {
                CacheAction::Expand {
                    refresh,
                    crate_name,
                    dry_run,
                    output,
                } => {
                    assert!(refresh);
                    assert_eq!(crate_name.as_deref(), Some("my_crate"));
                    assert!(dry_run);
                    assert_eq!(output.as_deref(), Some(std::path::Path::new("/tmp/expand-out")));
                }
                _ => panic!("Expected CacheAction::Expand"),
            }
        } else {
            panic!("Expected Cache command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_cache_expand_defaults() {
        let cli = Cli::parse_from(["sqry", "cache", "expand"]);
        if let Some(Command::Cache { action }) = cli.command.as_deref() {
            match action {
                CacheAction::Expand {
                    refresh,
                    crate_name,
                    dry_run,
                    output,
                } => {
                    assert!(!refresh);
                    assert!(crate_name.is_none());
                    assert!(!dry_run);
                    assert!(output.is_none());
                }
                _ => panic!("Expected CacheAction::Expand"),
            }
        } else {
            panic!("Expected Cache command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_index_macro_flags_parsing() {
        let cli = Cli::parse_from([
            "sqry", "index",
            "--enable-macro-expansion",
            "--cfg", "test",
            "--cfg", "unix",
            "--expand-cache", "/tmp/expand",
        ]);
        if let Some(Command::Index {
            enable_macro_expansion,
            cfg_flags,
            expand_cache,
            ..
        }) = cli.command.as_deref()
        {
            assert!(enable_macro_expansion);
            assert_eq!(cfg_flags, &["test".to_string(), "unix".to_string()]);
            assert_eq!(expand_cache.as_deref(), Some(std::path::Path::new("/tmp/expand")));
        } else {
            panic!("Expected Index command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_index_macro_flags_defaults() {
        let cli = Cli::parse_from(["sqry", "index"]);
        if let Some(Command::Index {
            enable_macro_expansion,
            cfg_flags,
            expand_cache,
            ..
        }) = cli.command.as_deref()
        {
            assert!(!enable_macro_expansion);
            assert!(cfg_flags.is_empty());
            assert!(expand_cache.is_none());
        } else {
            panic!("Expected Index command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_search_macro_flags_parsing() {
        let cli = Cli::parse_from([
            "sqry", "search", "test_fn",
            "--cfg-filter", "test",
            "--include-generated",
            "--macro-boundaries",
        ]);
        if let Some(Command::Search {
            cfg_filter,
            include_generated,
            macro_boundaries,
            ..
        }) = cli.command.as_deref()
        {
            assert_eq!(cfg_filter.as_deref(), Some("test"));
            assert!(include_generated);
            assert!(macro_boundaries);
        } else {
            panic!("Expected Search command");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn test_search_macro_flags_defaults() {
        let cli = Cli::parse_from(["sqry", "search", "test_fn"]);
        if let Some(Command::Search {
            cfg_filter,
            include_generated,
            macro_boundaries,
            ..
        }) = cli.command.as_deref()
        {
            assert!(cfg_filter.is_none());
            assert!(!include_generated);
            assert!(!macro_boundaries);
        } else {
            panic!("Expected Search command");
        }
    }
    }

    // ===== Daemon subcommand CLI tests (Task 10 U2) =====

    large_stack_test! {
    #[test]
    fn daemon_start_parses() {
        let cli = Cli::parse_from(["sqry", "daemon", "start"]);
        if let Some(Command::Daemon { action }) = cli.command.as_deref() {
            match action.as_ref() {
                DaemonAction::Start { sqryd_path, timeout } => {
                    assert!(sqryd_path.is_none(), "sqryd_path should default to None");
                    assert_eq!(*timeout, 10, "default timeout should be 10");
                }
                other => panic!("Expected DaemonAction::Start, got {other:?}"),
            }
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn daemon_stop_parses() {
        let cli = Cli::parse_from(["sqry", "daemon", "stop", "--timeout", "30"]);
        if let Some(Command::Daemon { action }) = cli.command.as_deref() {
            match action.as_ref() {
                DaemonAction::Stop { timeout } => {
                    assert_eq!(*timeout, 30, "timeout should be 30");
                }
                other => panic!("Expected DaemonAction::Stop, got {other:?}"),
            }
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn daemon_status_json_parses() {
        let cli = Cli::parse_from(["sqry", "daemon", "status", "--json"]);
        if let Some(Command::Daemon { action }) = cli.command.as_deref() {
            match action.as_ref() {
                DaemonAction::Status { json } => {
                    assert!(*json, "--json flag should be true");
                }
                other => panic!("Expected DaemonAction::Status, got {other:?}"),
            }
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn daemon_logs_follow_parses() {
        let cli = Cli::parse_from(["sqry", "daemon", "logs", "--follow", "--lines", "100"]);
        if let Some(Command::Daemon { action }) = cli.command.as_deref() {
            match action.as_ref() {
                DaemonAction::Logs { lines, follow } => {
                    assert_eq!(*lines, 100, "lines should be 100");
                    assert!(*follow, "--follow flag should be true");
                }
                other => panic!("Expected DaemonAction::Logs, got {other:?}"),
            }
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    #[test]
    fn daemon_load_parses() {
        let cli = Cli::parse_from(["sqry", "daemon", "load", "/some/workspace"]);
        if let Some(Command::Daemon { action }) = cli.command.as_deref() {
            match action.as_ref() {
                DaemonAction::Load { path } => {
                    assert_eq!(
                        path,
                        &std::path::PathBuf::from("/some/workspace"),
                        "path should be /some/workspace"
                    );
                }
                other => panic!("Expected DaemonAction::Load, got {other:?}"),
            }
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }
}
