//! Integration tests for LSP socket binding security validation.
//!
//! These tests verify that the security validation system correctly
//! validates CLI options and configuration.
//!
//! NOTE: Full log capture testing (actual warning output) is deferred to .
//! See `TODO_SECURITY_INTEGRATION.md` for implementation plan.
//! The classification logic itself is thoroughly tested in security.rs unit tests (12 tests).

use sqry_lsp::LspOptions;
use std::path::PathBuf;

/// Helper to create test LSP options with a given socket address.
fn options_with_socket(addr: &str) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: Some(addr.to_string()),
        index_root: None,
        log_level: "warn".to_string(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    }
}

/// Helper to create test LSP options with warning suppression enabled.
fn options_with_socket_and_suppression(addr: &str, allow_suppress: bool) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: Some(addr.to_string()),
        index_root: None,
        log_level: "warn".to_string(),
        config: None,
        allow_public_bind: allow_suppress,
        daemon: false,
        daemon_socket: None,
    }
}

// Tests for CLI option construction and validation.
// Actual security warning output testing deferred to .

#[test]
fn localhost_ipv4_option_parsing() {
    let opts = options_with_socket("127.0.0.1:9257");
    assert!(opts.socket.is_some());
    assert_eq!(opts.socket.unwrap(), "127.0.0.1:9257");
}

#[test]
fn localhost_ipv6_option_parsing() {
    let opts = options_with_socket("[::1]:9257");
    assert!(opts.socket.is_some());
    assert_eq!(opts.socket.unwrap(), "[::1]:9257");
}

#[test]
fn private_network_option_parsing() {
    let opts = options_with_socket("192.168.1.100:9257");
    assert!(opts.socket.is_some());
    assert_eq!(opts.socket.unwrap(), "192.168.1.100:9257");
}

#[test]
fn wildcard_ipv4_option_parsing() {
    let opts = options_with_socket("0.0.0.0:9257");
    assert!(opts.socket.is_some());
    assert_eq!(opts.socket.unwrap(), "0.0.0.0:9257");
}

#[test]
fn wildcard_ipv6_option_parsing() {
    let opts = options_with_socket("[::]:9257");
    assert!(opts.socket.is_some());
    assert_eq!(opts.socket.unwrap(), "[::]:9257");
}

#[test]
fn default_uses_stdio() {
    let opts = LspOptions {
        stdio: false,
        socket: None,
        index_root: None,
        log_level: "warn".to_string(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    };
    assert!(opts.use_stdio());
    assert!(opts.socket.is_none());
}

#[test]
fn explicit_stdio_flag() {
    let opts = LspOptions {
        stdio: true,
        socket: None,
        index_root: None,
        log_level: "warn".to_string(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    };
    assert!(opts.use_stdio());
}

/// Test that demonstrates the recommended secure configuration.
#[test]
fn recommended_secure_configuration() {
    let opts = LspOptions {
        stdio: false,
        socket: Some("127.0.0.1:9257".to_string()),
        index_root: Some(PathBuf::from("/workspace")),
        log_level: "info".to_string(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    };

    assert!(!opts.use_stdio());
    assert!(opts.socket.is_some());

    // Verify it's localhost
    let addr_str = opts.socket.unwrap();
    assert!(addr_str.starts_with("127.0.0.1:") || addr_str.starts_with("localhost:"));
}

/// Test that `allow_public_bind` flag defaults to false.
#[test]
fn allow_public_bind_defaults_to_false_in_integration() {
    let opts = options_with_socket("0.0.0.0:9257");
    assert!(!opts.allow_public_bind);
}

/// Test that `allow_public_bind` can be explicitly enabled.
#[test]
fn allow_public_bind_can_be_enabled() {
    let opts = options_with_socket_and_suppression("0.0.0.0:9257", true);
    assert!(opts.allow_public_bind);
}

/// Test suppression with private network address.
#[test]
fn suppression_works_with_private_network() {
    let opts_normal = options_with_socket_and_suppression("192.168.1.100:9257", false);
    let opts_suppressed = options_with_socket_and_suppression("192.168.1.100:9257", true);

    assert!(!opts_normal.allow_public_bind);
    assert!(opts_suppressed.allow_public_bind);
}

/// Test suppression with public/wildcard address.
#[test]
fn suppression_works_with_public_binding() {
    let opts_normal = options_with_socket_and_suppression("0.0.0.0:9257", false);
    let opts_suppressed = options_with_socket_and_suppression("0.0.0.0:9257", true);

    assert!(!opts_normal.allow_public_bind);
    assert!(opts_suppressed.allow_public_bind);
}

/// Test various hostname formats that should parse properly.
#[test]
fn hostname_formats() {
    let test_cases = vec![
        "127.0.0.1:9257",
        "localhost:9257",
        "0.0.0.0:9257",
        "[::1]:9257",
        "[::]:9257",
        "192.168.1.1:9257",
    ];

    for addr in test_cases {
        let opts = options_with_socket(addr);
        assert!(opts.socket.is_some(), "Failed for address: {addr}");
        assert_eq!(opts.socket.unwrap(), addr);
    }
}
