# TODO: Proper Security Integration Tests

## Issue

The current `security_validation.rs` tests don't actually exercise the `validate_bind_address()` function - they only test CLI option parsing.

**Code Review Finding (2025-10-30)**:
> MEDIUM sqry-lsp/tests/security_validation.rs (line 20) – These "integration" tests never hit validate_bind_address or observe the warned output; they only re-check the LspOptions struct. The new security logic could regress or be deleted and the tests would still pass.

## Root Cause

The `validate_bind_address()` function is called inside `serve_socket()`, which requires:
1. Actually binding to a socket
2. Capturing log output from the running server
3. Potentially spawning the server in a test harness

This is non-trivial for unit tests and requires proper integration test infrastructure.

## Current Coverage

**What IS tested**:
- Unit tests in `security.rs` (12 tests) - validate classification logic directly
- CLI option parsing (LspOptions struct construction)

**What is NOT tested**:
- Actual warning output when server starts
- Integration between CLI args and security validation
- Log capture of security warnings

## Proposed Solution (for  or later)

### Option 1: Direct Function Testing (Simpler)
Make `validate_bind_address()` public and test it directly:

```rust
#[test]
fn test_validate_bind_address_warnings() {
    // Set up log capture
    let logger = LogCapture::new();
    log::set_boxed_logger(Box::new(logger)).unwrap();

    // Test localhost (no warning)
    let addr = "127.0.0.1:9257".parse().unwrap();
    sqry_lsp::security::validate_bind_address(addr);
    assert!(!logger.has_warnings());

    // Test wildcard (strong warning)
    logger.clear();
    let addr = "0.0.0.0:9257".parse().unwrap();
    sqry_lsp::security::validate_bind_address(addr);
    assert!(logger.has_warning_containing("SECURITY WARNING"));
}
```

### Option 2: Server Startup Testing (More Complete)
Spawn server in test mode and capture logs:

```rust
#[tokio::test]
async fn test_server_startup_security_warnings() {
    let logger = LogCapture::new();
    log::set_boxed_logger(Box::new(logger)).unwrap();

    // Start server on wildcard address
    let opts = LspOptions {
        socket: Some("0.0.0.0:9257".to_string()),
        ..Default::default()
    };

    // Spawn server (will log warnings)
    tokio::spawn(async move {
        sqry_lsp::run(opts).await
    });

    // Give it time to log
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Assert warnings were logged
    assert!(logger.has_warning_containing("SECURITY WARNING"));
}
```

### Option 3: Mock-Based Testing (Most Isolated)
Extract logging into a trait and mock it:

```rust
trait SecurityLogger {
    fn warn_private_network(&self, addr: SocketAddr);
    fn warn_public_bind(&self, addr: SocketAddr);
}

// Production: logs to `log` crate
// Testing: captures to vector

#[test]
fn test_security_validation_with_mock() {
    let mock_logger = MockSecurityLogger::new();
    validate_bind_address_with_logger(addr, &mock_logger);
    assert_eq!(mock_logger.warnings.len(), 1);
}
```

## Recommendation

**For v1.9.0 (current)**: Accept the limitation, document it here
- Unit tests provide adequate coverage of classification logic
- Integration testing deferred to when it's needed

**For  (security enhancements)**: Implement Option 1 (direct function testing)
- Simplest to implement
- Provides the missing coverage
- No architectural changes needed
- Add to `05_TEST_PLAN.md` for that FR

## Current Mitigation

The unit tests in `security.rs` DO test the classification logic thoroughly:
- ✅ `classify_ipv4_localhost`
- ✅ `classify_ipv4_private_network`
- ✅ `classify_ipv4_public`
- ✅ `classify_ipv6_localhost`
- ✅ `classify_ipv6_unspecified`
- ✅ `classify_ipv6_public`

The `validate_bind_address()` function is a thin wrapper that:
1. Calls `BindSecurity::classify()` (tested)
2. Calls `log::warn!()` (stdlib, trusted)
3. Has no branching logic beyond `match` on enum (trivial)

**Risk Assessment**: LOW - Core logic is tested, only logging path untested

## Action Items

- [ ] Document this limitation in v1.9.0 release notes
- [ ] Add proper integration tests in  (security enhancements)
- [ ] Consider making `security` module public for easier testing
- [ ] Add this TODO to  SPEC as acceptance criteria

## References

- Current implementation: `sqry-lsp/src/security.rs`
- Unit tests: `sqry-lsp/src/security.rs#tests` (12 tests, all passing)
- Integration tests: `sqry-lsp/tests/security_validation.rs` (CLI only)
- Code review: Code review feedback 2025-10-30
