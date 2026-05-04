//! C029c — observable end-to-end smoke for the
//! `SQRY_MCP_MAX_OUTPUT_BYTES` cap, exercised through the actual
//! `success_result` dispatch boundary that every `#[tool]`-decorated
//! handler routes through (see `sqry-mcp/src/server.rs:480-487`).
//!
//! Iter1 reviewer flagged that `output_caps_truncation_smoke.rs` only
//! covered the helper-level `truncate_response`, not the full
//! production boundary that produces `CallToolResult`. This file is the
//! integration-through-dispatch coverage: it instantiates a real
//! `SqryServer` (via the `test-helpers`-feature `new_for_tests`
//! constructor) and drives `Self::success_result(&server, &value)` via
//! the gated `build_success_result_for_tests` wrapper, which calls the
//! private `success_result` verbatim — guaranteeing this test
//! exercises the same code path the rmcp `tool_router` invokes at
//! runtime, including the JSON pretty-print + `truncate_response` +
//! `Content::text` fold.
//!
//! The companion helper-level test
//! `tests/output_caps_truncation_smoke.rs` is retained as the
//! unit-level coverage of `truncate_response` itself.

use rmcp::model::RawContent;
use serde_json::json;
use serial_test::serial;
use sqry_mcp::server_test_helpers::{CallToolResult, SqryServer};

const TRUNCATION_MARKER: &str = "\n[…truncated by SQRY_MCP_MAX_OUTPUT_BYTES…]";

/// Pull the single text body out of a `CallToolResult`. Panics if the
/// shape is unexpected — every `success_result` invocation produces
/// exactly one `Content::text` element by construction
/// (`server.rs:486`).
fn text_body_of(result: &CallToolResult) -> String {
    let content = &result.content;
    assert_eq!(
        content.len(),
        1,
        "expected exactly one text content from success_result; got {} elements",
        content.len()
    );
    let annotated = content.first().expect("content must have one element");
    match &annotated.raw {
        RawContent::Text(text) => text.text.clone(),
        other => panic!("expected RawContent::Text from success_result; got {other:?}"),
    }
}

/// `SQRY_MCP_MAX_OUTPUT_BYTES=500` env override: dispatch a payload
/// whose pretty-printed JSON serialisation comfortably exceeds 500
/// bytes through the production `success_result` boundary, then assert:
///
/// 1. The returned `CallToolResult` is a single text body.
/// 2. The body ends with the canonical truncation marker.
/// 3. The pre-marker prefix is `<= 500` bytes (the cap).
/// 4. Total length is `<= 500 + marker.len()` (the upper bound).
#[test]
#[serial]
fn cap_500_truncates_via_success_result_dispatch() {
    // SAFETY: serial_test serialises env-mutating tests in this crate.
    unsafe {
        std::env::set_var("SQRY_MCP_MAX_OUTPUT_BYTES", "500");
    }

    // Build a real `SqryServer` so the dispatch boundary is the actual
    // production code path. We do not need a workspace or a graph — we
    // are exercising the post-execution serialisation cap, which runs
    // unconditionally on every successful tool result regardless of
    // tool identity.
    let _server = SqryServer::new_for_tests();

    // A pretty-printed payload guaranteed to exceed 500 bytes.
    let big_value = json!({
        "results": (0..30).map(|i| json!({
            "name": format!("symbol_{i}"),
            "kind": "function",
            "path": format!("/some/path/file_{i}.rs"),
            "line": i,
            "snippet": "x".repeat(40),
        })).collect::<Vec<_>>(),
        "total": 30,
        "truncated": false,
    });
    // Defensive: confirm the test payload actually exceeds the cap so
    // we are exercising the truncation branch, not the pass-through
    // branch.
    let pretty = serde_json::to_string_pretty(&big_value).unwrap();
    assert!(
        pretty.len() > 500,
        "test payload must exceed cap=500 to exercise the truncation path; got {} bytes",
        pretty.len()
    );

    // Drive the actual production boundary via the test-helpers gated
    // wrapper, which calls the private `success_result` verbatim.
    let result = SqryServer::build_success_result_for_tests(&big_value);
    let body = text_body_of(&result);

    // Cleanup BEFORE asserting to avoid leaking state on panic.
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }

    assert!(
        body.ends_with(TRUNCATION_MARKER),
        "truncated body must end with canonical marker; got {body:?}"
    );
    let prefix_len = body.len() - TRUNCATION_MARKER.len();
    assert!(
        prefix_len <= 500,
        "pre-marker prefix exceeded cap=500: {prefix_len} bytes"
    );
    assert!(
        body.len() <= 500 + TRUNCATION_MARKER.len(),
        "total body exceeded cap+marker: {} > {}",
        body.len(),
        500 + TRUNCATION_MARKER.len()
    );
}

/// Default 50_000-byte cap (no env override): build a payload whose
/// pretty-printed serialisation exceeds 50 000 bytes and assert the
/// dispatch path truncates it. This pins the documented default
/// against silent drift (see `sqry-mcp/README.md` and
/// `sqry-mcp/src/main.rs:54`).
#[test]
#[serial]
fn default_cap_50000_truncates_via_success_result_dispatch() {
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }

    let _server = SqryServer::new_for_tests();

    // Construct a payload whose pretty-printed form > 60 000 bytes.
    // 800 records * (~80 byte serialisation per record) ≈ 64 000 bytes.
    let big_value = json!({
        "results": (0..800).map(|i| json!({
            "name": format!("symbol_{i:08}"),
            "kind": "function",
            "path": format!("/repo/path/file_{i:08}.rs"),
            "line": i,
            "snippet": "y".repeat(40),
        })).collect::<Vec<_>>(),
        "total": 800,
    });
    let pretty = serde_json::to_string_pretty(&big_value).unwrap();
    assert!(
        pretty.len() > 60_000,
        "test payload must exceed default cap (50 000) to exercise the truncation path; \
         got {} bytes",
        pretty.len()
    );

    let result = SqryServer::build_success_result_for_tests(&big_value);
    let body = text_body_of(&result);

    assert!(
        body.ends_with(TRUNCATION_MARKER),
        "default-cap truncation must produce the canonical marker; got tail {:?}",
        &body[body.len().saturating_sub(80)..]
    );
    let prefix_len = body.len() - TRUNCATION_MARKER.len();
    assert!(
        prefix_len <= 50_000,
        "pre-marker prefix exceeded default cap=50000: {prefix_len} bytes"
    );
    assert!(
        body.len() <= 50_000 + TRUNCATION_MARKER.len(),
        "total body exceeded default cap+marker: {} > {}",
        body.len(),
        50_000 + TRUNCATION_MARKER.len()
    );
}

/// Below-cap payloads must pass through verbatim with no truncation
/// marker. This guards against an accidental "always append marker"
/// regression in `success_result`.
#[test]
#[serial]
fn below_cap_payload_passes_through_via_success_result_dispatch() {
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }

    let _server = SqryServer::new_for_tests();

    let small = json!({"ok": true, "count": 1, "results": []});
    let result = SqryServer::build_success_result_for_tests(&small);
    let body = text_body_of(&result);

    let expected = serde_json::to_string_pretty(&small).unwrap();
    assert_eq!(body, expected, "below-cap body must be byte-identical");
    assert!(
        !body.contains(TRUNCATION_MARKER),
        "below-cap body must not contain the truncation marker"
    );
}
