#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Daemon adapter is unused by the sqry-mcp binary itself — it exists
// for the sqry-daemon crate to consume through the library. Task 7
// (sqry-daemon tool_dispatch) wires in the callers. `daemon_params`
// is the companion module holding the wire-schema → *Args converters
// that both the rmcp path (server.rs) and the daemon path
// (daemon_adapter::dispatch) delegate through.
#[allow(dead_code)]
mod daemon_adapter;
// Phase 8c U12 — daemon shim-mode helpers (exposed from the lib target too
// so integration tests can call resolve_daemon_socket directly).
#[allow(dead_code)]
mod daemon_params;
mod daemon_shim;
mod engine;
mod error;
mod execution;
mod feature_flags;
mod mcp_config;
// Output truncation cap module — enforces `SQRY_MCP_MAX_OUTPUT_BYTES`
// (default 50 000) at the `success_result` boundary in `server.rs`.
// Both the lib and bin targets compile this file independently; the
// lib re-exports it as `pub mod output_caps;` so integration tests can
// import directly via `sqry_mcp::output_caps`.
pub mod output_caps;
mod pagination;
mod path_resolver;
mod prompts;
mod resources;
mod response;
mod server;
mod tools;
#[allow(dead_code)]
mod tools_schema;
mod workspace_session;

use anyhow::Result;
use daemon_shim::DaemonParseResult;
use rmcp::ServiceExt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

const HELP_TEXT: &str = r"sqry-mcp - Semantic code search MCP server

USAGE:
    sqry-mcp [OPTIONS]

DEFAULT BEHAVIOR:
    With no flags, sqry-mcp probes for a running sqryd daemon and connects
    as a shim client when one is reachable. If no daemon is found (the common
    case for first-run / standalone usage), it falls back transparently to
    in-process standalone mode and serves MCP itself. The daemon path is NOT
    auto-started in default mode — pass --daemon to opt into auto-start.

OPTIONS:
    -h, --help                   Print this help message
    -V, --version                Print version information
    --list-tools                 List all available tools with their descriptions
    --daemon                     Force daemon-shim mode (auto-start if not running, no fallback)
    --no-daemon                  Force in-process standalone mode (skip the daemon probe)
    --daemon-socket <PATH>       Daemon socket path (requires --daemon)

ENVIRONMENT VARIABLES:
    SQRY_MCP_WORKSPACE_ROOT               Root directory for searches (security boundary)
    SQRY_MCP_MAX_OUTPUT_BYTES             Max output size per response (default: 50000)
    SQRY_MCP_TIMEOUT_MS                   Timeout per request in ms (default: 60000)
    SQRY_MCP_INDEX_TIMEOUT_MS             Timeout for index rebuilds in ms (default: 600000 = 10min)
    SQRY_MCP_RETRY_DELAY_MS               Retry delay for exceeded deadlines in ms (default: 500)
    SQRY_MCP_ENGINE_CACHE_CAPACITY        Max cached workspace engines (default: 5)
    SQRY_MCP_DISCOVERY_CACHE_CAPACITY     Max cached workspace paths (default: 100)
    SQRY_MCP_TRACE_CACHE_SIZE             Trace path payload cache capacity (default: 256)
    SQRY_MCP_SUBGRAPH_CACHE_SIZE          Subgraph payload cache capacity (default: 128)
    SQRY_MCP_MAX_CROSS_LANG_EDGES         Max edges for cross-language analysis (default: 50000)
    SQRY_REDACTION_PRESET                 Response redaction: none|minimal|relative|standard|strict (default: minimal)
    SQRYD_SOCKET                          Daemon socket path override for default probe and --daemon mode
    SQRY_DAEMON_NO_AUTO_START             Set to 1 to disable sqryd auto-start in --daemon mode
    SQRYD_PATH                            Explicit path to sqryd binary for --daemon auto-start

AVAILABLE TOOLS:
    Use --list-tools to view the full rmcp tool catalog

AVAILABLE PROMPTS (appear as /mcp__sqry__* in Claude Code):
    semantic_search      Search code by semantic meaning
    find_callers         Find all code that calls a function
    find_callees         Find all functions called by a function
    trace_path           Trace call path between two functions
    explain_symbol       Get detailed explanation of a symbol
    code_impact          Analyze impact of changing a symbol

HIERARCHICAL_SEARCH CONFIGURABLE LIMITS:
    max_results                 Maximum symbols to return (default: 200)
    max_files                   Maximum files per page (default: 20)
    max_containers_per_file     Maximum containers per file (default: 50)
    max_symbols_per_container   Maximum symbols per container (default: 100)
    max_total_symbols           Hard limit on total symbols (default: 2000)
    context_lines               Lines of context around symbols (default: 3)
    expand_files                File paths to expand from stubs (lazy loading)

TOKEN BUDGET PARAMETERS (advanced):
    file_target_tokens              Target tokens for file grouping (default: 2000)
    container_target_tokens         Target tokens for container grouping (default: 1500)
    symbol_target_tokens            Target tokens for symbol detail (default: 500)
    context_cluster_target_tokens   Target tokens for context clusters (default: 768)

DOCUMENTATION:
    See sqry-mcp/USER_GUIDE.md for complete documentation

PROTOCOL:
    MCP 2024-11-05 (JSON-RPC 2.0 over stdio, newline-delimited)
";

/// The parsed action resulting from manual CLI argument inspection.
///
/// sqry-mcp uses manual argument parsing (not clap) because the binary's
/// primary use-case — serving the MCP protocol over stdio — does not
/// benefit from clap's full argument-parsing machinery. Structured
/// variants are added here as new runtime modes are introduced.
///
/// # Variants
///
/// - [`CliAction::Help`] — print help text and exit.
/// - [`CliAction::Version`] — print version and exit.
/// - [`CliAction::ListTools`] — enumerate available MCP tools and exit.
/// - [`CliAction::Daemon`] — connect to a running sqryd daemon as a shim
///   client (Phase 8c U12). Owns stdio end-to-end, pumping bytes between
///   the calling process's stdin/stdout and the daemon's hosted MCP
///   server. `socket` is `Some` when `--daemon-socket <PATH>` was
///   supplied; otherwise [`daemon_shim::resolve_daemon_socket`] determines
///   the path at runtime.
/// - [`CliAction::Standalone`] — `--no-daemon` was passed: skip the
///   default-mode daemon probe and run the in-process rmcp server
///   directly.
/// - [`CliAction::Unknown`] — unrecognised flag: print an error and exit
///   with code 1.
/// - [`CliAction::None`] — no flag given: probe for a running sqryd
///   daemon and shim through it when reachable; otherwise fall back to
///   the in-process rmcp server.
#[derive(Debug)]
enum CliAction {
    Help,
    Version,
    ListTools,
    /// Connect to a sqryd daemon as an MCP shim client (Phase 8c U12).
    ///
    /// `socket` is the optional `--daemon-socket <PATH>` override. When
    /// `None`, the socket path is resolved at runtime by
    /// [`daemon_shim::resolve_daemon_socket`] (env var → platform default).
    Daemon {
        socket: Option<PathBuf>,
    },
    /// `--no-daemon` was passed: force in-process standalone mode and
    /// skip the default-mode daemon probe.
    Standalone,
    Unknown(String),
    None,
}

/// Parse CLI arguments into a [`CliAction`].
///
/// The parser is deliberately simple and sequential:
///
/// - Recognises single-flag forms `-h`/`--help`, `-V`/`--version`,
///   `--list-tools`.
/// - Delegates the `--daemon` / `--daemon-socket` subset to
///   [`daemon_shim::parse_daemon_args`] and maps `DaemonParseResult` →
///   `CliAction`.
/// - Unrecognised leading flags produce [`CliAction::Unknown`].
/// - No arguments produce [`CliAction::None`] (proceed to normal server mode).
///
/// # Constraint: `--daemon-socket` requires `--daemon`
///
/// Passing `--daemon-socket` without `--daemon` is rejected as
/// `Unknown("--daemon-socket requires --daemon")`.  This mirrors the
/// compile-time guard that clap provides via `requires = "daemon"` in
/// U11 (`sqry-lsp`).  The error message is surfaced via
/// [`handle_cli_action_sync`] which calls [`std::process::exit(1)`].
pub(crate) fn parse_cli_action(args: &[String]) -> CliAction {
    // Collect all arguments after the program name.
    let tail: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();

    // Fast-path for no arguments.
    if tail.is_empty() {
        return CliAction::None;
    }

    // Check the first argument for single-flag commands.
    match tail[0] {
        "-h" | "--help" => return CliAction::Help,
        "-V" | "--version" => return CliAction::Version,
        "--list-tools" => return CliAction::ListTools,
        _ => {}
    }

    // `--no-daemon` is mutually exclusive with `--daemon` / `--daemon-socket`.
    // Surface the conflict explicitly rather than falling through to the
    // daemon-flag parser, which would otherwise either silently honour
    // `--daemon` and ignore `--no-daemon`, or report `--no-daemon` as an
    // `UnknownInDaemonMode` token without explaining why.
    if tail.contains(&"--no-daemon") {
        if tail.contains(&"--daemon") || tail.contains(&"--daemon-socket") {
            return CliAction::Unknown(
                "--no-daemon cannot be combined with --daemon or --daemon-socket".to_string(),
            );
        }
        // Any token other than `--no-daemon` is unknown.
        if let Some(bad) = tail.iter().find(|t| **t != "--no-daemon") {
            return CliAction::Unknown((*bad).to_string());
        }
        return CliAction::Standalone;
    }

    // Delegate daemon-flag scanning to daemon_shim (single source of truth).
    match daemon_shim::parse_daemon_args(args) {
        DaemonParseResult::Daemon { socket } => return CliAction::Daemon { socket },
        DaemonParseResult::MissingDaemon => {
            return CliAction::Unknown("--daemon-socket requires --daemon".to_string());
        }
        DaemonParseResult::MissingSocketPath => {
            return CliAction::Unknown("--daemon-socket requires a PATH argument".to_string());
        }
        DaemonParseResult::UnknownInDaemonMode { token } => {
            // Codex iter-0 MINOR-1 fix: silently ignoring trailing args
            // after `--daemon` lets operator typos change behaviour
            // without warning. Surface the offending token verbatim so
            // the operator can fix it. Matches the stricter clap-based
            // parser path used by `sqry-lsp`.
            return CliAction::Unknown(format!(
                "unknown argument {token:?} after --daemon (use --help for usage)"
            ));
        }
        DaemonParseResult::NotDaemonMode => {
            // Fall through to check for unknown flags.
        }
    }

    // No recognised flag was found; surface the first unrecognised token.
    if let Some(first) = tail.first() {
        return CliAction::Unknown(first.to_string());
    }

    CliAction::None
}

fn available_tools() -> Vec<rmcp::model::Tool> {
    let flags = feature_flags::FeatureFlags::from_env();
    let server = server::SqryServer::new(flags);
    server.get_filtered_tools()
}

/// Dispatch on the parsed [`CliAction`] and return whether the action was
/// handled (i.e. the caller should NOT fall through to the normal server loop).
///
/// The `Daemon` variant is **async** and must be invoked inside the Tokio
/// runtime; the other variants are synchronous.  The caller in `main` dispatches
/// the daemon variant separately after initialising tracing, so that the
/// log subscriber is in place before `run_daemon_shim` begins connecting.
///
/// Returns `true` for all handled variants except [`CliAction::Daemon`] and
/// [`CliAction::None`].
fn handle_cli_action_sync(action: &CliAction) -> bool {
    match action {
        CliAction::Help => {
            print!("{HELP_TEXT}");
            true
        }
        CliAction::Version => {
            println!("sqry-mcp {}", env!("CARGO_PKG_VERSION"));
            true
        }
        CliAction::ListTools => {
            println!("Available MCP tools:\n");
            for tool in available_tools() {
                let name = tool.name.as_ref();
                let desc = tool.description.as_deref().unwrap_or("");
                println!("  {name}");
                println!("    {desc}\n");
            }
            true
        }
        CliAction::Unknown(arg) => {
            eprintln!("Unknown argument: {arg}");
            eprintln!("Use --help for usage information");
            std::process::exit(1);
        }
        // Daemon, Standalone, and None are NOT handled here; the caller dispatches them.
        CliAction::Daemon { .. } | CliAction::Standalone | CliAction::None => false,
    }
}

/// Run the MCP server using rmcp SDK.
async fn run_rmcp_server() -> Result<()> {
    use rmcp::transport::stdio;

    tracing::info!("sqry-mcp starting (rmcp SDK)");

    let flags = feature_flags::FeatureFlags::from_env();

    // Load MCP configuration with environment variable overrides
    let mcp_config = mcp_config::McpConfig::load_or_default()?;
    let timeout_ms = mcp_config.effective_timeout_ms()?;
    let retry_delay_ms = mcp_config.effective_retry_delay_ms()?;
    let index_timeout_ms = mcp_config.effective_index_timeout_ms()?;

    tracing::info!(
        timeout_ms = timeout_ms,
        index_timeout_ms = index_timeout_ms,
        retry_delay_ms = retry_delay_ms,
        "MCP config loaded"
    );

    // CRITICAL: Initialize all caches before handling requests
    // This must happen after config load but before server starts accepting requests
    //
    // NOTE: this binary owns its own module tree (`mod engine;` etc.), so it
    // initializes its own statics inline here. The `sqryd` daemon links the
    // lib target instead and calls `sqry_mcp::init_mcp_caches` (lib.rs) for
    // the lib's statics. Adding a new payload cache means updating BOTH this
    // block and `init_mcp_caches`, or the daemon host will panic on the new
    // cache the way daemon-hosted trace_path/subgraph once did.
    //
    // Phase 3C DB21: the payload LRU caches in `execution::graph_cache`
    // (retained per the DB17 follow-up + DB19 confirmation — they cache
    // response DTOs, not predicate results) are now sized from the
    // existing `trace_cache_size` / `subgraph_cache_size` fields. The
    // TTL is fixed at `execution::graph_cache::CACHE_TTL_SECS` (currently
    // 300 s). The retired `trace_path_cache_capacity` /
    // `subgraph_cache_capacity` / `query_cache_ttl_secs` fields were
    // duplicate knobs; see the DB21 notes in mcp_config.rs.
    let engine_capacity = mcp_config.effective_engine_cache_capacity()?;
    let discovery_capacity = mcp_config.effective_discovery_cache_capacity()?;
    let trace_path_capacity = mcp_config.effective_trace_cache_size()?;
    let subgraph_capacity = mcp_config.effective_subgraph_cache_size()?;
    let query_ttl_secs = execution::graph_cache::CACHE_TTL_SECS;

    tracing::info!(
        engine_capacity = engine_capacity,
        discovery_capacity = discovery_capacity,
        trace_path_capacity = trace_path_capacity,
        subgraph_capacity = subgraph_capacity,
        query_ttl_secs = query_ttl_secs,
        "Initializing caches"
    );

    // Initialize engine cache
    engine::init_engine_cache(
        NonZeroUsize::new(engine_capacity)
            .ok_or_else(|| anyhow::anyhow!("BUG: engine_capacity validated but still zero"))?,
    );

    // Initialize discovery cache
    path_resolver::init_discovery_cache(
        NonZeroUsize::new(discovery_capacity)
            .ok_or_else(|| anyhow::anyhow!("BUG: discovery_capacity validated but still zero"))?,
    );

    // Initialize query caches (trace_path and subgraph)
    execution::init_trace_path_cache(
        NonZeroUsize::new(trace_path_capacity)
            .ok_or_else(|| anyhow::anyhow!("BUG: trace_path_capacity validated but still zero"))?,
        Duration::from_secs(query_ttl_secs),
    );

    execution::init_subgraph_cache(
        NonZeroUsize::new(subgraph_capacity)
            .ok_or_else(|| anyhow::anyhow!("BUG: subgraph_capacity validated but still zero"))?,
        Duration::from_secs(query_ttl_secs),
    );

    tracing::info!("All caches initialized successfully");

    // Initialize response redactor from environment config.
    //
    // STEP_7 codex iter4: `preset=none` now constructs a passthrough
    // redactor (rather than `None`) so the walker's
    // exclusions-override-passthrough branch
    // (`redact_excluded_in_passthrough`) can fire end-to-end when a
    // `LogicalWorkspaceView` is bound at request time. The walker
    // remains a no-op for non-excluded fields under passthrough mode,
    // so criterion 3 (`preset=none + non-excluded path → absolute
    // emitted`) is preserved. `None` only arises here for an unknown /
    // misconfigured preset.
    let redactor = server::SqryServer::create_redactor(&mcp_config.redaction_preset);
    match (&redactor, mcp_config.redaction_preset.as_str()) {
        (Some(_), "none") => tracing::info!(
            "Response redaction in passthrough mode (preset=none): excluded paths still rewritten when LogicalWorkspaceView is bound"
        ),
        (Some(_), preset) => tracing::info!(preset, "Response redaction enabled"),
        (None, preset) => {
            tracing::info!(preset, "Response redaction disabled (unknown preset)");
        }
    }

    let server = server::SqryServer::with_config(
        flags,
        timeout_ms,
        index_timeout_ms,
        retry_delay_ms,
        redactor,
    );

    let service = server
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start rmcp server: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {e}"))?;

    Ok(())
}

/// # Cancellation Safety
///
/// This is the main MCP server event loop. It is cancellation-safe because
/// dropping the future will stop accepting new JSON-RPC messages and cleanly
/// close stdin/stdout streams. No state corruption occurs as the loop maintains
/// no persistent state between messages - each request is handled independently.
/// `A_cancellation.md` §5 + GT-6: cap the blocking thread pool at 64
/// so a storm of timed-out tool calls (which leave their
/// `spawn_blocking` body running cooperatively until the
/// `CancellationToken` signal is observed) cannot exhaust the default
/// 512-thread cap and queue subsequent calls indefinitely. Mirrors
/// the sqry-daemon binary's runtime cap. See
/// `sqry-mcp/tests/semantic_search_timeout_recovery.rs` for the
/// regression test that pins this contract.
///
/// `#[tokio::main]` does not surface `max_blocking_threads` as a
/// macro attribute, so the runtime is built manually and the
/// `async fn main` body is dispatched via `block_on`.
fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Handle synchronous CLI arguments (--help, --version, --list-tools,
    // unknown flags) before initialising the tracing subscriber.
    let args: Vec<String> = std::env::args().collect();
    let action = parse_cli_action(&args);

    if handle_cli_action_sync(&action) {
        return Ok(());
    }

    // Log to stderr only; never stdout
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .init();

    match action {
        // Explicit `--daemon`: force daemon-shim mode with auto-start; do
        // not fall back. Owns stdio end-to-end.
        CliAction::Daemon { socket } => daemon_shim::run_daemon_shim(socket).await,
        // Explicit `--no-daemon`: skip the probe and run the in-process
        // rmcp server directly.
        CliAction::Standalone => run_rmcp_server().await,
        // Default: probe for a running daemon, shim through it on
        // success, fall back to in-process rmcp server otherwise.
        CliAction::None => match daemon_shim::probe_and_run_daemon_shim(None).await {
            daemon_shim::ProbeOutcome::Completed => Ok(()),
            daemon_shim::ProbeOutcome::Unavailable => run_rmcp_server().await,
            daemon_shim::ProbeOutcome::Failed(e) => Err(e),
        },
        // Other variants are dispatched in `handle_cli_action_sync` above.
        _ => unreachable!("Help/Version/ListTools/Unknown are handled by handle_cli_action_sync"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `parse_cli_action` with no extra arguments produces `CliAction::None`
    /// (proceed to normal server mode — the hot-path for AI tooling).
    #[test]
    fn parse_no_args_returns_none() {
        let a = parse_cli_action(&args(&["sqry-mcp"]));
        assert!(matches!(a, CliAction::None));
    }

    /// `--help` / `-h` produce `CliAction::Help`.
    #[test]
    fn parse_help_flags() {
        assert!(matches!(
            parse_cli_action(&args(&["sqry-mcp", "--help"])),
            CliAction::Help
        ));
        assert!(matches!(
            parse_cli_action(&args(&["sqry-mcp", "-h"])),
            CliAction::Help
        ));
    }

    /// `--version` / `-V` produce `CliAction::Version`.
    #[test]
    fn parse_version_flags() {
        assert!(matches!(
            parse_cli_action(&args(&["sqry-mcp", "--version"])),
            CliAction::Version
        ));
        assert!(matches!(
            parse_cli_action(&args(&["sqry-mcp", "-V"])),
            CliAction::Version
        ));
    }

    /// `--list-tools` produces `CliAction::ListTools`.
    #[test]
    fn parse_list_tools() {
        assert!(matches!(
            parse_cli_action(&args(&["sqry-mcp", "--list-tools"])),
            CliAction::ListTools
        ));
    }

    /// `--daemon` alone produces `CliAction::Daemon { socket: None }`.
    #[test]
    fn parse_daemon_without_socket() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--daemon"]));
        assert!(matches!(a, CliAction::Daemon { socket: None }));
    }

    /// `--daemon --daemon-socket <PATH>` produces `CliAction::Daemon { socket: Some(..) }`.
    #[test]
    fn parse_daemon_with_socket() {
        let a = parse_cli_action(&args(&[
            "sqry-mcp",
            "--daemon",
            "--daemon-socket",
            "/custom/sqryd.sock",
        ]));
        match a {
            CliAction::Daemon { socket: Some(p) } => {
                assert_eq!(p, std::path::PathBuf::from("/custom/sqryd.sock"));
            }
            other => panic!("expected Daemon with socket, got: {other:?}"),
        }
    }

    /// `--daemon-socket <PATH>` without `--daemon` produces `CliAction::Unknown`
    /// with a message that mentions the dependency.
    #[test]
    fn parse_daemon_socket_without_daemon_is_unknown() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--daemon-socket", "/some/path"]));
        match a {
            CliAction::Unknown(msg) => {
                assert!(
                    msg.contains("--daemon-socket requires --daemon"),
                    "error message should explain the requirement; got: {msg}"
                );
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }

    /// `--daemon-socket` with no following path argument produces `CliAction::Unknown`.
    #[test]
    fn parse_daemon_socket_missing_path_arg() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--daemon-socket"]));
        assert!(matches!(a, CliAction::Unknown(_)));
    }

    /// `--daemon --daemon-socket <PATH>` and `--daemon-socket <PATH> --daemon`
    /// are both valid (order-independent).
    #[test]
    fn parse_daemon_flags_are_order_independent() {
        // socket before daemon
        let a = parse_cli_action(&args(&[
            "sqry-mcp",
            "--daemon-socket",
            "/reorder.sock",
            "--daemon",
        ]));
        match a {
            CliAction::Daemon { socket: Some(p) } => {
                assert_eq!(p, std::path::PathBuf::from("/reorder.sock"));
            }
            other => panic!("expected Daemon with socket, got: {other:?}"),
        }
    }

    /// An unrecognised flag produces `CliAction::Unknown` with that flag as
    /// the message.
    #[test]
    fn parse_unknown_flag() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--unknown-flag"]));
        match a {
            CliAction::Unknown(msg) => {
                assert_eq!(msg, "--unknown-flag");
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }

    /// `--no-daemon` alone produces `CliAction::Standalone`.
    #[test]
    fn parse_no_daemon_alone() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--no-daemon"]));
        assert!(
            matches!(a, CliAction::Standalone),
            "expected Standalone, got: {a:?}"
        );
    }

    /// `--no-daemon --daemon` is a conflict and must be rejected.
    #[test]
    fn parse_no_daemon_with_daemon_is_unknown() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--no-daemon", "--daemon"]));
        match a {
            CliAction::Unknown(msg) => {
                assert!(
                    msg.contains("--no-daemon")
                        && msg.contains("--daemon")
                        && msg.contains("cannot be combined"),
                    "error must explain the conflict; got: {msg}"
                );
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }

    /// `--no-daemon --daemon-socket <PATH>` is a conflict — `--daemon-socket`
    /// already requires `--daemon`, and `--no-daemon` explicitly disables it.
    #[test]
    fn parse_no_daemon_with_daemon_socket_is_unknown() {
        let a = parse_cli_action(&args(&[
            "sqry-mcp",
            "--no-daemon",
            "--daemon-socket",
            "/x.sock",
        ]));
        match a {
            CliAction::Unknown(msg) => {
                assert!(
                    msg.contains("--no-daemon") && msg.contains("cannot be combined"),
                    "error must explain the conflict; got: {msg}"
                );
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }

    /// `--no-daemon` order-independent with conflicting flags.
    #[test]
    fn parse_no_daemon_after_daemon_is_unknown() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--daemon", "--no-daemon"]));
        match a {
            CliAction::Unknown(msg) => {
                assert!(
                    msg.contains("cannot be combined"),
                    "error must explain the conflict; got: {msg}"
                );
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }

    /// `--no-daemon --bogus` surfaces the unknown trailing token rather than
    /// silently entering standalone mode.
    #[test]
    fn parse_no_daemon_with_unknown_trailing_is_unknown() {
        let a = parse_cli_action(&args(&["sqry-mcp", "--no-daemon", "--bogus"]));
        match a {
            CliAction::Unknown(msg) => {
                assert_eq!(msg, "--bogus");
            }
            other => panic!("expected Unknown, got: {other:?}"),
        }
    }
}
