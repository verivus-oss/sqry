/// Common test utilities for sqry-cli integration tests
use std::path::PathBuf;

/// Locate the `sqry` binary for testing.
///
/// Delegates to the one resolver, `sqry_core::test_support::binaries::sqry_binary`
/// (surface parity W4, design W4-D13), which reads `SQRY_E2E_SQRY_BIN`, then
/// `CARGO_BIN_EXE_sqry`, then `CARGO_TARGET_DIR` and the workspace `target`,
/// debug before release, and panics naming every variable and candidate.
#[allow(dead_code)] // Used by integration tests; keep available for new tests
pub fn sqry_bin() -> PathBuf {
    sqry_core::test_support::binaries::sqry_binary()
}
