//! Test support utilities for sqry
//!
//! This module provides utilities for testing sqry components, including
//! verbose logging configuration, test artifact generation, and plugin management.
//!
//! # Features
//!
//! - **Verbosity Control**: Environment-variable driven logging for tests
//! - **Artifact Generation**: Optional file-based log capture for post-mortem analysis
//! - **Plugin Factory**: Pre-configured PluginManager with all built-in language plugins
//! - **Zero-Overhead**: Completely opt-in; no impact on test performance when disabled
//!
//! # Quick Start
//!
//! Add to your test module:
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
//!         log::info!("This will be logged when SQRY_TEST_VERBOSE=all");
//!         // test body...
//!     }
//! }
//! ```
//!
//! Then run tests with verbose logging:
//!
//! ```bash
//! # Enable verbose output for all tests
//! SQRY_TEST_VERBOSE=all cargo test
//!
//! # Enable for specific crates
//! SQRY_TEST_VERBOSE=core,cli cargo test
//!
//! # Enable with trace level
//! SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_LEVEL=trace cargo test
//!
//! # Capture logs to files
//! SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_ARTIFACTS=1 cargo test
//! ```
//!
//! # Environment Variables
//!
//! - `SQRY_TEST_VERBOSE`: Enable verbose mode
//!   - `all` - Enable for all crates
//!   - Comma-separated list: `core,cli,plugin`
//!   - Specific crate: `sqry-core`
//!
//! - `SQRY_TEST_VERBOSE_LEVEL`: Log level (default: `info`)
//!   - `trace`, `debug`, `info`, `warn`, `error`
//!
//! - `SQRY_TEST_VERBOSE_ARTIFACTS`: Enable file artifacts
//!   - Set to any value to enable
//!   - Files written to `target/test-artifacts/<crate>/<timestamp>.log`
//!
//! # Design Principles
//!
//! 1. **Never Panic**: Logging failures should never break tests
//! 2. **Opt-In**: Zero overhead when disabled (environment check only)
//! 3. **Idempotent**: Safe to call `init()` multiple times
//! 4. **Thread-Safe**: Uses atomic operations for initialization guard
//! 5. **Collision-Resistant**: Artifact filenames use millisecond timestamps + counters
//!
//! # Module Organization
//!
//! - [`verbosity`]: Core logging initialization and environment variable parsing
//! - [`artifacts`]: File-based log capture with collision-resistant naming
//! - [`plugin_factory`]: Pre-configured PluginManager with all built-in language plugins

pub mod artifacts;
pub mod plugin_factory;
pub mod verbosity;

// Re-export commonly used items
pub use verbosity::init;
