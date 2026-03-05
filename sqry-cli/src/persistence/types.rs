//! Core types for query persistence.
//!
//! This module defines the data structures used for storing saved queries (aliases)
//! and query history. All types are designed for serialization with both postcard
//! (primary index storage) and `serde_json` (export/import).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Current version of the user metadata format.
/// Increment when making breaking changes to the schema.
pub const USER_METADATA_VERSION: u32 = 1;

/// Storage scope for aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageScope {
    /// Global storage (~/.config/sqry/global.index.user)
    Global,
    /// Project-local storage (.sqry-index.user in project root)
    Local,
}

impl std::fmt::Display for StorageScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageScope::Global => write!(f, "global"),
            StorageScope::Local => write!(f, "local"),
        }
    }
}

/// A saved query alias.
///
/// Aliases allow users to save frequently used queries for quick reuse.
/// Each alias maps a short name to a full command with arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAlias {
    /// The base command (e.g., "search", "query")
    pub command: String,

    /// Command arguments (e.g., `["kind:function", "--lang", "rust"]`)
    pub args: Vec<String>,

    /// When this alias was created
    pub created: DateTime<Utc>,

    /// Optional description for the alias
    pub description: Option<String>,
}

/// A single entry in the query history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unique ID assigned when the entry is recorded
    pub id: u64,

    /// When the query was executed
    pub timestamp: DateTime<Utc>,

    /// The command that was run (e.g., "search", "query")
    pub command: String,

    /// Command arguments
    pub args: Vec<String>,

    /// Working directory when the query was executed
    pub working_dir: PathBuf,

    /// Whether the query succeeded
    pub success: bool,

    /// Query execution time in milliseconds
    pub duration_ms: Option<u64>,
}

/// History state stored in the user metadata index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryState {
    /// History entries (most recent last for efficient appending)
    pub entries: Vec<HistoryEntry>,

    /// Next ID to assign
    pub next_id: u64,
}

/// Root structure for user metadata stored in the index.
///
/// This is the top-level structure that gets serialized to `.sqry-index.user`
/// (local) or `global.index.user` (global).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMetadata {
    /// Schema version for forward compatibility
    pub version: u32,

    /// Saved query aliases by name
    pub aliases: HashMap<String, SavedAlias>,

    /// Query history
    pub history: HistoryState,
}

impl Default for UserMetadata {
    fn default() -> Self {
        Self {
            version: USER_METADATA_VERSION,
            aliases: HashMap::new(),
            history: HistoryState::default(),
        }
    }
}

/// JSON export format for aliases (for backup/restore via ).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasExportFile {
    /// Export format version
    pub version: u32,

    /// When this export was created
    pub exported_at: DateTime<Utc>,

    /// The exported aliases
    pub aliases: HashMap<String, SavedAlias>,
}

impl AliasExportFile {
    /// Create an export file from a list of aliases.
    #[must_use]
    pub fn from_aliases(aliases: &[AliasWithScope]) -> Self {
        let mut map = HashMap::new();
        for aws in aliases {
            map.insert(aws.name.clone(), aws.alias.clone());
        }
        Self {
            version: 1,
            exported_at: Utc::now(),
            aliases: map,
        }
    }
}

/// Import conflict resolution strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportConflictStrategy {
    /// Skip aliases that already exist
    Skip,
    /// Overwrite existing aliases
    Overwrite,
    /// Fail if any conflict is detected
    Fail,
}

/// Result of an import operation.
#[derive(Debug, Clone)]
pub struct ImportResult {
    /// Number of aliases successfully imported
    pub imported: usize,
    /// Number of aliases skipped due to conflicts
    pub skipped: usize,
    /// Number of aliases that failed to import
    pub failed: usize,
    /// Number of aliases overwritten
    pub overwritten: usize,
    /// Names of aliases that were skipped
    pub skipped_names: Vec<String>,
}

/// Alias with its storage scope for listing.
#[derive(Debug, Clone)]
pub struct AliasWithScope {
    /// The alias name
    pub name: String,
    /// The alias data
    pub alias: SavedAlias,
    /// Where the alias is stored
    pub scope: StorageScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saved_alias_serialization() {
        let alias = SavedAlias {
            command: "query".to_string(),
            args: vec![
                "kind:function".to_string(),
                "--lang".to_string(),
                "rust".to_string(),
            ],
            created: Utc::now(),
            description: Some("Find Rust functions".to_string()),
        };

        // Test postcard serialization
        let bytes = postcard::to_allocvec(&alias).expect("postcard serialize");
        let deserialized: SavedAlias = postcard::from_bytes(&bytes).expect("postcard deserialize");

        assert_eq!(alias.command, deserialized.command);
        assert_eq!(alias.args, deserialized.args);
        assert_eq!(alias.description, deserialized.description);
    }

    #[test]
    fn test_history_entry_serialization() {
        let entry = HistoryEntry {
            id: 42,
            timestamp: Utc::now(),
            command: "search".to_string(),
            args: vec!["main".to_string()],
            working_dir: PathBuf::from("/home/user/project"),
            success: true,
            duration_ms: Some(150),
        };

        let bytes = postcard::to_allocvec(&entry).expect("postcard serialize");
        let deserialized: HistoryEntry =
            postcard::from_bytes(&bytes).expect("postcard deserialize");

        assert_eq!(entry.id, deserialized.id);
        assert_eq!(entry.command, deserialized.command);
        assert_eq!(entry.args, deserialized.args);
        assert_eq!(entry.success, deserialized.success);
    }

    #[test]
    fn test_user_metadata_default() {
        let metadata = UserMetadata::default();

        assert_eq!(metadata.version, USER_METADATA_VERSION);
        assert!(metadata.aliases.is_empty());
        assert!(metadata.history.entries.is_empty());
        assert_eq!(metadata.history.next_id, 0);
    }

    #[test]
    fn test_user_metadata_serialization() {
        let mut metadata = UserMetadata::default();
        metadata.aliases.insert(
            "test".to_string(),
            SavedAlias {
                command: "search".to_string(),
                args: vec!["pattern".to_string()],
                created: Utc::now(),
                description: None,
            },
        );
        metadata.history.entries.push(HistoryEntry {
            id: 1,
            timestamp: Utc::now(),
            command: "query".to_string(),
            args: vec!["kind:function".to_string()],
            working_dir: PathBuf::from("/tmp"),
            success: true,
            duration_ms: Some(50),
        });
        metadata.history.next_id = 2;

        let bytes = postcard::to_allocvec(&metadata).expect("postcard serialize");
        let deserialized: UserMetadata =
            postcard::from_bytes(&bytes).expect("postcard deserialize");

        assert_eq!(deserialized.version, USER_METADATA_VERSION);
        assert!(deserialized.aliases.contains_key("test"));
        assert_eq!(deserialized.history.entries.len(), 1);
        assert_eq!(deserialized.history.next_id, 2);
    }

    #[test]
    fn test_storage_scope_display() {
        assert_eq!(format!("{}", StorageScope::Global), "global");
        assert_eq!(format!("{}", StorageScope::Local), "local");
    }

    #[test]
    fn test_alias_export_file_json() {
        let mut aliases = HashMap::new();
        aliases.insert(
            "test".to_string(),
            SavedAlias {
                command: "search".to_string(),
                args: vec!["main".to_string()],
                created: Utc::now(),
                description: None,
            },
        );

        let export = AliasExportFile {
            version: 1,
            exported_at: Utc::now(),
            aliases,
        };

        let json = serde_json::to_string(&export).expect("json serialize");
        let deserialized: AliasExportFile = serde_json::from_str(&json).expect("json deserialize");

        assert_eq!(deserialized.version, 1);
        assert!(deserialized.aliases.contains_key("test"));
    }
}
