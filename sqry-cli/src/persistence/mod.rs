//! Query persistence subsystem.
//!
//! This module provides persistence for user aliases and query history,
//! enabling users to save and reuse frequently used queries.
//!
//! # Architecture
//!
//! The persistence subsystem uses a binary index format (`postcard`) for
//! efficient storage and fast access. Data is stored in two locations:
//!
//! - **Global**: `~/.config/sqry/global.index.user` - cross-project aliases
//! - **Local**: `.sqry-index.user` in project root - project-specific aliases
//!
//! All file operations are atomic (using temp file + rename) to prevent
//! corruption from crashes or concurrent access.
//!
//! # Components
//!
//! - [`UserMetadataIndex`]: Main interface for loading/saving user metadata
//! - [`AliasManager`]: High-level API for managing saved query aliases
//! - [`HistoryManager`]: High-level API for managing query history
//! - [`PersistenceConfig`]: Configuration for paths and limits
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use sqry_cli::persistence::{
//!     UserMetadataIndex, PersistenceConfig, StorageScope, AliasManager
//! };
//!
//! // Open the index
//! let config = PersistenceConfig::default();
//! let index = Arc::new(UserMetadataIndex::open(Some("."), config)?);
//!
//! // Create an alias manager
//! let aliases = AliasManager::new(index.clone());
//! aliases.save("my-query", "search", &["main".into()], None, StorageScope::Global)?;
//!
//! // Retrieve the alias
//! let alias = aliases.get("my-query")?;
//! println!("Command: {} {:?}", alias.alias.command, alias.alias.args);
//! ```

mod alias;
mod config;
mod history;
mod index;
mod types;
mod validation;

// Re-export public API
// Allow unused_imports: These are exported for external consumers
// (shell integration, LSP, external tools)
#[allow(unused_imports)]
pub use alias::{AliasError, AliasManager};
#[allow(unused_imports)]
pub use config::{
    DEFAULT_MAX_HISTORY_ENTRIES, DEFAULT_MAX_INDEX_BYTES, ENV_CONFIG_DIR, ENV_NO_HISTORY,
    ENV_NO_REDACT, PersistenceConfig, global_config_dir,
};
#[allow(unused_imports)]
pub use history::{
    HistoryError, HistoryManager, HistoryStats, REDACTED_PLACEHOLDER, contains_secrets,
    parse_duration, redact_secrets,
};
#[allow(unused_imports)]
pub use index::{GLOBAL_INDEX_FILE, LOCAL_INDEX_FILE, UserMetadataIndex, open_shared_index};
#[allow(unused_imports)]
pub use types::{
    AliasExportFile, AliasWithScope, HistoryEntry, HistoryState, ImportConflictStrategy,
    ImportResult, SavedAlias, StorageScope, USER_METADATA_VERSION, UserMetadata,
};
#[allow(unused_imports)]
pub use validation::{
    AliasNameError, MAX_ALIAS_LENGTH, MIN_ALIAS_LENGTH, suggest_alias_name, validate_alias_name,
};
