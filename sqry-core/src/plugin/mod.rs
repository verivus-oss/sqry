//! Plugin system for language support.
//!
//! This module provides a trait-based plugin architecture that allows sqry to support
//! multiple programming languages through both built-in (statically linked) and external
//! (dynamically loaded in Phase 3+) plugins.
//!
//! # Architecture
//!
//! - **LanguagePlugin trait**: Core trait that all language plugins must implement
//! - **PluginManager**: Manages plugin registration and lookup
//! - **Built-in plugins**: Statically linked plugins (Rust, JS, TS, Python, Go)
//! - **External plugins** (Phase 3+): Dynamically loaded plugins from disk
//!
//! # Example
//!
//! ```
//! use sqry_core::plugin::{PluginManager, LanguagePlugin};
//!
//! // Create manager with built-in plugins
//! let manager = PluginManager::new();
//!
//! // Lookup plugin by file extension
//! if let Some(plugin) = manager.plugin_for_extension("rs") {
//!     let metadata = plugin.metadata();
//!     println!("Found {} plugin v{}", metadata.name, metadata.version);
//! }
//! ```

pub mod error;
pub mod manager;
pub mod safe_parse;
pub mod types;

pub use error::{PluginError, PluginResult};
pub use manager::PluginManager;
pub use safe_parse::{CancellationFlag, SafeParser, SafeParserConfig};
pub use types::{LanguageMetadata, LanguagePlugin};
