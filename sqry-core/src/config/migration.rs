//! Configuration migration from legacy `.sqry-config.toml` to unified graph config.
//!
//! Implements Step 6 of the Unified Graph Config Partition feature:
//! - Detects legacy `.sqry-config.toml` files
//! - Migrates settings to `.sqry/graph/config/config.json`
//! - Preserves user settings during migration
//! - Logs deprecation warnings when legacy config is used
//!
//! # Migration Strategy
//!
//! When a legacy config file is detected:
//! 1. Load the legacy `.sqry-config.toml`
//! 2. Convert to new `GraphConfigFile` format
//! 3. Initialize new config store at `.sqry/graph/config/`
//! 4. Save converted config atomically
//! 5. Leave legacy file in place (user can remove manually)
//! 6. Log migration summary
//!
//! # Design
//!
//! See: `docs/development/unified-graph-config-partition/02_DESIGN.md`

use std::path::{Path, PathBuf};
use thiserror::Error;

use super::ConfigPersistence;
use super::graph_config_schema::GraphConfigFile;
use super::graph_config_store::GraphConfigStore;
use super::project_config::{CONFIG_FILE_NAME, ProjectConfig};

/// Errors that can occur during migration
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Failed to load legacy config
    #[error("Failed to load legacy config from {path}: {source}")]
    LegacyLoadError {
        /// Path to the legacy config file
        path: PathBuf,
        /// Underlying config error
        #[source]
        source: super::project_config::ConfigError,
    },

    /// Failed to initialize new config store
    #[error("Failed to initialize new config store: {0}")]
    StoreInitError(String),

    /// Failed to save migrated config
    #[error("Failed to save migrated config: {0}")]
    SaveError(String),

    /// IO error
    #[error("IO error at {path}: {source}")]
    IoError {
        /// Path where IO error occurred
        path: PathBuf,
        /// Underlying IO error
        #[source]
        source: std::io::Error,
    },
}

/// Result type for migration operations
pub type MigrationResult<T> = Result<T, MigrationError>;

/// Result of a migration attempt
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// Whether migration was performed
    pub migrated: bool,

    /// Path to legacy config file (if found)
    pub legacy_path: Option<PathBuf>,

    /// Path to new config file (if created)
    pub new_path: Option<PathBuf>,

    /// Warnings generated during migration
    pub warnings: Vec<String>,

    /// Summary of settings migrated
    pub migrated_settings: Vec<String>,
}

impl MigrationReport {
    /// Create a report for when no migration was needed
    #[must_use]
    pub fn no_migration_needed() -> Self {
        Self {
            migrated: false,
            legacy_path: None,
            new_path: None,
            warnings: Vec::new(),
            migrated_settings: Vec::new(),
        }
    }

    /// Add a warning message
    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    /// Add a migrated setting
    pub fn add_migrated_setting(&mut self, setting: impl Into<String>) {
        self.migrated_settings.push(setting.into());
    }
}

/// Detect if a legacy config file exists in the project root or its ancestors
///
/// Searches from `project_root` upward through parent directories for `.sqry-config.toml`.
///
/// # Arguments
///
/// * `project_root` - Starting directory for config search
///
/// # Returns
///
/// Returns `Some(path)` if legacy config found, `None` otherwise.
pub fn detect_legacy_config<P: AsRef<Path>>(project_root: P) -> Option<PathBuf> {
    let mut current = project_root.as_ref();

    loop {
        let config_path = current.join(CONFIG_FILE_NAME);

        if config_path.exists() && config_path.is_file() {
            return Some(config_path);
        }

        // Move to parent directory
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                current = parent;
            }
            _ => break, // Reached filesystem root
        }
    }

    None
}

/// Check if new config system is already initialized
///
/// Returns `true` if `.sqry/graph/config/config.json` exists.
pub fn is_new_config_initialized<P: AsRef<Path>>(project_root: P) -> bool {
    let Ok(store) = GraphConfigStore::new(project_root) else {
        return false;
    };

    store.paths().config_file_exists()
}

/// Migrate legacy config to new format
///
/// Detects and migrates legacy `.sqry-config.toml` if present.
/// Returns a report of migration actions taken.
///
/// # Arguments
///
/// * `project_root` - Project root directory
///
/// # Returns
///
/// Returns a migration report with status and warnings.
///
/// # Errors
///
/// Returns error if migration fails (legacy config corrupt, write fails, etc.)
pub fn migrate_legacy_config<P: AsRef<Path>>(project_root: P) -> MigrationResult<MigrationReport> {
    let project_root = project_root.as_ref();
    let mut report = MigrationReport::default();

    // Check if new config already exists
    if is_new_config_initialized(project_root) {
        log::debug!(
            "New config already initialized at {}, skipping migration",
            project_root.display()
        );
        return Ok(MigrationReport::no_migration_needed());
    }

    // Detect legacy config
    let Some(legacy_path) = detect_legacy_config(project_root) else {
        log::debug!(
            "No legacy config found in {}, skipping migration",
            project_root.display()
        );
        return Ok(MigrationReport::no_migration_needed());
    };
    log::info!(
        "Migrating legacy config from {} to new format",
        legacy_path.display()
    );

    report.legacy_path = Some(legacy_path.clone());

    // Load legacy config
    let legacy_config =
        ProjectConfig::load(&legacy_path).map_err(|e| MigrationError::LegacyLoadError {
            path: legacy_path.clone(),
            source: e,
        })?;

    // Convert to new format
    let new_config = convert_project_config_to_graph_config(&legacy_config, &mut report);

    // Initialize new config store
    let store = GraphConfigStore::new(project_root).map_err(|e| {
        MigrationError::StoreInitError(format!("Failed to create config store: {e}"))
    })?;

    // Ensure config directory exists
    let config_dir = store.paths().config_dir();
    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir).map_err(|e| MigrationError::IoError {
            path: config_dir.clone(),
            source: e,
        })?;
    }

    // Save migrated config
    let persistence = ConfigPersistence::from_paths(store.paths().clone());
    let mut new_config_mut = new_config;
    persistence
        .save(&mut new_config_mut, 5000, "migration")
        .map_err(|e| MigrationError::SaveError(format!("Failed to save config: {e}")))?;

    report.new_path = Some(store.paths().config_file());
    report.migrated = true;

    // Log deprecation warning
    report.add_warning(format!(
        "Legacy config file detected at {}. Migrated to {}. \
         Consider removing the legacy file after verifying the migration.",
        legacy_path.display(),
        store.paths().config_file().display()
    ));

    log::warn!(
        "Migrated legacy config from {} to {}. {} settings migrated.",
        legacy_path.display(),
        store.paths().config_file().display(),
        report.migrated_settings.len()
    );

    Ok(report)
}

/// Convert legacy `ProjectConfig` to new `GraphConfigFile`
///
/// Maps legacy settings to new config structure, tracking what was migrated.
fn convert_project_config_to_graph_config(
    legacy: &ProjectConfig,
    report: &mut MigrationReport,
) -> GraphConfigFile {
    let mut config_file = GraphConfigFile::default();
    let config = &mut config_file.config;

    // Migrate indexing settings directly (GraphConfig has indexing field from ProjectConfig)
    config.indexing = legacy.indexing.clone();
    report.add_migrated_setting(format!(
        "indexing.max_file_size = {}",
        legacy.indexing.max_file_size
    ));
    report.add_migrated_setting(format!(
        "indexing.max_depth = {}",
        legacy.indexing.max_depth
    ));
    report.add_migrated_setting(format!(
        "indexing.enable_scope_extraction = {}",
        legacy.indexing.enable_scope_extraction
    ));
    report.add_migrated_setting(format!(
        "indexing.enable_relation_extraction = {}",
        legacy.indexing.enable_relation_extraction
    ));

    // Migrate ignore patterns
    config.ignore = legacy.ignore.clone();
    report.add_migrated_setting(format!(
        "ignore.patterns = [{} patterns]",
        legacy.ignore.patterns.len()
    ));

    // Migrate include patterns
    config.include = legacy.include.clone();
    report.add_migrated_setting(format!(
        "include.patterns = [{} patterns]",
        legacy.include.patterns.len()
    ));

    // Migrate language mappings
    config.languages = legacy.languages.clone();
    report.add_migrated_setting(format!(
        "languages.extensions = [{} mappings]",
        legacy.languages.extensions.len()
    ));
    report.add_migrated_setting(format!(
        "languages.files = [{} mappings]",
        legacy.languages.files.len()
    ));

    // Migrate cache settings
    config.cache = legacy.cache.clone();
    report.add_migrated_setting(format!("cache.directory = {}", legacy.cache.directory));
    report.add_migrated_setting(format!("cache.persistent = {}", legacy.cache.persistent));

    // Note: Buffer sizes and other runtime settings remain environment-variable controlled
    // and are not migrated from the legacy config

    config_file
}

/// Log a deprecation warning if legacy config is detected but not migrated
///
/// This is used when the system detects a legacy config but the new config
/// already exists (manual migration or previous auto-migration).
pub fn log_deprecation_warning_if_legacy_exists<P: AsRef<Path>>(project_root: P) {
    let project_root = project_root.as_ref();

    // Only warn if legacy exists AND new config exists (coexistence)
    if let Some(legacy_path) = detect_legacy_config(project_root)
        && is_new_config_initialized(project_root)
    {
        log::warn!(
            "Legacy config file detected at {}. The new config system is active. \
                 Consider removing the legacy file to avoid confusion.",
            legacy_path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_legacy_config_not_found() {
        let temp = TempDir::new().unwrap();
        let result = detect_legacy_config(temp.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_legacy_config_found_in_root() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join(CONFIG_FILE_NAME);
        std::fs::write(&config_path, "[indexing]\nmax_depth = 50").unwrap();

        let result = detect_legacy_config(temp.path());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), config_path);
    }

    #[test]
    fn test_detect_legacy_config_found_in_ancestor() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join(CONFIG_FILE_NAME);
        std::fs::write(&config_path, "[indexing]\nmax_depth = 50").unwrap();

        // Create nested directory
        let nested = temp.path().join("nested/deep");
        std::fs::create_dir_all(&nested).unwrap();

        // Should find config in ancestor
        let result = detect_legacy_config(&nested);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), config_path);
    }

    #[test]
    fn test_is_new_config_initialized_false() {
        let temp = TempDir::new().unwrap();
        assert!(!is_new_config_initialized(temp.path()));
    }

    #[test]
    fn test_is_new_config_initialized_true() {
        let temp = TempDir::new().unwrap();

        // Create .sqry/graph/config directory
        let config_dir = temp.path().join(".sqry/graph/config");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Create config.json
        let config_file = config_dir.join("config.json");
        let default_config = GraphConfigFile::default();
        let json = serde_json::to_string_pretty(&default_config).unwrap();
        std::fs::write(&config_file, json).unwrap();

        assert!(is_new_config_initialized(temp.path()));
    }

    #[test]
    fn test_convert_project_config_to_graph_config() {
        let mut legacy = ProjectConfig::default();
        legacy.indexing.max_file_size = 20_971_520; // 20 MB
        legacy.indexing.max_depth = 75;
        legacy.indexing.enable_scope_extraction = false;
        legacy.cache.directory = ".custom-cache".to_string();
        legacy.cache.persistent = false;

        let mut report = MigrationReport::default();
        let new_config = convert_project_config_to_graph_config(&legacy, &mut report);

        assert_eq!(new_config.config.indexing.max_file_size, 20_971_520);
        assert_eq!(new_config.config.indexing.max_depth, 75);
        assert!(!new_config.config.indexing.enable_scope_extraction);
        assert_eq!(new_config.config.cache.directory, ".custom-cache");
        assert!(!new_config.config.cache.persistent);

        // Should have recorded migrations
        assert!(!report.migrated_settings.is_empty());
        assert!(
            report
                .migrated_settings
                .iter()
                .any(|s| s.contains("max_file_size"))
        );
    }

    #[test]
    fn test_migrate_legacy_config_no_legacy_found() {
        let temp = TempDir::new().unwrap();
        let result = migrate_legacy_config(temp.path()).unwrap();

        assert!(!result.migrated);
        assert!(result.legacy_path.is_none());
        assert!(result.new_path.is_none());
    }

    #[test]
    fn test_migrate_legacy_config_success() {
        let temp = TempDir::new().unwrap();

        // Create legacy config
        let legacy_path = temp.path().join(CONFIG_FILE_NAME);
        std::fs::write(
            &legacy_path,
            r#"
[indexing]
max_file_size = 15728640
max_depth = 60

[cache]
directory = ".my-cache"
persistent = true
"#,
        )
        .unwrap();

        // Run migration
        let result = migrate_legacy_config(temp.path()).unwrap();

        assert!(result.migrated);
        assert_eq!(result.legacy_path, Some(legacy_path.clone()));
        assert!(result.new_path.is_some());
        assert!(!result.warnings.is_empty());
        assert!(!result.migrated_settings.is_empty());

        // Verify new config was created
        let new_config_path = result.new_path.unwrap();
        assert!(new_config_path.exists());

        // Verify migrated values
        let content = std::fs::read_to_string(&new_config_path).unwrap();
        let loaded: GraphConfigFile = serde_json::from_str(&content).unwrap();

        assert_eq!(loaded.config.indexing.max_file_size, 15_728_640);
        assert_eq!(loaded.config.indexing.max_depth, 60);
        assert_eq!(loaded.config.cache.directory, ".my-cache");
        assert!(loaded.config.cache.persistent);
    }

    #[test]
    fn test_migrate_legacy_config_skips_if_new_exists() {
        let temp = TempDir::new().unwrap();

        // Create legacy config
        let legacy_path = temp.path().join(CONFIG_FILE_NAME);
        std::fs::write(&legacy_path, "[indexing]\nmax_depth = 50").unwrap();

        // Create new config
        let config_dir = temp.path().join(".sqry/graph/config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let new_config_path = config_dir.join("config.json");
        let default_config = GraphConfigFile::default();
        std::fs::write(
            &new_config_path,
            serde_json::to_string_pretty(&default_config).unwrap(),
        )
        .unwrap();

        // Migration should skip
        let result = migrate_legacy_config(temp.path()).unwrap();

        assert!(!result.migrated);
    }

    #[test]
    fn test_migration_report_default() {
        let report = MigrationReport::default();
        assert!(!report.migrated);
        assert!(report.legacy_path.is_none());
        assert!(report.new_path.is_none());
        assert!(report.warnings.is_empty());
        assert!(report.migrated_settings.is_empty());
    }

    #[test]
    fn test_migration_report_add_warning() {
        let mut report = MigrationReport::default();
        report.add_warning("Test warning");
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0], "Test warning");
    }

    #[test]
    fn test_migration_report_add_migrated_setting() {
        let mut report = MigrationReport::default();
        report.add_migrated_setting("setting.key = value");
        assert_eq!(report.migrated_settings.len(), 1);
        assert_eq!(report.migrated_settings[0], "setting.key = value");
    }

    #[test]
    fn test_convert_preserves_ignore_patterns() {
        let mut legacy = ProjectConfig::default();
        legacy.ignore.patterns = vec!["custom/**".to_string(), "*.tmp".to_string()];

        let mut report = MigrationReport::default();
        let new_config = convert_project_config_to_graph_config(&legacy, &mut report);

        assert_eq!(
            new_config.config.ignore.patterns,
            vec!["custom/**".to_string(), "*.tmp".to_string()]
        );
    }

    #[test]
    fn test_convert_preserves_language_mappings() {
        let mut legacy = ProjectConfig::default();
        legacy
            .languages
            .extensions
            .insert("jsx".to_string(), "javascript".to_string());
        legacy
            .languages
            .files
            .insert("Jenkinsfile".to_string(), "groovy".to_string());

        let mut report = MigrationReport::default();
        let new_config = convert_project_config_to_graph_config(&legacy, &mut report);

        assert_eq!(
            new_config
                .config
                .languages
                .extensions
                .get("jsx")
                .map(String::as_str),
            Some("javascript")
        );
        assert_eq!(
            new_config
                .config
                .languages
                .files
                .get("Jenkinsfile")
                .map(String::as_str),
            Some("groovy")
        );
    }
}
