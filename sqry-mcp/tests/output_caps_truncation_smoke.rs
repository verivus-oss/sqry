//! C029c — Output truncation cap end-to-end smoke test.
//!
//! Verifies that the `SQRY_MCP_MAX_OUTPUT_BYTES` env-var cap is
//! honoured at the `success_result` boundary (single-site enforcement
//! covering all `#[tool]`-annotated handlers per `server.rs:472-477`).
//!
//! Test strategy: drive the public `sqry_mcp::output_caps` API
//! directly with the env var in place. This is the same code path
//! `SqryServer::success_result` invokes after JSON serialisation, so
//! covering it via the public lib surface gives a deterministic
//! assertion without standing up a full rmcp server. Serialised
//! `serde_json::to_string_pretty(value)` output is the exact input
//! shape the truncation function sees at the production boundary.

use serde_json::json;
use serial_test::serial;
use sqry_mcp::output_caps::{DEFAULT_MAX_OUTPUT_BYTES, max_output_bytes, truncate_response};

const TRUNCATION_MARKER: &str = "\n[…truncated by SQRY_MCP_MAX_OUTPUT_BYTES…]";

/// With the documented default (no env override), the cap must be
/// `DEFAULT_MAX_OUTPUT_BYTES` (50 000) — pinned against silent drift
/// from `sqry-mcp/README.md` and `sqry-mcp/src/main.rs:54`.
#[test]
#[serial]
fn default_cap_matches_documented_50_000() {
    // SAFETY: serial_test serialises env-mutating tests in this crate.
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }
    assert_eq!(max_output_bytes(), 50_000);
    assert_eq!(max_output_bytes(), DEFAULT_MAX_OUTPUT_BYTES);
}

/// `SQRY_MCP_MAX_OUTPUT_BYTES=10` env override must be honoured: a
/// large pretty-printed JSON payload (mirroring what
/// `SqryServer::success_result` produces) gets truncated, the response
/// body ends with the canonical truncation marker, and the body length
/// is `<= cap + marker.len()`.
#[test]
#[serial]
fn cap_10_truncates_large_payload_with_marker() {
    // Construct a large JSON payload similar to what an MCP tool
    // (e.g. `sqry_query` over a fixture with many results) would
    // produce: a few KB of structured data.
    let big_value = json!({
        "results": (0..100).map(|i| json!({
            "name": format!("symbol_{i}"),
            "kind": "function",
            "path": format!("/some/path/file_{i}.rs"),
            "line": i,
            "snippet": "a".repeat(50),
        })).collect::<Vec<_>>(),
        "total": 100,
        "truncated": false,
    });
    let serialised = serde_json::to_string_pretty(&big_value).unwrap();
    assert!(
        serialised.len() > 10,
        "test payload must exceed cap=10 to exercise the truncation path; got {} bytes",
        serialised.len()
    );

    // SAFETY: serial_test guarantees no concurrent env mutation across
    // tests in this crate.
    unsafe {
        std::env::set_var("SQRY_MCP_MAX_OUTPUT_BYTES", "10");
    }
    let cap = max_output_bytes();
    let out = truncate_response(&serialised, cap).into_owned();
    // Cleanup BEFORE asserting to avoid leaking state on panic.
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }

    assert_eq!(
        cap, 10,
        "env override SQRY_MCP_MAX_OUTPUT_BYTES=10 not read"
    );
    assert!(
        out.ends_with(TRUNCATION_MARKER),
        "truncated response must end with canonical marker; got {out:?}"
    );
    // Body length: the truncation marker is appended; the prefix must
    // be at most cap bytes (UTF-8 boundary safe).
    let body_len = out.len() - TRUNCATION_MARKER.len();
    assert!(
        body_len <= 10,
        "truncated body exceeded cap: {body_len} > 10 (out={out:?})"
    );
    // Total length: cap + marker length is the upper bound.
    assert!(
        out.len() <= 10 + TRUNCATION_MARKER.len(),
        "total truncated response exceeded cap+marker: {} > {}",
        out.len(),
        10 + TRUNCATION_MARKER.len()
    );
}

/// Small payloads (`<= cap`) must pass through verbatim — no
/// truncation, no marker, byte-identical output.
#[test]
#[serial]
fn small_payload_under_cap_passes_through_verbatim() {
    unsafe {
        std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
    }
    let small = json!({"ok": true, "count": 1});
    let serialised = serde_json::to_string_pretty(&small).unwrap();
    let out = truncate_response(&serialised, max_output_bytes()).into_owned();
    assert_eq!(out, serialised);
    assert!(
        !out.contains(TRUNCATION_MARKER),
        "untruncated response must not contain the marker"
    );
}
