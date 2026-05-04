//! Output truncation for MCP tool responses.
//!
//! Provides a single-source-of-truth cap on the byte-length of each
//! tool response payload, configurable via `SQRY_MCP_MAX_OUTPUT_BYTES`
//! (default: 50 000 bytes). The cap is enforced at the
//! [`crate::server::SqryServer::success_result`] boundary so every
//! `#[tool]`-annotated handler is covered uniformly.
//!
//! # Behaviour
//!
//! * If the serialised response is `<= cap`, it is passed through
//!   verbatim.
//! * Otherwise the response is truncated at the largest UTF-8 character
//!   boundary `<= cap` and a fixed marker
//!   `[…truncated by SQRY_MCP_MAX_OUTPUT_BYTES…]` is appended so
//!   downstream consumers can detect truncation deterministically.
//!
//! # Stability
//!
//! The default cap is pinned at compile time via a `const _:` assert so
//! drift between this module and the matching MCP README documentation
//! row (`SQRY_MCP_MAX_OUTPUT_BYTES (default: 50000)`) is impossible
//! without breaking the build. No new dependency is required —
//! `const _: () = assert!(...);` works on stable Rust ≥ 1.57.

use std::borrow::Cow;

/// Default maximum response size in bytes (50 000 = ~50 KB).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 50_000;

// Compile-time pin: any drift here would silently desync from the
// `sqry-mcp/README.md` env-var table and the help text in `main.rs`.
const _: () = assert!(DEFAULT_MAX_OUTPUT_BYTES == 50_000);

/// Truncation marker appended to over-cap responses.
const TRUNCATION_MARKER: &str = "\n[…truncated by SQRY_MCP_MAX_OUTPUT_BYTES…]";

/// Read the configured maximum output byte cap from the environment.
///
/// Reads `SQRY_MCP_MAX_OUTPUT_BYTES`. If unset or unparseable as
/// `usize`, falls back to [`DEFAULT_MAX_OUTPUT_BYTES`].
#[must_use]
pub fn max_output_bytes() -> usize {
    std::env::var("SQRY_MCP_MAX_OUTPUT_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)
}

/// Truncate a string to at most `cap` bytes (UTF-8 boundary safe),
/// appending [`TRUNCATION_MARKER`] when truncation occurred.
///
/// Returns `Cow::Borrowed(s)` when no truncation is needed (the common
/// case) and `Cow::Owned(...)` only when the cap is exceeded. UTF-8
/// boundary safety is preserved by walking `char_indices` and stopping
/// at the largest character whose end offset is `<= cap`.
#[must_use]
pub fn truncate_response(s: &str, cap: usize) -> Cow<'_, str> {
    if s.len() <= cap {
        return Cow::Borrowed(s);
    }
    // Find the largest UTF-8 boundary <= cap. `char_indices` gives us
    // (byte_offset, char) pairs; we keep advancing while the END of the
    // current char (`i + len_utf8`) is still within cap, then take that
    // end position. If even the first char doesn't fit, we yield 0
    // (yielding just the marker, which is the safest possible
    // truncation).
    let end = s
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|end| *end <= cap)
        .last()
        .unwrap_or(0);
    Cow::Owned(format!("{}{TRUNCATION_MARKER}", &s[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// With no `SQRY_MCP_MAX_OUTPUT_BYTES` env var set,
    /// [`max_output_bytes`] must return [`DEFAULT_MAX_OUTPUT_BYTES`]
    /// (50 000). This pins the documented default in
    /// `sqry-mcp/README.md` and `sqry-mcp/src/main.rs:54` against
    /// silent drift.
    #[test]
    #[serial]
    fn max_output_bytes_default_is_50_000() {
        // SAFETY: serial_test guarantees no other test in this crate
        // mutates the env var concurrently.
        unsafe {
            std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
        }
        assert_eq!(max_output_bytes(), 50_000);
        assert_eq!(max_output_bytes(), DEFAULT_MAX_OUTPUT_BYTES);
    }

    /// `SQRY_MCP_MAX_OUTPUT_BYTES=10` must be honoured (env-var read
    /// path).
    #[test]
    #[serial]
    fn max_output_bytes_reads_env_var() {
        // SAFETY: serial_test serialises env-mutating tests across this
        // crate.
        unsafe {
            std::env::set_var("SQRY_MCP_MAX_OUTPUT_BYTES", "10");
        }
        let v = max_output_bytes();
        // Cleanup BEFORE asserting to avoid leaking on panic.
        unsafe {
            std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
        }
        assert_eq!(v, 10);
    }

    /// Invalid env-var values must fall back to the default.
    #[test]
    #[serial]
    fn max_output_bytes_invalid_falls_back_to_default() {
        unsafe {
            std::env::set_var("SQRY_MCP_MAX_OUTPUT_BYTES", "not-a-number");
        }
        let v = max_output_bytes();
        unsafe {
            std::env::remove_var("SQRY_MCP_MAX_OUTPUT_BYTES");
        }
        assert_eq!(v, DEFAULT_MAX_OUTPUT_BYTES);
    }

    /// Short strings (`<= cap`) must pass through verbatim and as
    /// `Cow::Borrowed` (zero-copy fast path).
    #[test]
    fn truncate_response_under_cap_is_borrowed_passthrough() {
        let s = "small payload";
        let out = truncate_response(s, 1024);
        assert_eq!(out.as_ref(), s);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    /// Strings strictly over `cap` must be truncated and marker-tagged.
    #[test]
    fn truncate_response_over_cap_emits_marker() {
        let big = "a".repeat(100);
        let out = truncate_response(&big, 10);
        assert!(
            out.ends_with("[…truncated by SQRY_MCP_MAX_OUTPUT_BYTES…]"),
            "expected truncation marker, got {out:?}"
        );
        // Body before marker must be at most cap bytes.
        let body_len = out.len() - TRUNCATION_MARKER.len();
        assert!(
            body_len <= 10,
            "truncated body exceeded cap: {body_len} > 10"
        );
    }

    /// Equality at the cap boundary (`s.len() == cap`) must NOT
    /// truncate.
    #[test]
    fn truncate_response_at_exact_cap_is_passthrough() {
        let s = "0123456789"; // len 10
        let out = truncate_response(s, 10);
        assert_eq!(out.as_ref(), s);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    /// UTF-8 multibyte characters must not be split mid-codepoint when
    /// truncation lands inside one.
    #[test]
    fn truncate_response_utf8_boundary_safe() {
        // Each "é" is 2 bytes in UTF-8. With cap=3 we should keep one
        // "é" (2 bytes) and append the marker — never split a
        // codepoint.
        let s = "ééééé"; // 10 bytes
        let out = truncate_response(s, 3);
        // Strip the marker and validate the prefix is valid UTF-8.
        let prefix = out
            .as_ref()
            .strip_suffix(TRUNCATION_MARKER)
            .expect("marker must be present");
        assert!(
            std::str::from_utf8(prefix.as_bytes()).is_ok(),
            "truncated prefix must be valid UTF-8: {prefix:?}"
        );
        // We expect exactly one "é" (2 bytes) — the second would push
        // the byte length to 4, exceeding cap=3.
        assert_eq!(prefix, "é");
    }
}
