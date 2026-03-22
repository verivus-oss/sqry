//! Test verbosity configuration and logging setup
//!
//! This module provides utilities for enabling verbose logging in tests.
//! It's designed to be opt-in via environment variables and never panic,
//! ensuring test failures are never caused by logging configuration issues.
//!
//! # Usage
//!
//! ## Unit Tests
//!
//! ```rust,ignore
//! #[cfg(test)]
//! mod tests {
//!     use sqry_core::test_support::verbosity;
//!     use std::sync::Once;
//!
//!     static INIT: Once = Once::new();
//!
//!     fn init_logging() {
//!         INIT.call_once(|| {
//!             verbosity::init(env!("CARGO_PKG_NAME"));
//!         });
//!     }
//!
//!     #[test]
//!     fn my_test() {
//!         init_logging();
//!         log::info!("Test with verbose logging");
//!         // test body...
//!     }
//! }
//! ```
//!
//! ## Integration Tests
//!
//! ```rust,ignore
//! // tests/integration_test.rs
//! use sqry_core::test_support::verbosity;
//! use std::sync::Once;
//!
//! static INIT: Once = Once::new();
//!
//! fn init_logging() {
//!     INIT.call_once(|| {
//!         verbosity::init(env!("CARGO_PKG_NAME"));
//!     });
//! }
//!
//! #[test]
//! fn integration_test() {
//!     init_logging();
//!     log::info!("Integration test with verbose logging");
//!     // test body...
//! }
//! ```
//!
//! # Environment Variables
//!
//! - `SQRY_TEST_VERBOSE`: Enable verbose logging. Accepts:
//!   - `all`: Enable for all crates
//!   - Comma-separated list: `core,cli,plugin`
//!   - Specific crate name: `sqry-core`
//! - `SQRY_TEST_VERBOSE_LEVEL`: Log level override (`trace`, `debug`, `info`, `warn`, `error`)
//! - `SQRY_TEST_VERBOSE_ARTIFACTS`: Enable log file artifacts (creates files in `target/test-artifacts/`)
//!
//! # Examples
//!
//! ```bash
//! # Enable verbose logging for all tests
//! SQRY_TEST_VERBOSE=all cargo test
//!
//! # Enable for specific crates
//! SQRY_TEST_VERBOSE=core,cli cargo test
//!
//! # Enable with trace level
//! SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_LEVEL=trace cargo test
//!
//! # Enable with artifact files
//! SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_ARTIFACTS=1 cargo test
//! ```

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag to track if logging has been initialized
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize test logging for a specific crate
///
/// This function is idempotent and will only initialize logging once per process.
/// It reads environment variables to determine verbosity settings and never panics.
///
/// # Arguments
///
/// * `crate_name` - The name of the crate (typically `env!("CARGO_PKG_NAME")`)
///
/// # Error Handling
///
/// This function NEVER panics. All errors are logged to stderr and execution continues:
/// - Logger already initialized: Silently continues
/// - Invalid environment variable values: Logs warning and continues
/// - Artifact directory creation fails: Logs warning and continues without artifacts
///
/// # Examples
///
/// ```rust,ignore
/// // In test module
/// sqry_core::test_support::verbosity::init(env!("CARGO_PKG_NAME"));
/// ```
pub fn init(crate_name: &str) {
    // Check if already initialized (fast path - no allocation)
    if INITIALIZED.load(Ordering::Relaxed) {
        return;
    }

    // Check if logging should be enabled for this crate
    if !should_enable(crate_name) {
        return;
    }

    // Try to set initialized flag (atomic compare-and-swap)
    if INITIALIZED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        // Another thread beat us to initialization
        return;
    }

    // Configure env_logger builder
    let mut builder = env_logger::Builder::from_default_env();

    // Determine log level and apply it (only if RUST_LOG is not set)
    // If RUST_LOG is set, env_logger has already configured filters from it
    let level = determine_level();
    if env::var("RUST_LOG").is_err() {
        // RUST_LOG not set - apply our default level
        builder.filter_level(level);
    }
    // If RUST_LOG is set, we don't call filter_level() to preserve user's configuration

    // Set test mode
    builder.is_test(true);

    // Configure artifact writer if requested
    if env::var("SQRY_TEST_VERBOSE_ARTIFACTS").is_ok() {
        if let Some(writer) = super::artifacts::maybe_writer(crate_name) {
            builder.target(env_logger::Target::Pipe(Box::new(writer)));
        } else {
            // Failed to create artifact writer - log warning but continue
            eprintln!(
                "Warning: Failed to create artifact writer for {crate_name}. Continuing without artifacts."
            );
        }
    }

    // Try to initialize logger
    match builder.try_init() {
        Ok(()) => {
            log::info!("sqry test verbose logging enabled for {crate_name} (level: {level})");
        }
        Err(e) => {
            // Logger already initialized (e.g., by another test module)
            // This is expected in some scenarios - just log to stderr
            eprintln!("Info: Test logging already initialized for {crate_name}: {e}");
        }
    }

    // NO PANICS - always returns normally
}

/// Check if verbose logging should be enabled for the given crate
///
/// Reads `SQRY_TEST_VERBOSE` environment variable and matches against:
/// - `all`: Matches any crate
/// - Comma-separated list: Matches if crate name (or short name) is in list
/// - Empty/unset: Returns false
///
/// # Examples
///
/// ```rust,ignore
/// // SQRY_TEST_VERBOSE=all
/// assert!(should_enable("sqry-core")); // true
///
/// // SQRY_TEST_VERBOSE=core,cli
/// assert!(should_enable("sqry-core"));  // true
/// assert!(!should_enable("sqry-plugin")); // false
///
/// // SQRY_TEST_VERBOSE not set
/// assert!(!should_enable("sqry-core")); // false
/// ```
pub fn should_enable(crate_name: &str) -> bool {
    let Ok(verbose) = env::var("SQRY_TEST_VERBOSE") else {
        return false;
    };

    if verbose.trim().is_empty() {
        return false;
    }

    // Handle "all" case
    if verbose.trim().eq_ignore_ascii_case("all") {
        return true;
    }

    // Extract short name from crate name (sqry-core -> core)
    let short_name = crate_name.strip_prefix("sqry-").unwrap_or(crate_name);

    // Check comma-separated list
    verbose.split(',').map(str::trim).any(|target| {
        target.eq_ignore_ascii_case(crate_name) || target.eq_ignore_ascii_case(short_name)
    })
}

/// Determine the log level from environment variables
///
/// Checks (in order):
/// 1. `RUST_LOG` (if set, use its level)
/// 2. `SQRY_TEST_VERBOSE_LEVEL` (override level)
/// 3. Default to `info`
///
/// # Supported Values
///
/// - `trace`, `debug`, `info`, `warn`, `error`, `off`
/// - Case-insensitive
///
/// # Error Handling
///
/// Invalid values log a warning and default to `info`.
#[must_use]
pub fn determine_level() -> log::LevelFilter {
    // If RUST_LOG is set, respect it (don't override explicit user configuration)
    if env::var("RUST_LOG").is_ok() {
        // env_logger will parse RUST_LOG, so we just return a permissive filter
        return log::LevelFilter::Trace;
    }

    // Check for explicit level override
    let Ok(level_str) = env::var("SQRY_TEST_VERBOSE_LEVEL") else {
        return log::LevelFilter::Info;
    };

    // Parse level string
    match level_str.trim().to_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "info" => log::LevelFilter::Info,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        invalid => {
            eprintln!(
                "Warning: Invalid SQRY_TEST_VERBOSE_LEVEL '{invalid}'. Valid values: trace, debug, info, warn, error, off. Defaulting to 'info'."
            );
            log::LevelFilter::Info
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    // Mutex to ensure environment variable tests don't run concurrently
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_should_enable_with_all() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "all");
        }
        assert!(should_enable("sqry-core"));
        assert!(should_enable("sqry-cli"));
        assert!(should_enable("any-crate"));
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_should_enable_with_comma_list() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "core,cli");
        }

        // Should match full name
        assert!(should_enable("sqry-core"));
        assert!(should_enable("sqry-cli"));

        // Should not match other crates
        assert!(!should_enable("sqry-plugin"));
        assert!(!should_enable("sqry-mcp"));

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_should_enable_with_short_names() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "core");
        }

        // Should match both full and short name
        assert!(should_enable("sqry-core"));

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_should_enable_case_insensitive() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "CORE,CLI");
        }
        assert!(should_enable("sqry-core"));
        assert!(should_enable("sqry-cli"));
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_should_enable_disabled_by_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
        assert!(!should_enable("sqry-core"));
    }

    #[test]
    fn test_should_enable_with_whitespace() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", " core , cli ");
        }
        assert!(should_enable("sqry-core"));
        assert!(should_enable("sqry-cli"));
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_determine_level_defaults_to_info() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("RUST_LOG");
        }
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_LEVEL");
        }
        assert_eq!(determine_level(), log::LevelFilter::Info);
    }

    #[test]
    fn test_determine_level_respects_rust_log() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("RUST_LOG", "debug");
        }
        // Should return Trace to allow env_logger to parse RUST_LOG
        assert_eq!(determine_level(), log::LevelFilter::Trace);
        unsafe {
            env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_determine_level_with_override() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("RUST_LOG");
        }

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "trace");
        }
        assert_eq!(determine_level(), log::LevelFilter::Trace);

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "debug");
        }
        assert_eq!(determine_level(), log::LevelFilter::Debug);

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "warn");
        }
        assert_eq!(determine_level(), log::LevelFilter::Warn);

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "error");
        }
        assert_eq!(determine_level(), log::LevelFilter::Error);

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "off");
        }
        assert_eq!(determine_level(), log::LevelFilter::Off);

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_LEVEL");
        }
    }

    #[test]
    fn test_determine_level_case_insensitive() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("RUST_LOG");
        }

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "DEBUG");
        }
        assert_eq!(determine_level(), log::LevelFilter::Debug);

        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "TrAcE");
        }
        assert_eq!(determine_level(), log::LevelFilter::Trace);

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_LEVEL");
        }
    }

    #[test]
    fn test_determine_level_invalid_value_defaults_to_info() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("RUST_LOG");
        }
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_LEVEL", "invalid");
        }
        assert_eq!(determine_level(), log::LevelFilter::Info);
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_LEVEL");
        }
    }

    #[test]
    fn test_init_is_idempotent() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "all");
        }

        // First call should succeed
        init("test-crate");

        // Second call should not panic
        init("test-crate");
        init("test-crate");

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }

    #[test]
    fn test_init_with_invalid_crate_does_not_panic() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE", "other-crate");
        }

        // Should not panic even with non-matching crate
        init("sqry-core");

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE");
        }
    }
}
