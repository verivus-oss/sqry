use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::future::join;
use tokio::sync::Mutex;
use tower::Service;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_lsp::jsonrpc::{ErrorCode, Request};
use tower_lsp::lsp_types::{
    InitializeParams, PartialResultParams, WorkDoneProgressParams, WorkspaceSymbolParams,
};

mod common;

fn new_session(root: &std::path::Path) -> sqry_lsp::session::SessionManager {
    let options = sqry_lsp::LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    };
    sqry_lsp::session::SessionManager::new(options)
}

#[tokio::test(flavor = "current_thread")]
async fn server_handles_cancellation_fast_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root)?;
    let session = new_session(&root);
    let params = WorkspaceSymbolParams {
        query: "lang:rust page_size:1 helper".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
    static DELAY_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    let _lock = DELAY_GUARD.get_or_init(|| Mutex::new(())).lock().await;
    sqry_lsp::handlers::configure_test_delay_ms(200);

    let service = sqry_lsp::build_test_service(&session);
    let mut buffered = Buffer::new(service, 8);

    let initialize = Request::build("initialize")
        .params(serde_json::to_value(InitializeParams::default())?)
        .id(0i64)
        .finish();
    let init_future = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialize);
    let init_response = init_future
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    assert!(init_response.is_some(), "initialize response expected");

    let initialized = Request::build("initialized").finish();
    buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialized)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let request = Request::build("workspace/symbol")
        .params(serde_json::to_value(&params)?)
        .id(1i64)
        .finish();

    let cancel = Request::build("$/cancelRequest")
        .params(serde_json::json!({ "id": 1 }))
        .finish();

    let ready = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    let workspace_future = ready.call(request);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let start = Instant::now();
    let ready = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    let cancel_future = ready.call(cancel);

    let (workspace_result, cancel_result) = join(workspace_future, cancel_future).await;
    let elapsed = start.elapsed();
    sqry_lsp::handlers::configure_test_delay_ms(0);

    let cancel_result = cancel_result.map_err(|err| anyhow::anyhow!(err.to_string()))?;
    assert!(
        cancel_result.is_none(),
        "cancel notification should not return data: {cancel_result:?}"
    );

    let response = workspace_result
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .expect("workspace symbol response");
    let (_, body) = response.into_parts();
    let error = body.expect_err("expected cancellation error");
    assert_eq!(error.code, ErrorCode::RequestCancelled);
    assert!(
        elapsed < Duration::from_millis(50),
        "cancellation acknowledgement took {elapsed:?}"
    );

    Ok(())
}
