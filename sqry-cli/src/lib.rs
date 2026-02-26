//! sqry-cli library exports
//!
//! This module exists to make certain functionality available to integration tests
//! and benchmarks. The main binary is still in `main.rs`.

pub mod args;
pub mod commands;
pub mod error;
pub mod index_discovery;
pub mod output;
pub mod persistence;
pub mod plugin_defaults;
pub mod progress;
