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
    pub command: Option<Command>,

    /// Search pattern (shorthand for 'search' command)
    ///
    /// Supports regex patterns by default. Use --exact for literal matching.
    #[arg(required = false)]
    pub pattern: Option<String>,

    /// Search path (defaults to current directory)
    #[arg(required = false)]
    pub path: Option<String>,

    /// Output format as JSON
    #[arg(long, short = 'j', global = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 10)]
    pub json: bool,

    /// Output format as CSV (comma-separated values)
    ///
    /// RFC 4180 compliant CSV output. Use with --headers to include column names.
    /// By default, formula-triggering characters are prefixed with single quote
    /// for Excel/LibreOffice safety. Use --raw-csv to disable this protection.
    #[arg(long, global = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 12)]
    pub csv: bool,

    /// Output format as TSV (tab-separated values)
    ///
    /// Tab-delimited output for easy Unix pipeline processing.
    /// Newlines and tabs in field values are replaced with spaces.
    #[arg(long, global = true, group = "output_format", help_heading = headings::COMMON_OPTIONS, display_order = 13)]
    pub tsv: bool,

    /// Include header row in CSV/TSV output
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, help_heading = headings::OUTPUT_CONTROL, display_order = 11)]
    pub headers: bool,

    /// Columns to include in CSV/TSV output (comma-separated)
    ///
    /// Available columns: `name`, `qualified_name`, `kind`, `file`, `line`, `column`,
    /// `end_line`, `end_column`, `language`, `preview`
    ///
    /// Example: --columns name,file,line
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, value_name = "COLUMNS", help_heading = headings::OUTPUT_CONTROL, display_order = 12)]
    pub columns: Option<String>,

    /// Output raw CSV without formula injection protection
    ///
    /// By default, values starting with =, +, -, @, tab, or carriage return
    /// are prefixed with single quote to prevent Excel/LibreOffice formula
    /// injection attacks. Use this flag to disable protection for programmatic
    /// processing where raw values are needed.
    ///
    /// Requires --csv or --tsv to be specified.
    #[arg(long, global = true, help_heading = headings::OUTPUT_CONTROL, display_order = 13)]
    pub raw_csv: bool,

    /// Show code context around matches (number of lines before/after)
    #[arg(
        long, short = 'p', global = true, value_name = "LINES",
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
    #[arg(long, global = true, help_heading = headings::COMMON_OPTIONS, display_order = 14)]
    pub no_color: bool,

    /// Select output color theme (default, dark, light, none)
    #[arg(
        long,
        value_enum,
        default_value = "default",
        global = true,
        help_heading = headings::COMMON_OPTIONS,
        display_order = 15
    )]
    pub theme: crate::output::ThemeName,

    /// Sort results (opt-in)
    #[arg(
        long,
        value_enum,
        global = true,
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

    /// Exact match (disable regex)
    ///
    /// Applies to search mode (top-level shorthand and `sqry search`).
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
    #[arg(long, global = true, help_heading = headings::SEARCH_MODES_FUZZY, display_order = 52)]
    pub fuzzy_fields: bool,

    /// Maximum edit distance for fuzzy field correction
    #[arg(
        long,
        default_value_t = 2,
        global = true,
        help_heading = headings::SEARCH_MODES_FUZZY,
        display_order = 53
    )]
    pub fuzzy_field_distance: usize,

    /// Maximum number of results to return
    ///
    /// Limits the output to prevent overwhelming LLM context windows.
    /// Defaults: search=100, query=1000, fuzzy=50
    #[arg(long, global = true, help_heading = headings::OUTPUT_CONTROL, display_order = 20)]
    pub limit: Option<usize>,

    /// List enabled languages and exit
    #[arg(long, global = true, help_heading = headings::COMMON_OPTIONS, display_order = 30)]
    pub list_languages: bool,

    /// Print cache telemetry to stderr after the command completes
    #[arg(long, global = true, help_heading = headings::COMMON_OPTIONS, display_order = 40)]
    pub debug_cache: bool,

    /// Display fully qualified symbol names in CLI output.
    ///
    /// Helpful for disambiguating relation queries (callers/callees) where
    /// multiple namespaces define the same method name.
    #[arg(long, global = true, help_heading = headings::OUTPUT_CONTROL, display_order = 30)]
    pub qualified_names: bool,

    // ===== Index Validation Flags (P1-14) =====
    /// Index validation strictness level (off, warn, fail)
    ///
    /// Controls how to handle index corruption during load:
    /// - off: Skip validation entirely (fastest)
    /// - warn: Log warnings but continue (default)
    /// - fail: Abort on validation errors
    #[arg(long, value_enum, default_value = "warn", global = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 40)]
    pub validate: ValidationMode,

    /// Automatically rebuild index if validation fails
    ///
    /// When set, if index validation fails in strict mode, sqry will
    /// automatically rebuild the index once and retry. Useful for
    /// recovering from transient corruption without manual intervention.
    #[arg(long, requires = "validate", global = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 41)]
    pub auto_rebuild: bool,

    /// Maximum ratio of dangling references before rebuild (0.0-1.0)
    ///
    /// Sets the threshold for dangling reference errors during validation.
    /// Default: 0.05 (5%). If more than this ratio of symbols have dangling
    /// references, validation will fail in strict mode.
    #[arg(long, value_name = "RATIO", global = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 42)]
    pub threshold_dangling_refs: Option<f64>,

    /// Maximum ratio of orphaned files before rebuild (0.0-1.0)
    ///
    /// Sets the threshold for orphaned file errors during validation.
    /// Default: 0.20 (20%). If more than this ratio of indexed files are
    /// orphaned (no longer exist on disk), validation will fail.
    #[arg(long, value_name = "RATIO", global = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 43)]
    pub threshold_orphaned_files: Option<f64>,

    /// Maximum ratio of ID gaps before warning (0.0-1.0)
    ///
    /// Sets the threshold for ID gap warnings during validation.
    /// Default: 0.10 (10%). If more than this ratio of symbol IDs have gaps,
    /// validation will warn or fail depending on strictness.
    #[arg(long, value_name = "RATIO", global = true, help_heading = headings::INDEX_CONFIGURATION, display_order = 44)]
    pub threshold_id_gaps: Option<f64>,

    // ===== Hybrid Search Flags (FR-2025-002) =====
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

    /// Search for symbols by pattern (simple pattern matching)
    ///
    /// Fast pattern-based search using regex or literal matching.
    /// Use this for quick searches with simple text patterns.
    ///
    /// For complex queries with boolean logic and AST predicates, use 'query' instead.
    ///
    /// Examples:
    ///   sqry search "test.*"           # Find symbols matching regex
    ///   sqry search "test" --save-as find-tests  # Save as alias
    ///   sqry search "test" --validate fail       # Strict index validation
    ///
    /// For kind/language/fuzzy filtering, use the top-level shorthand:
    ///   sqry --kind function "test"    # Filter by kind
    ///   sqry --exact "main"            # Exact match
    ///   sqry --fuzzy "config"          # Fuzzy search
    ///
    /// See also: 'sqry query' for structured AST-aware queries
    #[command(display_order = 1, verbatim_doc_comment)]
    Search {
        /// Search pattern (regex or literal with --exact).
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
    },

    /// Execute AST-aware query (structured queries with boolean logic)
    ///
    /// Powerful structured queries using predicates and boolean operators.
    /// Use this for complex searches that combine multiple criteria.
    ///
    /// For simple pattern matching, use 'search' instead.
    ///
    /// Predicate examples:
    ///   - kind:function                 # Find functions
    ///   - name:test                     # Name contains 'test'
    ///   - lang:rust                     # Rust files only
    ///   - visibility:public             # Public symbols
    ///   - async:true                    # Async functions
    ///
    /// Boolean logic:
    ///   - kind:function AND name:test   # Functions with 'test' in name
    ///   - kind:class OR kind:struct     # All classes or structs
    ///   - lang:rust AND visibility:public  # Public Rust symbols
    ///
    /// Relation queries (14 languages with full support):
    ///   - callers:authenticate          # Who calls authenticate?
    ///   - callees:processData           # What does processData call?
    ///   - exports:UserService           # What does `UserService` export?
    ///   - imports:database              # What imports database?
    ///
    /// Supported for: JavaScript, TypeScript, Python, Java, Go, Rust, Ruby, PHP, Lua,
    /// C++, C#, Kotlin, Groovy, Svelte, Vue
    ///
    /// Saving as alias:
    ///   sqry query "kind:function AND name:test" --save-as test-funcs
    ///   sqry @test-funcs src/
    ///
    /// See also: 'sqry search' for simple pattern-based searches
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
        #[arg(long, short = 'f', default_value = "text", help_heading = headings::GRAPH_CONFIGURATION, display_order = 20)]
        format: String,

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

    /// Build symbol index for fast queries
    ///
    /// Creates a persistent index of all symbols in the specified directory.
    /// The index is saved to .sqry-index and speeds up subsequent queries.
    /// Uses parallel processing by default for faster indexing.
    #[command(display_order = 10)]
    Index {
        /// Directory to index (defaults to current directory).
        #[arg(help_heading = headings::INDEX_INPUT, display_order = 10)]
        path: Option<String>,

        /// Force rebuild even if index exists.
        #[arg(long, short = 'f', alias = "rebuild", help_heading = headings::INDEX_CONFIGURATION, display_order = 10)]
        force: bool,

        /// Show index status without building.
        ///
        /// Returns metadata about the existing index (age, symbol count, languages).
        /// Useful for LLM agents to check if indexing is needed.
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

        /// Disable index compression (P1-12: Index Compression).
        ///
        /// By default, indexes are compressed with zstd for faster load times
        /// and reduced disk space. Use this flag to store uncompressed indexes
        /// (useful for debugging or compatibility testing).
        #[arg(long, help_heading = headings::ADVANCED_CONFIGURATION, display_order = 20)]
        no_compress: bool,

        /// Metrics export format for validation status (json or prometheus).
        ///
        /// Used with --status --json to export validation metrics in different
        /// formats. Prometheus format outputs OpenMetrics-compatible text for
        /// monitoring systems. JSON format (default) provides structured data.
        #[arg(long, short = 'M', value_enum, default_value = "json", requires = "status", help_heading = headings::OUTPUT_CONTROL, display_order = 30)]
        metrics_format: MetricsFormat,
    },

    /// Build precomputed graph analyses for fast query performance
    ///
    /// Computes CSR adjacency, SCC (Strongly Connected Components), condensation DAGs,
    /// and 2-hop interval labels to eliminate O(V+E) query-time costs. Analysis files
    /// are persisted to .sqry/analysis/ and enable fast cycle detection, reachability
    /// queries, and path finding.
    ///
    /// Note: `sqry index` automatically runs analysis after building the graph.
    /// Use this command to rebuild analysis files without re-indexing.
    ///
    /// Examples:
    ///   sqry analyze                 # Build analyses for current index
    ///   sqry analyze --force         # Rebuild even if analyses exist
    #[command(display_order = 13, verbatim_doc_comment)]
    Analyze {
        /// Search path (defaults to current directory).
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        path: Option<String>,

        /// Force rebuild even if analysis files exist.
        #[arg(long, short = 'f', help_heading = headings::INDEX_CONFIGURATION, display_order = 10)]
        force: bool,
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
    /// Generate shell completion scripts for bash, zsh, fish, or `PowerShell`.
    /// Install by redirecting output to the appropriate location for your shell.
    ///
    /// Examples:
    ///   sqry completions bash > /`etc/bash_completion.d/sqry`
    ///   sqry completions zsh > ~/.zfunc/_sqry
    ///   sqry completions fish > ~/.config/fish/completions/sqry.fish
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
    /// unless you explicitly use --share (which generates a file, not
    /// a network request).
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
    #[command(alias = "imp", display_order = 24, verbatim_doc_comment)]
    Impact {
        /// Symbol to analyze.
        #[arg(help_heading = headings::SEARCH_INPUT, display_order = 10)]
        symbol: String,

        /// Search path (defaults to current directory).
        #[arg(long, help_heading = headings::SEARCH_INPUT, display_order = 20)]
        path: Option<String>,

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

        /// Override parallel query threads (reserved for future tuning).
        #[arg(long, help_heading = headings::PERFORMANCE_TUNING, display_order = 10)]
        threads: Option<usize>,
    },

    /// Emit aggregate statistics for the workspace
    Stats {
        /// Workspace root containing the .sqry-workspace file.
        #[arg(value_name = "WORKSPACE", help_heading = headings::WORKSPACE_INPUT, display_order = 10)]
        workspace: String,
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

    /// Check if tabular output mode is enabled
    #[allow(dead_code)] // Helper for future use
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

// Windows debug builds overflow the default 1MB stack when constructing
// clap's deep subcommand tree via Cli::parse_from (STATUS_STACK_OVERFLOW).
#[cfg(test)]
#[cfg(not(target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_basic_search() {
        let cli = Cli::parse_from(["sqry", "main"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.pattern, Some("main".to_string()));
        assert_eq!(cli.path, None); // Defaults to None, use cli.search_path() to get "."
        assert_eq!(cli.search_path(), ".");
    }

    #[test]
    fn test_cli_parse_with_path() {
        let cli = Cli::parse_from(["sqry", "test", "src/"]);
        assert_eq!(cli.pattern, Some("test".to_string()));
        assert_eq!(cli.path, Some("src/".to_string()));
        assert_eq!(cli.search_path(), "src/");
    }

    #[test]
    fn test_cli_parse_search_subcommand() {
        let cli = Cli::parse_from(["sqry", "search", "main"]);
        assert!(matches!(cli.command, Some(Command::Search { .. })));
    }

    #[test]
    fn test_cli_parse_query_subcommand() {
        let cli = Cli::parse_from(["sqry", "query", "kind:function"]);
        assert!(matches!(cli.command, Some(Command::Query { .. })));
    }

    #[test]
    fn test_cli_flags() {
        let cli = Cli::parse_from(["sqry", "main", "--json", "--no-color", "--ignore-case"]);
        assert!(cli.json);
        assert!(cli.no_color);
        assert!(cli.ignore_case);
    }

    #[test]
    fn test_validation_mode_default() {
        let cli = Cli::parse_from(["sqry", "index"]);
        assert_eq!(cli.validate, ValidationMode::Warn);
        assert!(!cli.auto_rebuild);
    }

    #[test]
    fn test_validation_mode_flags() {
        let cli = Cli::parse_from(["sqry", "index", "--validate", "fail", "--auto-rebuild"]);
        assert_eq!(cli.validate, ValidationMode::Fail);
        assert!(cli.auto_rebuild);
    }

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

    #[test]
    fn test_index_rebuild_alias_sets_force() {
        // Verify --rebuild is an alias for --force
        let cli = Cli::parse_from(["sqry", "index", "--rebuild", "."]);
        if let Some(Command::Index { force, .. }) = cli.command {
            assert!(force, "--rebuild should set force=true");
        } else {
            panic!("Expected Index command");
        }
    }

    #[test]
    fn test_index_force_still_works() {
        // Ensure --force continues to work (backward compat)
        let cli = Cli::parse_from(["sqry", "index", "--force", "."]);
        if let Some(Command::Index { force, .. }) = cli.command {
            assert!(force, "--force should set force=true");
        } else {
            panic!("Expected Index command");
        }
    }

    #[test]
    fn test_graph_deps_alias() {
        // Verify "deps" is an alias for dependency-tree
        let cli = Cli::parse_from(["sqry", "graph", "deps", "main"]);
        assert!(matches!(
            cli.command,
            Some(Command::Graph {
                operation: GraphOperation::DependencyTree { .. },
                ..
            })
        ));
    }

    #[test]
    fn test_graph_cyc_alias() {
        let cli = Cli::parse_from(["sqry", "graph", "cyc"]);
        assert!(matches!(
            cli.command,
            Some(Command::Graph {
                operation: GraphOperation::Cycles { .. },
                ..
            })
        ));
    }

    #[test]
    fn test_graph_cx_alias() {
        let cli = Cli::parse_from(["sqry", "graph", "cx"]);
        assert!(matches!(
            cli.command,
            Some(Command::Graph {
                operation: GraphOperation::Complexity { .. },
                ..
            })
        ));
    }

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
        }) = cli.command
        {
            assert_eq!(kind, Some("function".to_string()));
            assert_eq!(languages, Some("rust".to_string()));
            assert_eq!(file, Some("src/".to_string()));
            assert_eq!(name, Some("main".to_string()));
            assert_eq!(qualified_name, Some("crate::main".to_string()));
            assert_eq!(limit, 5);
            assert_eq!(offset, 2);
            assert!(full_paths);
        } else {
            panic!("Expected Graph Nodes command");
        }
    }

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
        }) = cli.command
        {
            assert_eq!(kind, Some("calls".to_string()));
            assert_eq!(from, Some("main".to_string()));
            assert_eq!(to, Some("worker".to_string()));
            assert_eq!(from_lang, Some("rust".to_string()));
            assert_eq!(to_lang, Some("python".to_string()));
            assert_eq!(file, Some("src/main.rs".to_string()));
            assert_eq!(limit, 10);
            assert_eq!(offset, 1);
            assert!(full_paths);
        } else {
            panic!("Expected Graph Edges command");
        }
    }

    // ===== Pager Tests (P2-29) =====

    #[test]
    fn test_pager_flag_default() {
        let cli = Cli::parse_from(["sqry", "query", "kind:function"]);
        assert!(!cli.pager);
        assert!(!cli.no_pager);
        assert!(cli.pager_cmd.is_none());
    }

    #[test]
    fn test_pager_flag() {
        let cli = Cli::parse_from(["sqry", "--pager", "query", "kind:function"]);
        assert!(cli.pager);
        assert!(!cli.no_pager);
    }

    #[test]
    fn test_no_pager_flag() {
        let cli = Cli::parse_from(["sqry", "--no-pager", "query", "kind:function"]);
        assert!(!cli.pager);
        assert!(cli.no_pager);
    }

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

    #[test]
    fn test_pager_and_no_pager_conflict() {
        // These flags conflict and clap should reject
        let result =
            Cli::try_parse_from(["sqry", "--pager", "--no-pager", "query", "kind:function"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_pager_flags_global() {
        // Pager flags work with any subcommand
        let cli = Cli::parse_from(["sqry", "--no-pager", "search", "test"]);
        assert!(cli.no_pager);

        let cli = Cli::parse_from(["sqry", "--pager", "index"]);
        assert!(cli.pager);
    }

    #[test]
    fn test_pager_config_json_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // JSON output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--json", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }

    #[test]
    fn test_pager_config_csv_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // CSV output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--csv", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }

    #[test]
    fn test_pager_config_tsv_bypasses_pager() {
        use crate::output::pager::PagerMode;

        // TSV output should bypass pager by default
        let cli = Cli::parse_from(["sqry", "--tsv", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Never);
    }

    #[test]
    fn test_pager_config_json_with_explicit_pager() {
        use crate::output::pager::PagerMode;

        // JSON with explicit --pager should enable pager
        let cli = Cli::parse_from(["sqry", "--json", "--pager", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Always);
    }

    #[test]
    fn test_pager_config_text_output_auto() {
        use crate::output::pager::PagerMode;

        // Text output (default) should use auto pager mode
        let cli = Cli::parse_from(["sqry", "search", "test"]);
        let config = cli.pager_config();
        assert_eq!(config.enabled, PagerMode::Auto);
    }
}
