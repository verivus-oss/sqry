//! NL08 — ONNX Runtime missing surface (MCP standalone).
//!
//! Drives the `SQRY_NL_FORCE_ORT_MISSING` deterministic test seam in
//! `sqry-nl/src/classifier/model.rs` and asserts the structured MCP
//! envelope emitted by `execute_sqry_ask` carries the canonical
//! `{ code: "ONNX_RUNTIME_MISSING", message: <hint>, retriable: false }`
//! payload defined by NL08 design §8 / DAG `[units.NL08]`.
//!
//! The envelope is wrapped inside the existing sqry-mcp 4-key wire
//! envelope (`kind` / `retryable` / `retry_after_ms` / `details`) so
//! the NL08 fields live inside `details`. The inner shape is what
//! NL08-aware MCP clients pattern-match on.

use serde_json::Value;
use sqry_mcp::tool_args::SqryAskParams;
use sqry_mcp::tool_handlers::execute_sqry_ask;

/// Build a minimal `SqryAskParams` pointing at a temp dir so the
/// translator-init path is the dominant fail-fast site.
fn make_params(path: &std::path::Path) -> SqryAskParams {
    SqryAskParams {
        query: "find functions".to_string(),
        path: path.display().to_string(),
        execute: false,
        model_dir: None,
        allow_unverified_model: false,
        allow_model_download: false,
    }
}

#[test]
#[serial_test::serial(workspace_env)]
fn mcp_envelope_has_code_and_hint() {
    // Engine cache + discovery cache must be initialised before
    // `execute_sqry_ask` reaches `engine_for_workspace`.
    let cap = std::num::NonZeroUsize::new(4).unwrap();
    sqry_mcp::test_setup::init_engine_cache(cap);
    sqry_mcp::test_setup::init_discovery_cache(cap);

    let tempdir = tempfile::TempDir::new().expect("tempdir");

    // Pin the NL02 resolver to the in-tree model fixtures so
    // `Translator::new` reaches `IntentClassifier::load` (and thus
    // the `SQRY_NL_FORCE_ORT_MISSING` seam) instead of short-circuiting
    // to no-classifier mode when no model_dir resolves.
    let model_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("sqry-nl/models");

    // SAFETY: env-var mutation. `serial_test::serial(workspace_env)`
    // ensures no parallel test reads/writes these vars concurrently.
    unsafe {
        std::env::set_var("SQRY_NL_FORCE_ORT_MISSING", "1");
        std::env::set_var("SQRY_NL_MODEL_DIR", &model_dir);
    }

    let params = make_params(tempdir.path());
    let result = execute_sqry_ask(&params);

    // Restore environment before any panic so subsequent tests in the
    // same binary aren't poisoned. SAFETY: same as above.
    unsafe {
        std::env::remove_var("SQRY_NL_FORCE_ORT_MISSING");
        std::env::remove_var("SQRY_NL_MODEL_DIR");
    }

    let err = result
        .err()
        .expect("translator init must fail when SQRY_NL_FORCE_ORT_MISSING=1");

    // The error is wrapped in anyhow but downcasts to `RpcError`.
    let rpc_err = err
        .downcast_ref::<sqry_mcp::error::RpcError>()
        .unwrap_or_else(|| {
            panic!(
                "expected anyhow error to downcast to RpcError, got: {err:?}\nchain:\n{}",
                err.chain()
                    .map(|c| format!("  - {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });

    // Outer envelope code: -32603 (Internal error) per design §8.
    assert_eq!(
        rpc_err.code, -32603,
        "RpcError.code must be -32603 (Internal error)"
    );

    // The structured 3-field NL08 envelope lives inside `details`.
    let details = rpc_err
        .details
        .as_ref()
        .expect("RpcError.details must be present for ONNX_RUNTIME_MISSING");

    let code = details.get("code").and_then(Value::as_str).unwrap_or("");
    let message = details.get("message").and_then(Value::as_str).unwrap_or("");
    let retriable = details.get("retriable").and_then(Value::as_bool);

    assert_eq!(
        code, "ONNX_RUNTIME_MISSING",
        "details.code must be 'ONNX_RUNTIME_MISSING', got: {details:?}"
    );
    assert!(
        !message.is_empty(),
        "details.message must carry the platform install hint, got: {details:?}"
    );
    assert_eq!(
        retriable,
        Some(false),
        "details.retriable must be exactly `false`, got: {details:?}"
    );

    // Sanity: the message field carries platform-specific guidance.
    let platform_marker = if cfg!(target_os = "linux") {
        "apt-get install libonnxruntime-dev"
    } else if cfg!(target_os = "macos") {
        "brew install onnxruntime"
    } else if cfg!(target_os = "windows") {
        "libonnxruntime.dll"
    } else {
        "libonnxruntime"
    };
    assert!(
        message.contains(platform_marker),
        "details.message must contain platform substring '{platform_marker}', got: {message:?}"
    );
}
