#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod engine;
mod error;
mod execution;
mod feature_flags;
mod mcp_config;
mod pagination;
mod path_resolver;
mod prompts;
mod resources;
mod server;
mod tools;

use anyhow::Result;
use rmcp::ServiceExt;
use std::num::NonZeroUsize;
use std::time::Duration;

const HELP_TEXT: &str = r"sqry-mcp - Semantic code search MCP server

USAGE:
    sqry-mcp [OPTIONS]

OPTIONS:
    -h, --help       Print this help message
    -V, --version    Print version information
    --list-tools     List all available tools with their descriptions

ENVIRONMENT VARIABLES:
    SQRY_MCP_WORKSPACE_ROOT               Root directory for searches (security boundary)
    SQRY_MCP_MAX_OUTPUT_BYTES             Max output size per response (default: 50000)
    SQRY_MCP_TIMEOUT_MS                   Timeout per request in ms (default: 30000)
    SQRY_MCP_RETRY_DELAY_MS               Retry delay for exceeded deadlines in ms (default: 500)
    SQRY_MCP_ENGINE_CACHE_CAPACITY        Max cached workspace engines (default: 5)
    SQRY_MCP_DISCOVERY_CACHE_CAPACITY     Max cached workspace paths (default: 100)
    SQRY_MCP_TRACE_PATH_CACHE_CAPACITY    Trace path cache capacity (default: 256)
    SQRY_MCP_SUBGRAPH_CACHE_CAPACITY      Subgraph cache capacity (default: 128)
    SQRY_MCP_QUERY_CACHE_TTL_SECS         Query cache TTL in seconds (default: 300)
    SQRY_MCP_MAX_CROSS_LANG_EDGES         Max edges for cross-language analysis (default: 50000)
    SQRY_REDACTION_PRESET                 Response redaction: none|minimal|standard|strict (default: none)

AVAILABLE TOOLS:
    Use --list-tools to view the full rmcp tool catalog

AVAILABLE PROMPTS (appear as /mcp__sqry__* in Claude Code):
    semantic_search      Search code by semantic meaning
    find_callers         Find all code that calls a function
    find_callees         Find all functions called by a function
    trace_path           Trace call path between two functions
    explain_symbol       Get detailed explanation of a symbol
    code_impact          Analyze impact of changing a symbol
    ask                  Natural language query interface

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

#[derive(Debug)]
enum CliAction {
    Help,
    Version,
    ListTools,
    Unknown(String),
    None,
}

fn parse_cli_action(args: &[String]) -> CliAction {
    match args.get(1).map(String::as_str) {
        Some("-h" | "--help") => CliAction::Help,
        Some("-V" | "--version") => CliAction::Version,
        Some("--list-tools") => CliAction::ListTools,
        Some(arg) => CliAction::Unknown(arg.to_string()),
        None => CliAction::None,
    }
}

fn available_tools() -> Vec<rmcp::model::Tool> {
    let flags = feature_flags::FeatureFlags::from_env();
    let server = server::SqryServer::new(flags);
    server.get_filtered_tools()
}

fn handle_cli_action(action: CliAction) -> bool {
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
        CliAction::None => false,
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

    tracing::info!(
        timeout_ms = timeout_ms,
        retry_delay_ms = retry_delay_ms,
        "MCP config loaded"
    );

    // CRITICAL: Initialize all caches before handling requests
    // This must happen after config load but before server starts accepting requests
    let engine_capacity = mcp_config.effective_engine_cache_capacity()?;
    let discovery_capacity = mcp_config.effective_discovery_cache_capacity()?;
    let trace_path_capacity = mcp_config.effective_trace_path_cache_capacity()?;
    let subgraph_capacity = mcp_config.effective_subgraph_cache_capacity()?;
    let query_ttl_secs = mcp_config.effective_query_cache_ttl_secs()?;

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

    // Initialize response redactor from environment config
    let redactor = server::SqryServer::create_redactor(&mcp_config.redaction_preset);
    if redactor.is_some() {
        tracing::info!(
            preset = mcp_config.redaction_preset.as_str(),
            "Response redaction enabled"
        );
    } else {
        tracing::info!("Response redaction disabled (passthrough mode)");
    }

    let server = server::SqryServer::with_config(flags, timeout_ms, retry_delay_ms, redactor);

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
#[tokio::main]
async fn main() -> Result<()> {
    // Handle CLI arguments
    let args: Vec<String> = std::env::args().collect();
    if handle_cli_action(parse_cli_action(&args)) {
        return Ok(());
    }

    // Log to stderr only; never stdout
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .init();

    run_rmcp_server().await
}
