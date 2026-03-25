//! Configuration for query persistence.
//!
//! Handles path resolution, environment variable overrides, and default settings
//! for the persistence subsystem.

use std::path::PathBuf;

/// Default maximum number of history entries to retain.
pub const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;

/// Default maximum size of the user metadata index in bytes (10 MB).
pub const DEFAULT_MAX_INDEX_BYTES: u64 = 10 * 1024 * 1024;

/// Environment variable for overriding the global config directory.
pub const ENV_CONFIG_DIR: &str = "SQRY_CONFIG_DIR";

/// Environment variable to disable history recording.
pub const ENV_NO_HISTORY: &str = "SQRY_NO_HISTORY";

/// Environment variable to disable secret redaction (default: enabled).
pub const ENV_NO_REDACT: &str = "SQRY_NO_REDACT";

/// Configuration for the persistence subsystem.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Override for global config directory (from CLI or env)
    pub global_dir_override: Option<PathBuf>,

    /// Override for local config directory (for testing)
    pub local_dir_override: Option<PathBuf>,

    /// Whether history recording is enabled
    pub history_enabled: bool,

    /// Maximum history entries to retain
    pub max_history_entries: usize,

    /// Maximum index size in bytes before rotation
    pub max_index_bytes: u64,

    /// Whether to redact detected secrets from history
    pub redact_secrets: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            global_dir_override: None,
            local_dir_override: None,
            history_enabled: true,
            max_history_entries: DEFAULT_MAX_HISTORY_ENTRIES,
            max_index_bytes: DEFAULT_MAX_INDEX_BYTES,
            // Default to true for privacy - users can opt out via SQRY_NO_REDACT=1
            redact_secrets: true,
        }
    }
}

impl PersistenceConfig {
    /// Create config from environment variables only.
    ///
    /// This is a convenience method for when CLI options are not available.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_and_cli(None, None, false)
    }

    /// Create config from environment variables and CLI overrides.
    ///
    /// CLI options take precedence over environment variables, which take
    /// precedence over defaults.
    ///
    /// # Arguments
    ///
    /// * `cli_config_dir` - Config directory override from CLI
    /// * `cli_max_history` - Max history entries from CLI config file
    /// * `cli_no_history` - Whether `--no-history` was passed
    #[must_use]
    pub fn from_env_and_cli(
        cli_config_dir: Option<PathBuf>,
        cli_max_history: Option<usize>,
        cli_no_history: bool,
    ) -> Self {
        // Config dir: CLI > ENV > default
        let global_dir_override =
            cli_config_dir.or_else(|| std::env::var(ENV_CONFIG_DIR).ok().map(PathBuf::from));

        // History enabled: CLI --no-history > ENV > default (true)
        let history_enabled = if cli_no_history {
            false
        } else {
            std::env::var(ENV_NO_HISTORY)
                .map(|v| !["1", "true", "yes"].contains(&v.to_lowercase().as_str()))
                .unwrap_or(true)
        };

        // Redact secrets: default true, disable via SQRY_NO_REDACT=1
        let redact_secrets = std::env::var(ENV_NO_REDACT)
            .map(|v| !["1", "true", "yes"].contains(&v.to_lowercase().as_str()))
            .unwrap_or(true);

        Self {
            global_dir_override,
            local_dir_override: None,
            history_enabled,
            max_history_entries: cli_max_history.unwrap_or(DEFAULT_MAX_HISTORY_ENTRIES),
            max_index_bytes: DEFAULT_MAX_INDEX_BYTES,
            redact_secrets,
        }
    }

    /// Get the global config directory path.
    ///
    /// Uses the override if set, otherwise returns the platform-specific
    /// config directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the config directory cannot be determined.
    pub fn global_config_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(ref override_path) = self.global_dir_override {
            return Ok(override_path.clone());
        }

        // Use dirs crate for cross-platform paths
        dirs::config_dir()
            .map(|p| p.join("sqry"))
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))
    }

    /// Get the local config directory path.
    ///
    /// For local storage, this is the project root where `.sqry-index.user`
    /// will be stored.
    ///
    /// # Arguments
    ///
    /// * `project_root` - The project root directory
    #[must_use]
    pub fn local_config_dir(&self, project_root: &std::path::Path) -> PathBuf {
        self.local_dir_override
            .clone()
            .unwrap_or_else(|| project_root.to_path_buf())
    }
}

/// Get the default global config directory.
///
/// This is a convenience function for when you don't have a full config.
///
/// # Errors
///
/// Returns an error if the config directory cannot be determined.
pub fn global_config_dir() -> anyhow::Result<PathBuf> {
    dirs::config_dir()
        .map(|p| p.join("sqry"))
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_default_config() {
        let config = PersistenceConfig::default();

        assert!(config.global_dir_override.is_none());
        assert!(config.local_dir_override.is_none());
        assert!(config.history_enabled);
        assert_eq!(config.max_history_entries, DEFAULT_MAX_HISTORY_ENTRIES);
        assert_eq!(config.max_index_bytes, DEFAULT_MAX_INDEX_BYTES);
        // Default is now true for privacy
        assert!(
            config.redact_secrets,
            "redact_secrets should default to true"
        );
    }

    #[test]
    #[serial]
    fn test_cli_config_dir_takes_precedence() {
        // Set env var
        unsafe {
            std::env::set_var(ENV_CONFIG_DIR, "/env/path");
        }

        let config =
            PersistenceConfig::from_env_and_cli(Some(PathBuf::from("/cli/path")), None, false);

        assert_eq!(config.global_dir_override, Some(PathBuf::from("/cli/path")));

        unsafe {
            std::env::remove_var(ENV_CONFIG_DIR);
        }
    }

    #[test]
    #[serial]
    fn test_env_config_dir_fallback() {
        unsafe {
            std::env::set_var(ENV_CONFIG_DIR, "/env/path");
        }

        let config = PersistenceConfig::from_env_and_cli(None, None, false);

        assert_eq!(config.global_dir_override, Some(PathBuf::from("/env/path")));

        unsafe {
            std::env::remove_var(ENV_CONFIG_DIR);
        }
    }

    #[test]
    fn test_cli_no_history_disables_history() {
        let config = PersistenceConfig::from_env_and_cli(None, None, true);

        assert!(!config.history_enabled);
    }

    #[test]
    #[serial]
    fn test_env_no_history_disables_history() {
        unsafe {
            std::env::set_var(ENV_NO_HISTORY, "1");
        }

        let config = PersistenceConfig::from_env_and_cli(None, None, false);

        assert!(!config.history_enabled);

        unsafe {
            std::env::remove_var(ENV_NO_HISTORY);
        }
    }

    #[test]
    #[serial]
    fn test_redact_secrets_default_enabled() {
        // Default is now true (enabled)
        let config = PersistenceConfig::from_env_and_cli(None, None, false);
        assert!(
            config.redact_secrets,
            "redact_secrets should default to true"
        );
    }

    #[test]
    #[serial]
    fn test_redact_secrets_disabled_via_env() {
        unsafe {
            std::env::set_var(ENV_NO_REDACT, "1");
        }

        let config = PersistenceConfig::from_env_and_cli(None, None, false);

        assert!(
            !config.redact_secrets,
            "SQRY_NO_REDACT=1 should disable redaction"
        );

        unsafe {
            std::env::remove_var(ENV_NO_REDACT);
        }
    }

    #[test]
    fn test_global_config_dir_with_override() {
        let config = PersistenceConfig {
            global_dir_override: Some(PathBuf::from("/custom/path")),
            ..Default::default()
        };

        let dir = config.global_config_dir().expect("should succeed");
        assert_eq!(dir, PathBuf::from("/custom/path"));
    }

    #[test]
    fn test_local_config_dir() {
        let config = PersistenceConfig::default();
        let project = PathBuf::from("/home/user/project");

        let local_dir = config.local_config_dir(&project);
        assert_eq!(local_dir, project);
    }

    #[test]
    fn test_local_config_dir_with_override() {
        let config = PersistenceConfig {
            local_dir_override: Some(PathBuf::from("/custom/local")),
            ..Default::default()
        };
        let project = PathBuf::from("/home/user/project");

        let local_dir = config.local_config_dir(&project);
        assert_eq!(local_dir, PathBuf::from("/custom/local"));
    }
}
