mod cancel;
mod cli;
pub mod config;
mod conversion;
pub mod documents;
pub mod file_types;
pub mod handlers;
pub mod protocol;
mod security;
mod server;
pub mod session;
pub mod utils;

pub use cancel::spawn_blocking;
pub use cli::{LspCli, LspOptions};
pub use server::SqryLanguageServer;

use anyhow::{Context, Result};
use log::{error, info};
use session::SessionManager;
use std::net::ToSocketAddrs;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tower_lsp::{ClientSocket, LspService};

/// Run the sqry LSP server with the provided options.
///
/// # Errors
///
/// Returns an error when the Tokio runtime fails to start or when either the
/// stdio or socket transports encounter unrecoverable IO failures.
pub fn run(options: LspOptions) -> Result<()> {
    init_logger(&options);

    let rt = Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    rt.block_on(async move {
        let session = SessionManager::new(options.clone());

        if let Some(addr) = &options.socket {
            serve_socket(addr, options.allow_public_bind, session.clone()).await?;
        }

        if options.use_stdio() {
            serve_stdio(session).await?;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

fn init_logger(options: &LspOptions) {
    let env = env_logger::Env::default().default_filter_or(&options.log_level);
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp(None);
    if builder.try_init().is_err() {
        // Logger already initialised elsewhere (e.g., via sqry CLI); ignore.
    }
}

/// Build an LSP service with all sqry custom methods registered.
///
/// This is the single source of truth for custom method registration,
/// shared by stdio, socket, and test transports.
fn build_sqry_service(session: SessionManager) -> (LspService<SqryLanguageServer>, ClientSocket) {
    LspService::build(|client| server::SqryLanguageServer::new(client, session))
        .custom_method(
            "sqry/search",
            server::SqryLanguageServer::handle_sqry_search,
        )
        .custom_method(
            "sqry/references",
            server::SqryLanguageServer::handle_sqry_relation,
        )
        .custom_method(
            "sqry/indexStatus",
            server::SqryLanguageServer::handle_index_status,
        )
        .custom_method(
            "sqry/listFiles",
            server::SqryLanguageServer::handle_list_files,
        )
        .custom_method(
            "sqry/listSymbols",
            server::SqryLanguageServer::handle_list_symbols,
        )
        .custom_method(
            "sqry/listFilesByLanguage",
            server::SqryLanguageServer::handle_list_files_by_language,
        )
        .custom_method(
            "sqry/listCrossLanguageRelations",
            server::SqryLanguageServer::handle_list_cross_language_relations,
        )
        .custom_method(
            "sqry/listDuplicateGroups",
            server::SqryLanguageServer::handle_list_duplicate_groups,
        )
        .custom_method(
            "sqry/listCircularDependencies",
            server::SqryLanguageServer::handle_list_circular_dependencies,
        )
        .custom_method(
            "sqry/listUnusedSymbols",
            server::SqryLanguageServer::handle_list_unused_symbols,
        )
        .custom_method(
            "sqry/hierarchicalSearch",
            server::SqryLanguageServer::handle_hierarchical_search,
        )
        .custom_method("sqry/ask", server::SqryLanguageServer::handle_ask)
        .custom_method(
            "sqry/directCallers",
            server::SqryLanguageServer::handle_direct_callers,
        )
        .custom_method(
            "sqry/directCallees",
            server::SqryLanguageServer::handle_direct_callees,
        )
        .custom_method(
            "sqry/graphStats",
            server::SqryLanguageServer::handle_graph_stats,
        )
        .custom_method(
            "sqry/patternSearch",
            server::SqryLanguageServer::handle_pattern_search,
        )
        .custom_method(
            "sqry/dependencyImpact",
            server::SqryLanguageServer::handle_dependency_impact,
        )
        .custom_method(
            "sqry/explainSymbol",
            server::SqryLanguageServer::handle_explain_symbol,
        )
        .custom_method(
            "sqry/tracePath",
            server::SqryLanguageServer::handle_trace_path,
        )
        .custom_method(
            "sqry/graphExport",
            server::SqryLanguageServer::handle_graph_export,
        )
        .custom_method("sqry/subgraph", server::SqryLanguageServer::handle_subgraph)
        .custom_method(
            "sqry/isNodeInCycle",
            server::SqryLanguageServer::handle_is_node_in_cycle,
        )
        .custom_method(
            "sqry/similarSymbols",
            server::SqryLanguageServer::handle_similar_symbols,
        )
        .custom_method(
            "sqry/showDependencies",
            server::SqryLanguageServer::handle_show_dependencies,
        )
        .custom_method(
            "sqry/complexityMetrics",
            server::SqryLanguageServer::handle_complexity_metrics,
        )
        .custom_method(
            "sqry/getInsights",
            server::SqryLanguageServer::handle_get_insights,
        )
        .custom_method(
            "sqry/semanticDiff",
            server::SqryLanguageServer::handle_semantic_diff,
        )
        .finish()
}

/// Serve the LSP protocol over stdio.
///
/// # Errors
///
/// Returns an error when the transport layer fails to initialise or run.
///
/// # Cancellation Safety
///
/// This function is the main LSP server loop. It is cancellation-safe because
/// dropping the future will gracefully shutdown the `tower_lsp::Server`, which
/// closes stdio connections cleanly. No state corruption occurs as the server
/// manages all connection state internally.
async fn serve_stdio(session: SessionManager) -> Result<()> {
    info!("sqry-lsp using stdio transport");

    let (service, messages) = build_sqry_service(session);
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    tower_lsp::Server::new(stdin, stdout, messages)
        .serve(service)
        .await;
    Ok(())
}

/// Build an in-process LSP service for testing purposes.
pub fn build_test_service(session: &SessionManager) -> LspService<SqryLanguageServer> {
    let (service, _messages) = build_sqry_service(session.clone());
    service
}

/// Serve the LSP protocol over TCP.
///
/// # Errors
///
/// Returns an error when binding the socket or serving the connection fails.
///
/// # Cancellation Safety
///
/// This function is the main TCP socket server loop. It is cancellation-safe
/// because dropping the future will stop accepting new connections and drop the
/// `TcpListener`, which releases the bound socket. In-flight connections spawned
/// via `tokio::spawn` continue running independently and are not affected.
async fn serve_socket(addr: &str, allow_public_bind: bool, session: SessionManager) -> Result<()> {
    let resolved_addr = addr
        .to_socket_addrs()
        .context("invalid socket address")?
        .next()
        .ok_or_else(|| anyhow::anyhow!("unable to resolve socket address"))?;

    let listener = TcpListener::bind(resolved_addr)
        .await
        .context("failed to bind LSP socket")?;

    // Validate bind address for security concerns
    security::validate_bind_address(resolved_addr, allow_public_bind);

    info!("sqry-lsp listening on socket {resolved_addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("accepted LSP client from {peer}");
                let session_clone = session.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_socket_stream(stream, session_clone).await {
                        error!("socket session ended with error: {err:?}");
                    }
                });
            }
            Err(err) => {
                return Err(err).context("failed to accept LSP socket connection");
            }
        }
    }
}

/// Handle an individual TCP stream.
///
/// # Errors
///
/// Returns an error when the LSP server fails to process the stream.
///
/// # Cancellation Safety
///
/// This function handles a single LSP client connection. It is cancellation-safe
/// because dropping the future will gracefully shutdown the `tower_lsp::Server`
/// for this connection, closing the TCP stream cleanly. The session state (shared
/// via `SessionManager`) remains consistent as all mutations are atomic.
async fn handle_socket_stream(stream: TcpStream, session: SessionManager) -> Result<()> {
    let (reader, writer) = tokio::io::split(stream);
    let (service, messages) = build_sqry_service(session);
    tower_lsp::Server::new(reader, writer, messages)
        .serve(service)
        .await;
    Ok(())
}
