//! Alias management for saved queries.
//!
//! The `AliasManager` provides a high-level API for creating, retrieving,
//! updating, and deleting saved query aliases.

use std::sync::Arc;

use chrono::Utc;

use crate::persistence::index::UserMetadataIndex;
use crate::persistence::types::{
    AliasExportFile, AliasWithScope, ImportConflictStrategy, ImportResult, SavedAlias,
    StorageScope, UserMetadata,
};
use crate::persistence::validation::{AliasNameError, validate_alias_name};

/// Error type for alias operations.
#[derive(Debug)]
pub enum AliasError {
    /// Alias name validation failed.
    InvalidName(AliasNameError),
    /// Alias not found.
    NotFound { name: String },
    /// Alias already exists.
    AlreadyExists { name: String, scope: StorageScope },
    /// Storage operation failed.
    Storage(anyhow::Error),
}

impl std::fmt::Display for AliasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(e) => write!(f, "invalid alias name: {e}"),
            Self::NotFound { name } => write!(f, "alias '{name}' not found"),
            Self::AlreadyExists { name, scope } => {
                write!(f, "alias '{name}' already exists in {scope} storage")
            }
            Self::Storage(e) => write!(f, "storage error: {e}"),
        }
    }
}

impl std::error::Error for AliasError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidName(e) => Some(e),
            Self::Storage(e) => e.source(),
            _ => None,
        }
    }
}

impl From<AliasNameError> for AliasError {
    fn from(e: AliasNameError) -> Self {
        Self::InvalidName(e)
    }
}

impl From<anyhow::Error> for AliasError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}

/// Manager for saved query aliases.
///
/// Provides CRUD operations for aliases stored in the user metadata index.
/// Local aliases take precedence over global aliases when resolving by name.
#[derive(Debug, Clone)]
pub struct AliasManager {
    index: Arc<UserMetadataIndex>,
}

impl AliasManager {
    /// Create a new alias manager.
    #[must_use]
    pub fn new(index: Arc<UserMetadataIndex>) -> Self {
        Self { index }
    }

    /// Save a new alias.
    ///
    /// # Arguments
    ///
    /// * `name` - The alias name (validated against naming rules)
    /// * `command` - The command to execute (e.g., "query", "search")
    /// * `args` - Command arguments
    /// * `description` - Optional description
    /// * `scope` - Where to store the alias (Global or Local)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The name is invalid
    /// - An alias with the same name already exists in the target scope
    /// - The storage operation fails
    pub fn save(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        description: Option<&str>,
        scope: StorageScope,
    ) -> Result<(), AliasError> {
        // Validate name
        validate_alias_name(name)?;

        // Check if alias already exists in target scope
        self.index
            .update(scope, |metadata| {
                if metadata.aliases.contains_key(name) {
                    anyhow::bail!("alias '{name}' already exists");
                }

                let alias = SavedAlias {
                    command: command.to_string(),
                    args: args.to_vec(),
                    created: Utc::now(),
                    description: description.map(String::from),
                };

                metadata.aliases.insert(name.to_string(), alias);
                Ok(())
            })
            .map_err(|e| {
                if e.to_string().contains("already exists") {
                    AliasError::AlreadyExists {
                        name: name.to_string(),
                        scope,
                    }
                } else {
                    AliasError::Storage(e)
                }
            })
    }

    /// Get an alias by name.
    ///
    /// Searches local storage first (if available), then global storage.
    /// Returns the alias and the scope it was found in.
    ///
    /// # Errors
    ///
    /// Returns an error if the alias is not found or storage fails.
    pub fn get(&self, name: &str) -> Result<AliasWithScope, AliasError> {
        // Check local first (if project root is set)
        if self.index.has_project_root() {
            let local = self.index.load(StorageScope::Local)?;
            if let Some(alias) = local.aliases.get(name) {
                return Ok(AliasWithScope {
                    name: name.to_string(),
                    alias: alias.clone(),
                    scope: StorageScope::Local,
                });
            }
        }

        // Check global
        let global = self.index.load(StorageScope::Global)?;
        if let Some(alias) = global.aliases.get(name) {
            return Ok(AliasWithScope {
                name: name.to_string(),
                alias: alias.clone(),
                scope: StorageScope::Global,
            });
        }

        Err(AliasError::NotFound {
            name: name.to_string(),
        })
    }

    /// Get an alias from a specific scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the alias is not found or storage fails.
    pub fn get_from_scope(
        &self,
        name: &str,
        scope: StorageScope,
    ) -> Result<SavedAlias, AliasError> {
        let metadata = self.index.load(scope)?;
        metadata
            .aliases
            .get(name)
            .cloned()
            .ok_or_else(|| AliasError::NotFound {
                name: name.to_string(),
            })
    }

    /// List all aliases.
    ///
    /// Returns aliases from both local and global storage.
    /// If an alias exists in both scopes, only the local version is returned
    /// (local takes precedence).
    ///
    /// # Errors
    ///
    /// Returns an error if storage fails.
    pub fn list(&self) -> Result<Vec<AliasWithScope>, AliasError> {
        let mut result = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // Load local aliases first (they take precedence)
        if self.index.has_project_root() {
            let local = self.index.load(StorageScope::Local)?;
            for (name, alias) in local.aliases {
                seen_names.insert(name.clone());
                result.push(AliasWithScope {
                    name,
                    alias,
                    scope: StorageScope::Local,
                });
            }
        }

        // Load global aliases (skip those already in local)
        let global = self.index.load(StorageScope::Global)?;
        for (name, alias) in global.aliases {
            if !seen_names.contains(&name) {
                result.push(AliasWithScope {
                    name,
                    alias,
                    scope: StorageScope::Global,
                });
            }
        }

        // Sort by name for consistent output
        result.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(result)
    }

    /// List aliases from a specific scope only.
    ///
    /// # Errors
    ///
    /// Returns an error if storage fails.
    pub fn list_scope(&self, scope: StorageScope) -> Result<Vec<AliasWithScope>, AliasError> {
        let metadata = self.index.load(scope)?;
        let mut result: Vec<AliasWithScope> = metadata
            .aliases
            .into_iter()
            .map(|(name, alias)| AliasWithScope { name, alias, scope })
            .collect();

        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    /// Delete an alias.
    ///
    /// If no scope is specified, deletes from both scopes.
    /// If a scope is specified, only deletes from that scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the alias is not found or storage fails.
    pub fn delete(&self, name: &str, scope: Option<StorageScope>) -> Result<(), AliasError> {
        let mut deleted = false;

        if let Some(s) = scope {
            // Delete from specific scope
            self.delete_in_scope(s, name, &mut deleted)?;
        } else {
            // Delete from both scopes
            if self.index.has_project_root() {
                self.delete_in_scope(StorageScope::Local, name, &mut deleted)?;
            }

            self.delete_in_scope(StorageScope::Global, name, &mut deleted)?;
        }

        if deleted {
            Ok(())
        } else {
            Err(AliasError::NotFound {
                name: name.to_string(),
            })
        }
    }

    fn delete_in_scope(
        &self,
        scope: StorageScope,
        name: &str,
        deleted: &mut bool,
    ) -> Result<(), AliasError> {
        self.index.update(scope, |metadata| {
            if metadata.aliases.remove(name).is_some() {
                *deleted = true;
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Rename an alias.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The old alias doesn't exist
    /// - The new name is invalid
    /// - An alias with the new name already exists
    /// - Storage fails
    pub fn rename(
        &self,
        old_name: &str,
        new_name: &str,
        scope: Option<StorageScope>,
    ) -> Result<StorageScope, AliasError> {
        // Validate new name
        validate_alias_name(new_name)?;

        let found_scope = self.resolve_alias_scope(old_name, scope)?;

        self.perform_rename(found_scope, old_name, new_name)?;

        Ok(found_scope)
    }

    fn resolve_alias_scope(
        &self,
        old_name: &str,
        scope: Option<StorageScope>,
    ) -> Result<StorageScope, AliasError> {
        if let Some(s) = scope {
            let metadata = self.index.load(s)?;
            if metadata.aliases.contains_key(old_name) {
                return Ok(s);
            }

            return Err(AliasError::NotFound {
                name: old_name.to_string(),
            });
        }

        let mut found = None;
        if self.index.has_project_root() {
            let local = self.index.load(StorageScope::Local)?;
            if local.aliases.contains_key(old_name) {
                found = Some(StorageScope::Local);
            }
        }
        if found.is_none() {
            let global = self.index.load(StorageScope::Global)?;
            if global.aliases.contains_key(old_name) {
                found = Some(StorageScope::Global);
            }
        }

        found.ok_or_else(|| AliasError::NotFound {
            name: old_name.to_string(),
        })
    }

    fn perform_rename(
        &self,
        found_scope: StorageScope,
        old_name: &str,
        new_name: &str,
    ) -> Result<(), AliasError> {
        self.index
            .update(found_scope, |metadata| {
                // Check new name doesn't already exist
                if metadata.aliases.contains_key(new_name) {
                    anyhow::bail!("alias '{new_name}' already exists");
                }

                // Remove old and insert new
                if let Some(alias) = metadata.aliases.remove(old_name) {
                    metadata.aliases.insert(new_name.to_string(), alias);
                }
                Ok(())
            })
            .map_err(|e| {
                if e.to_string().contains("already exists") {
                    AliasError::AlreadyExists {
                        name: new_name.to_string(),
                        scope: found_scope,
                    }
                } else {
                    AliasError::Storage(e)
                }
            })?;

        Ok(())
    }

    fn ensure_no_conflicts(
        &self,
        export: &AliasExportFile,
        scope: StorageScope,
    ) -> Result<(), AliasError> {
        let existing = self.index.load(scope)?;
        for name in export.aliases.keys() {
            if existing.aliases.contains_key(name) {
                return Err(AliasError::AlreadyExists {
                    name: name.clone(),
                    scope,
                });
            }
        }
        Ok(())
    }

    fn apply_import_entry(
        metadata: &mut UserMetadata,
        name: &str,
        alias: &SavedAlias,
        strategy: ImportConflictStrategy,
        result: &mut ImportResult,
    ) {
        if metadata.aliases.contains_key(name) {
            match strategy {
                ImportConflictStrategy::Skip => {
                    result.skipped += 1;
                    result.skipped_names.push(name.to_string());
                }
                ImportConflictStrategy::Overwrite => {
                    metadata.aliases.insert(name.to_string(), alias.clone());
                    result.overwritten += 1;
                }
                ImportConflictStrategy::Fail => {
                    // Should not reach here due to first pass check
                    unreachable!();
                }
            }
        } else {
            metadata.aliases.insert(name.to_string(), alias.clone());
            result.imported += 1;
        }
    }

    /// Check if an alias exists.
    ///
    /// Checks both local and global storage.
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.get(name).is_ok()
    }

    /// Get the count of aliases in each scope.
    ///
    /// # Errors
    ///
    /// Returns an error if storage fails.
    pub fn count(&self) -> Result<(usize, usize), AliasError> {
        let local_count = if self.index.has_project_root() {
            self.index.load(StorageScope::Local)?.aliases.len()
        } else {
            0
        };
        let global_count = self.index.load(StorageScope::Global)?.aliases.len();
        Ok((local_count, global_count))
    }

    /// Import aliases from an export file.
    ///
    /// # Errors
    ///
    /// Returns an error if storage fails or conflict strategy is Fail and conflicts exist.
    pub fn import(
        &self,
        export: &AliasExportFile,
        scope: StorageScope,
        strategy: ImportConflictStrategy,
    ) -> Result<ImportResult, AliasError> {
        let mut result = ImportResult {
            imported: 0,
            skipped: 0,
            failed: 0,
            overwritten: 0,
            skipped_names: Vec::new(),
        };

        // First pass: check for conflicts if strategy is Fail
        if strategy == ImportConflictStrategy::Fail {
            self.ensure_no_conflicts(export, scope)?;
        }

        // Import each alias
        self.index.update(scope, |metadata| {
            for (name, alias) in &export.aliases {
                Self::apply_import_entry(metadata, name, alias, strategy, &mut result);
            }
            Ok(())
        })?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::config::PersistenceConfig;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<UserMetadataIndex>) {
        let dir = TempDir::new().unwrap();
        let config = PersistenceConfig {
            global_dir_override: Some(dir.path().join("global")),
            ..Default::default()
        };
        let index = Arc::new(UserMetadataIndex::open(Some(dir.path()), config).unwrap());
        (dir, index)
    }

    #[test]
    fn test_save_and_get_alias() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save(
                "test-query",
                "search",
                &[
                    "main".to_string(),
                    "--kind".to_string(),
                    "function".to_string(),
                ],
                Some("Find main functions"),
                StorageScope::Global,
            )
            .unwrap();

        let alias = manager.get("test-query").unwrap();
        assert_eq!(alias.name, "test-query");
        assert_eq!(alias.alias.command, "search");
        assert_eq!(alias.alias.args, vec!["main", "--kind", "function"]);
        assert_eq!(
            alias.alias.description,
            Some("Find main functions".to_string())
        );
        assert_eq!(alias.scope, StorageScope::Global);
    }

    #[test]
    fn test_local_takes_precedence() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        // Save to global
        manager
            .save(
                "shared",
                "search",
                &["global".to_string()],
                None,
                StorageScope::Global,
            )
            .unwrap();

        // Save same name to local
        manager
            .save(
                "shared",
                "query",
                &["local".to_string()],
                None,
                StorageScope::Local,
            )
            .unwrap();

        // Get should return local version
        let alias = manager.get("shared").unwrap();
        assert_eq!(alias.alias.command, "query");
        assert_eq!(alias.alias.args, vec!["local"]);
        assert_eq!(alias.scope, StorageScope::Local);
    }

    #[test]
    fn test_list_aliases() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save("alpha", "search", &[], None, StorageScope::Global)
            .unwrap();
        manager
            .save("beta", "query", &[], None, StorageScope::Local)
            .unwrap();
        manager
            .save("gamma", "search", &[], None, StorageScope::Global)
            .unwrap();

        let list = manager.list().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[1].name, "beta");
        assert_eq!(list[2].name, "gamma");
    }

    #[test]
    fn test_delete_alias() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save("to-delete", "search", &[], None, StorageScope::Global)
            .unwrap();

        assert!(manager.exists("to-delete"));

        manager.delete("to-delete", None).unwrap();

        assert!(!manager.exists("to-delete"));
    }

    #[test]
    fn test_delete_from_specific_scope() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        // Save to both scopes
        manager
            .save(
                "shared",
                "search",
                &["global".to_string()],
                None,
                StorageScope::Global,
            )
            .unwrap();
        manager
            .save(
                "shared",
                "query",
                &["local".to_string()],
                None,
                StorageScope::Local,
            )
            .unwrap();

        // Delete only from local
        manager.delete("shared", Some(StorageScope::Local)).unwrap();

        // Should still exist in global
        let alias = manager.get("shared").unwrap();
        assert_eq!(alias.scope, StorageScope::Global);
        assert_eq!(alias.alias.args, vec!["global"]);
    }

    #[test]
    fn test_rename_alias() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save(
                "old-name",
                "search",
                &["test".to_string()],
                None,
                StorageScope::Global,
            )
            .unwrap();

        let scope = manager.rename("old-name", "new-name", None).unwrap();
        assert_eq!(scope, StorageScope::Global);

        assert!(!manager.exists("old-name"));
        assert!(manager.exists("new-name"));

        let alias = manager.get("new-name").unwrap();
        assert_eq!(alias.alias.args, vec!["test"]);
    }

    #[test]
    fn test_rename_to_existing_fails() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save("first", "search", &[], None, StorageScope::Global)
            .unwrap();
        manager
            .save("second", "query", &[], None, StorageScope::Global)
            .unwrap();

        let result = manager.rename("first", "second", None);
        assert!(matches!(result, Err(AliasError::AlreadyExists { .. })));
    }

    #[test]
    fn test_save_invalid_name_fails() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        let result = manager.save("123invalid", "search", &[], None, StorageScope::Global);
        assert!(matches!(result, Err(AliasError::InvalidName(_))));
    }

    #[test]
    fn test_get_nonexistent_fails() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        let result = manager.get("nonexistent");
        assert!(matches!(result, Err(AliasError::NotFound { .. })));
    }

    #[test]
    fn test_duplicate_save_fails() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save("unique", "search", &[], None, StorageScope::Global)
            .unwrap();

        let result = manager.save("unique", "query", &[], None, StorageScope::Global);
        assert!(matches!(result, Err(AliasError::AlreadyExists { .. })));
    }

    #[test]
    fn test_count() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        assert_eq!(manager.count().unwrap(), (0, 0));

        manager
            .save("global1", "search", &[], None, StorageScope::Global)
            .unwrap();
        manager
            .save("global2", "search", &[], None, StorageScope::Global)
            .unwrap();
        manager
            .save("local1", "search", &[], None, StorageScope::Local)
            .unwrap();

        assert_eq!(manager.count().unwrap(), (1, 2));
    }

    #[test]
    fn test_list_scope() {
        let (_dir, index) = setup();
        let manager = AliasManager::new(index);

        manager
            .save("global1", "search", &[], None, StorageScope::Global)
            .unwrap();
        manager
            .save("local1", "query", &[], None, StorageScope::Local)
            .unwrap();

        let global_list = manager.list_scope(StorageScope::Global).unwrap();
        assert_eq!(global_list.len(), 1);
        assert_eq!(global_list[0].name, "global1");

        let local_list = manager.list_scope(StorageScope::Local).unwrap();
        assert_eq!(local_list.len(), 1);
        assert_eq!(local_list[0].name, "local1");
    }

    #[test]
    fn test_error_display() {
        let err = AliasError::NotFound {
            name: "test".to_string(),
        };
        assert_eq!(err.to_string(), "alias 'test' not found");

        let err = AliasError::AlreadyExists {
            name: "test".to_string(),
            scope: StorageScope::Global,
        };
        assert_eq!(
            err.to_string(),
            "alias 'test' already exists in global storage"
        );
    }
}
