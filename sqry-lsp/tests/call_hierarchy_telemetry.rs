use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::stream::StreamExt;
use serde_json::json;
use tokio::time::timeout;
use tower::buffer::Buffer;
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::{ClientSocket, LspService};

mod common;

#[tokio::test(flavor = "current_thread")]
async fn call_hierarchy_timeout_emits_telemetry_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root)?;

    let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));

    let (service, client_socket) =
        LspService::build(|client| sqry_lsp::SqryLanguageServer::new(client, session.clone()))
            .custom_method(
                "sqry/search",
                sqry_lsp::SqryLanguageServer::handle_sqry_search,
            )
            .custom_method(
                "sqry/references",
                sqry_lsp::SqryLanguageServer::handle_sqry_relation,
            )
            .custom_method(
                "sqry/indexStatus",
                sqry_lsp::SqryLanguageServer::handle_index_status,
            )
            .finish();

    let telemetry_events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let telemetry_capture = telemetry_events.clone();
    let messages: ClientSocket = client_socket;
    let mut messages = messages.fuse();
    let telemetry_task = tokio::spawn(async move {
        while let Some(request) = messages.next().await {
            if request.method() == "telemetry/event" {
                let (_, _, params) = request.into_parts();
                if let Some(value) = params {
                    telemetry_capture.lock().unwrap().push(value);
                }
            }
        }
    });

    let mut buffered = Buffer::new(service, 8);

    let initialize = Request::build("initialize")
        .params(serde_json::to_value(
            tower_lsp::lsp_types::InitializeParams::default(),
        )?)
        .id(0_i64)
        .finish();
    let _ = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialize)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let initialized = Request::build("initialized").finish();
    let _ = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialized)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let config = Request::build("workspace/didChangeConfiguration")
        .params(json!({
            "settings": {
                "sqry": {
                    "callHierarchy": {
                        "timeoutMs": 10
                    }
                }
            }
        }))
        .finish();
    let _ = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(config)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let lib_path = root.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib_path)?;
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": tower_lsp::lsp_types::TextDocumentItem {
                uri: tower_lsp::lsp_types::Url::from_file_path(&lib_path).unwrap(),
                language_id: "rust".into(),
                version: 1,
                text,
            }
        }))
        .finish();
    let _ = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(did_open)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    struct DelayGuard;
    impl Drop for DelayGuard {
        fn drop(&mut self) {
            sqry_lsp::handlers::configure_test_delay_ms(0);
        }
    }
    let _delay_guard = DelayGuard;
    sqry_lsp::handlers::configure_test_delay_ms(200);

    let prepare = Request::build("textDocument/prepareCallHierarchy")
        .params(json!({
            "textDocument": {
                "uri": tower_lsp::lsp_types::Url::from_file_path(&lib_path).unwrap(),
            },
            "position": { "line": 40, "character": 5 }
        }))
        .id(99_i64)
        .finish();

    let response = buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(prepare)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let error = response
        .expect("call hierarchy response")
        .error()
        .cloned()
        .expect("expected RPC error");
    assert_eq!(error.code, tower_lsp::jsonrpc::ErrorCode::RequestCancelled);

    timeout(Duration::from_secs(1), async {
        loop {
            {
                let guard = telemetry_events.lock().unwrap();
                if guard.iter().any(|value| {
                    value["event"] == "sqry/callHierarchy" && value["outcome"] == "timeout"
                }) {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timeout telemetry event not observed");

    telemetry_task.abort();

    let events = telemetry_events.lock().unwrap();
    let timeout_event = events
        .iter()
        .find(|value| value["event"] == "sqry/callHierarchy" && value["outcome"] == "timeout")
        .expect("call hierarchy timeout telemetry event");

    assert_eq!(timeout_event["handler"], "prepare");
    assert!(
        timeout_event["durationMs"].is_number(),
        "durationMs should be numeric"
    );

    Ok(())
}
