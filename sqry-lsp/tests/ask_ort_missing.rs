//! NL08 — ONNX Runtime missing surface (LSP).
//!
//! Drives the `SQRY_NL_FORCE_ORT_MISSING` deterministic test seam in
//! `sqry-nl/src/classifier/model.rs` and asserts the LSP error
//! response (produced by the server's private `map_error` helper,
//! re-exposed for tests via `map_error_public_for_tests`) carries:
//!
//! * `code = ErrorCode::InternalError` (`-32603`)
//! * `message` containing the platform-specific install hint
//! * `data` carrying the canonical NL08 envelope:
//!   `{ code: "ONNX_RUNTIME_MISSING", message: <hint>, retriable: false }`
//!
//! Mirrors the standalone-MCP envelope so an LSP client and a daemon
//! MCP client see the same wire payload for the same condition.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::ask::execute;
use sqry_lsp::map_error_public_for_tests;
use sqry_lsp::protocol::SqryAskParams;
use sqry_lsp::session::SessionManager;

fn in_tree_model_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-lsp manifest dir must have a parent (workspace root)")
        .join("sqry-nl/models")
}

fn test_lsp_options(index_root: PathBuf) -> LspOptions {
    LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(index_root),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    }
}

fn expected_hint_substring() -> &'static str {
    if cfg!(target_os = "linux") {
        "apt-get install libonnxruntime-dev"
    } else if cfg!(target_os = "macos") {
        "brew install onnxruntime"
    } else if cfg!(target_os = "windows") {
        "libonnxruntime.dll"
    } else {
        "libonnxruntime"
    }
}

#[test]
fn lsp_emits_error_response_with_hint() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let opts = test_lsp_options(workspace_root.clone());
    let session = Arc::new(SessionManager::new(opts));
    let model_dir = in_tree_model_dir();

    let params = SqryAskParams {
        query: "find functions".to_string(),
        path: None,
        model_dir: Some(model_dir.to_string_lossy().into_owned()),
        allow_unverified_model: false,
        allow_model_download: false,
    };

    // SAFETY: env-var mutation. `cargo test` defaults parallelise
    // tests at the binary level; this test lives in its own binary
    // (`tests/ask_ort_missing.rs`) so no other test in the same
    // process touches `SQRY_NL_FORCE_ORT_MISSING`.
    unsafe {
        std::env::set_var("SQRY_NL_FORCE_ORT_MISSING", "1");
    }

    let result = execute(session.as_ref(), &params);

    unsafe {
        std::env::remove_var("SQRY_NL_FORCE_ORT_MISSING");
    }

    let err = result.expect_err(
        "execute must fail when SQRY_NL_FORCE_ORT_MISSING=1 is set; \
         the env-var seam fires at first translator init",
    );

    // Drive the production wire-mapping path.
    let rpc_err = map_error_public_for_tests(err);

    // ErrorCode::InternalError is `-32603`. Compare via the JSON
    // serialisation of the underlying enum to keep the assertion
    // stable across `tower_lsp` minor versions.
    let code_json = serde_json::to_value(rpc_err.code).expect("serialize ErrorCode");
    assert_eq!(
        code_json,
        Value::from(-32603i64),
        "rpc_err.code must be ErrorCode::InternalError (-32603), got: {code_json:?}"
    );

    let hint_substr = expected_hint_substring();
    assert!(
        rpc_err.message.contains(hint_substr),
        "rpc_err.message must contain platform install hint substring \
         '{hint_substr}'. Got: {msg:?}",
        msg = rpc_err.message,
    );

    let data = rpc_err
        .data
        .as_ref()
        .expect("rpc_err.data must be populated for ONNX_RUNTIME_MISSING");

    let code = data.get("code").and_then(Value::as_str).unwrap_or("");
    let message = data.get("message").and_then(Value::as_str).unwrap_or("");
    let retriable = data.get("retriable").and_then(Value::as_bool);

    assert_eq!(
        code, "ONNX_RUNTIME_MISSING",
        "data.code must be 'ONNX_RUNTIME_MISSING'. data: {data:?}"
    );
    assert!(
        message.contains(hint_substr),
        "data.message must contain platform hint substring '{hint_substr}'. data: {data:?}"
    );
    assert_eq!(
        retriable,
        Some(false),
        "data.retriable must be exactly `false`. data: {data:?}"
    );
}
