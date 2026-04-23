//! Daemon-hosted LSP entrypoint. sqry-daemon's router (Phase 8c U10)
//! invokes [`host_on_streams`] for each `ShimProtocol::Lsp` shim
//! connection after the `ShimRegisterAck { accepted: true }` has
//! been written. The raw byte-pump stream is handed to tower_lsp's
//! server for LSP protocol handling.
//!
//! Per Codex iter-1 §E + iter-2 §E: each daemon-hosted LSP shim
//! gets a fresh [`SessionManager`] per connection. Shared session
//! state is a deferred performance optimisation, not a correctness
//! requirement.

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::session::SessionManager;

/// Host a tower_lsp LSP server on an arbitrary full-duplex stream
/// pair. Used by sqry-daemon's shim byte-pump to serve
/// `ShimProtocol::Lsp` connections.
///
/// The stream pair `(reader, writer)` carries LSP's native framing
/// (Content-Length headers + JSON body) — NOT sqryd's 4-byte LE
/// length-prefix frames. The daemon router's shim dispatch is
/// responsible for consuming its own `ShimRegister`/`ShimRegisterAck`
/// frames BEFORE handing the raw halves to this function.
///
/// # Cancellation
///
/// Biased `tokio::select!` on `shutdown.cancelled()` — returns early
/// on daemon shutdown. Dropping the future also cleanly shuts down
/// the `tower_lsp::Server`, closing the stream pair.
///
/// # Errors
///
/// Returns an [`anyhow::Error`] if tower_lsp's service construction
/// fails. Normal disconnects (including cancellation) return
/// `Ok(())`.
pub async fn host_on_streams<R, W>(
    reader: R,
    writer: W,
    session: SessionManager,
    shutdown: CancellationToken,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (service, messages) = crate::build_sqry_service(session);
    let serve_fut = async move {
        tower_lsp::Server::new(reader, writer, messages)
            .serve(service)
            .await;
    };
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Ok(()),
        () = serve_fut => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::LspOptions;
    use crate::session::SessionManager;
    use tokio::io::duplex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_on_streams_serves_initialize_via_duplex() {
        // Client end writes LSP initialize; server end reads.
        let (client_side, server_side) = duplex(65536);
        let (client_reader, client_writer) = tokio::io::split(client_side);
        let (server_reader, server_writer) = tokio::io::split(server_side);

        let session = SessionManager::new(LspOptions::default_daemon());
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        let server_task = tokio::spawn(async move {
            host_on_streams(server_reader, server_writer, session, shutdown_clone).await
        });

        // Write LSP initialize request.
        let request_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                "capabilities": {}
            }
        })
        .to_string();

        let mut client_writer = client_writer;
        let frame = format!(
            "Content-Length: {}\r\n\r\n{}",
            request_body.len(),
            request_body
        );
        client_writer.write_all(frame.as_bytes()).await.unwrap();

        // Read the initialize response — just check we got SOMETHING back.
        let mut client_reader = client_reader;
        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client_reader.read(&mut buf),
        )
        .await
        .expect("read timeout")
        .expect("read error");

        assert!(n > 0, "server should respond to initialize");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(
            response.contains("Content-Length:"),
            "response should be LSP-framed"
        );
        assert!(
            response.contains("\"jsonrpc\":\"2.0\""),
            "response should be JSON-RPC 2.0"
        );
        assert!(
            response.contains("\"id\":1"),
            "response should match request id"
        );

        // Shut down cleanly.
        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_task)
            .await
            .expect("server shutdown timeout");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_on_streams_observes_shutdown_cancellation() {
        let (_client_side, server_side) = duplex(4096);
        let (server_reader, server_writer) = tokio::io::split(server_side);
        let session = SessionManager::new(LspOptions::default_daemon());
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        let server_task = tokio::spawn(async move {
            host_on_streams(server_reader, server_writer, session, shutdown_clone).await
        });

        // Give the server a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Fire shutdown; server should return Ok quickly.
        shutdown.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), server_task)
            .await
            .expect("server shutdown timeout")
            .expect("join error");

        assert!(result.is_ok());
    }
}
