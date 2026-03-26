/// Common test utilities for sqry-cli integration tests
use std::path::PathBuf;

/// Helper to find the sqry binary for testing
///
/// Tries `CARGO_BIN_EXE_sqry` first, then falls back to looking in target/debug or target/release.
/// This makes tests work both in CI (where `CARGO_BIN_EXE_sqry` is set) and locally
/// (where it might not be set in workspace contexts).
#[allow(dead_code)] // Used by integration tests; keep available for new tests
pub fn sqry_bin() -> PathBuf {
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let make_candidate = |base: &str| {
        if exe_suffix.is_empty() {
            PathBuf::from(base)
        } else {
            PathBuf::from(format!("{base}{exe_suffix}"))
        }
    };

    std::env::var("CARGO_BIN_EXE_sqry")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Fallback: look in target/debug or target/release
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let workspace_dir = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();

            let debug_path = workspace_dir.join(make_candidate("target/debug/sqry"));
            let release_path = workspace_dir.join(make_candidate("target/release/sqry"));

            if debug_path.exists() {
                debug_path
            } else if release_path.exists() {
                release_path
            } else {
                panic!(
                    "Could not find sqry binary. Tried:\n  - CARGO_BIN_EXE_sqry environment variable\n  - {}\n  - {}\n\nRun `cargo build` first.",
                    debug_path.display(),
                    release_path.display()
                );
            }
        })
}
