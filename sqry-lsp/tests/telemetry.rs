use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tower::Service;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{
    HoverParams, InitializeParams, PartialResultParams, Position, TextDocumentIdentifier,
    TextDocumentPositionParams, WorkDoneProgressParams, WorkspaceSymbolParams,
};

mod common;

type TestLspFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = std::result::Result<
                    Option<tower_lsp::jsonrpc::Response>,
                    tower_lsp::ExitedError,
                >,
            > + Send,
    >,
>;
type TestLspBuffer = Buffer<Request, TestLspFuture>;

struct VecWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for VecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MakeVecWriter(Arc<Mutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeVecWriter {
    type Writer = VecWriter;

    fn make_writer(&'a self) -> Self::Writer {
        VecWriter(self.0.clone())
    }
}

async fn initialize_service(buffered: &mut TestLspBuffer) -> Result<()> {
    let initialize = Request::build("initialize")
        .params(serde_json::to_value(InitializeParams::default())?)
        .id(0i64)
        .finish();
    buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialize)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let initialized = Request::build("initialized").finish();
    buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialized)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_handler_emits_completion_span_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root)?;
    let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));
    let service = sqry_lsp::build_test_service(&session);
    let mut buffered = Buffer::new(service, 4);

    let logs = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_target(true)
        .with_writer(MakeVecWriter(logs.clone()))
        .without_time()
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    initialize_service(&mut buffered).await?;

    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: tower_lsp::lsp_types::Url::from_file_path(root.join("src/lib.rs")).unwrap(),
            },
            position: Position::new(24, 13),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let hover_request = Request::build("textDocument/hover")
        .params(serde_json::to_value(&params).unwrap())
        .id(1i64)
        .finish();

    let response = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(hover_request)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    assert!(response.is_some());

    let log_output = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(log_output.contains("handler=\"hover\""));
    assert!(log_output.contains("status=\"success\""));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_emits_warning_span_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root)?;
    let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));
    let service = sqry_lsp::build_test_service(&session);
    let mut buffered = Buffer::new(service, 4);

    let logs = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_target(true)
        .with_writer(MakeVecWriter(logs.clone()))
        .without_time()
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    initialize_service(&mut buffered).await?;

    sqry_lsp::handlers::configure_test_delay_ms(500);

    let params = WorkspaceSymbolParams {
        query: "lang:rust helper".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    let request = Request::build("workspace/symbol")
        .params(serde_json::to_value(&params).unwrap())
        .id(1i64)
        .finish();

    let request_future = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(request);

    tokio::time::sleep(Duration::from_millis(10)).await;

    let cancel = Request::build("$/cancelRequest")
        .params(serde_json::json!({ "id": 1 }))
        .finish();

    let cancel_future = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(cancel);

    let _ = tokio::join!(request_future, cancel_future);
    sqry_lsp::handlers::configure_test_delay_ms(0);

    let log_output = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(log_output.contains("handler=\"workspace_symbol\""));
    assert!(log_output.contains("event=\"cancelled\""));
    Ok(())
}
