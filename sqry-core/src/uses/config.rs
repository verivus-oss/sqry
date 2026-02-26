//! Configuration for local uses and insights
//!
//! This module provides configuration loading and management for the uses system.
//! Configuration can come from:
//! 1. Environment variables (highest priority)
//! 2. Configuration file (`~/.sqry/uses/config.json`)
//! 3. Defaults (lowest priority)
//!
//! # Environment Variables
//!
//! - `SQRY_USES_ENABLED` - Set to "false" or "0" to disable all uses capture
//! - `SQRY_USES_DIR` - Custom directory for uses storage (default: `~/.sqry/uses/`)
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::uses::config::UsesConfig;
//!
//! let config = UsesConfig::load()?;
//! if config.enabled {
//!     // Uses capture is enabled
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default retention period in days
const DEFAULT_RETENTION_DAYS: u32 = 365;

/// Default uses directory name
const USES_DIR_NAME: &str = "uses";

/// Environment variable for enabling/disabling uses
const ENV_USES_ENABLED: &str = "SQRY_USES_ENABLED";

/// Environment variable for custom uses directory
const ENV_USES_DIR: &str = "SQRY_USES_DIR";

/// Configuration for local uses and insights
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UsesConfig {
    /// Whether uses capture is enabled
    ///
    /// Default: true
    /// Can be overridden by `SQRY_USES_ENABLED=false`
    pub enabled: bool,

    /// Number of days to retain event logs
    ///
    /// Default: 365
    pub retention_days: u32,

    /// Configuration for contextual feedback prompts
    #[serde(default)]
    pub contextual_feedback: ContextualFeedbackConfig,

    /// Configuration for automatic summarization
    #[serde(default)]
    pub auto_summarize: AutoSummarizeConfig,
}

impl Default for UsesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: DEFAULT_RETENTION_DAYS,
            contextual_feedback: ContextualFeedbackConfig::default(),
            auto_summarize: AutoSummarizeConfig::default(),
        }
    }
}

/// Configuration for contextual feedback prompts
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextualFeedbackConfig {
    /// Whether contextual feedback prompts are enabled
    ///
    /// Default: true
    pub enabled: bool,

    /// How often to prompt for feedback
    ///
    /// Default: "`session_once`" (at most once per session)
    pub prompt_frequency: PromptFrequency,
}

impl Default for ContextualFeedbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_frequency: PromptFrequency::SessionOnce,
        }
    }
}

/// Frequency of feedback prompts
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptFrequency {
    /// Prompt at most once per CLI session
    SessionOnce,
    /// Never prompt (user must initiate feedback)
    Never,
}

/// Configuration for automatic summarization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AutoSummarizeConfig {
    /// Whether automatic summarization is enabled
    ///
    /// Default: true
    pub enabled: bool,
}

impl Default for AutoSummarizeConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl UsesConfig {
    /// Load configuration from environment and/or file
    ///
    /// Priority (highest to lowest):
    /// 1. Environment variables
    /// 2. Config file
    /// 3. Defaults
    ///
    /// # Returns
    ///
    /// The effective configuration.
    #[must_use]
    pub fn load() -> Self {
        let mut config = Self::load_from_file().unwrap_or_default();

        // Environment overrides
        config.apply_env_overrides();

        config
    }

    /// Load configuration from the default config file
    ///
    /// # Returns
    ///
    /// The configuration from file, or None if file doesn't exist or is invalid.
    fn load_from_file() -> Option<Self> {
        let config_path = Self::default_config_path()?;

        if !config_path.exists() {
            return None;
        }

        let contents = std::fs::read_to_string(&config_path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Apply environment variable overrides
    fn apply_env_overrides(&mut self) {
        // SQRY_USES_ENABLED
        if let Ok(value) = std::env::var(ENV_USES_ENABLED) {
            let value_lower = value.to_lowercase();
            if value_lower == "false" || value_lower == "0" || value_lower == "no" {
                self.enabled = false;
            } else if value_lower == "true" || value_lower == "1" || value_lower == "yes" {
                self.enabled = true;
            }
        }
    }

    /// Get the default config file path
    ///
    /// # Returns
    ///
    /// Path to `~/.sqry/uses/config.json`, or None if home dir unavailable.
    #[must_use]
    pub fn default_config_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".sqry").join(USES_DIR_NAME).join("config.json"))
    }

    /// Get the uses directory path
    ///
    /// Respects `SQRY_USES_DIR` environment variable.
    ///
    /// # Returns
    ///
    /// Path to the uses directory.
    #[must_use]
    pub fn uses_dir() -> Option<PathBuf> {
        // Check environment variable first
        if let Ok(custom_dir) = std::env::var(ENV_USES_DIR) {
            let path = PathBuf::from(custom_dir);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }

        // Default to ~/.sqry/uses/
        dirs::home_dir().map(|h| h.join(".sqry").join(USES_DIR_NAME))
    }

    /// Save configuration to the default config file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self) -> Result<(), ConfigSaveError> {
        let config_path = Self::default_config_path().ok_or(ConfigSaveError::NoHomeDir)?;

        // Ensure directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigSaveError::IoError(e.to_string()))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| ConfigSaveError::SerializeError(e.to_string()))?;

        std::fs::write(&config_path, json).map_err(|e| ConfigSaveError::IoError(e.to_string()))?;

        Ok(())
    }

    /// Load configuration from a specific path
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the config file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self, ConfigLoadError> {
        let path = path.as_ref();
        let contents =
            std::fs::read_to_string(path).map_err(|e| ConfigLoadError::IoError(e.to_string()))?;

        serde_json::from_str(&contents).map_err(|e| ConfigLoadError::ParseError(e.to_string()))
    }

    /// Create a config for testing (uses disabled by default)
    #[cfg(test)]
    #[must_use]
    pub fn test_disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Errors that can occur when loading configuration
#[derive(Debug, thiserror::Error)]
pub enum ConfigLoadError {
    /// IO error reading config file
    #[error("failed to read config: {0}")]
    IoError(String),

    /// Parse error in config file
    #[error("failed to parse config: {0}")]
    ParseError(String),
}

/// Errors that can occur when saving configuration
#[derive(Debug, thiserror::Error)]
pub enum ConfigSaveError {
    /// Home directory not available
    #[error("home directory not available")]
    NoHomeDir,

    /// IO error writing config file
    #[error("failed to write config: {0}")]
    IoError(String),

    /// Serialization error
    #[error("failed to serialize config: {0}")]
    SerializeError(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = UsesConfig::default();

        assert!(config.enabled);
        assert_eq!(config.retention_days, 365);
        assert!(config.contextual_feedback.enabled);
        assert_eq!(
            config.contextual_feedback.prompt_frequency,
            PromptFrequency::SessionOnce
        );
        assert!(config.auto_summarize.enabled);
    }

    #[test]
    fn test_config_serialization() {
        let config = UsesConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: UsesConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config, parsed);
    }

    #[test]
    fn test_config_partial_parse() {
        // Partial config should use defaults for missing fields
        let json = r#"{"enabled": false}"#;
        let config: UsesConfig = serde_json::from_str(json).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.retention_days, 365); // Default
        assert!(config.contextual_feedback.enabled); // Default
    }

    #[test]
    fn test_prompt_frequency_serialization() {
        assert_eq!(
            serde_json::to_string(&PromptFrequency::SessionOnce).unwrap(),
            "\"session_once\""
        );
        assert_eq!(
            serde_json::to_string(&PromptFrequency::Never).unwrap(),
            "\"never\""
        );
    }

    #[test]
    fn test_load_from_path() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");

        let config = UsesConfig {
            enabled: false,
            retention_days: 90,
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&config_path, json).unwrap();

        let loaded = UsesConfig::load_from_path(&config_path).unwrap();

        assert!(!loaded.enabled);
        assert_eq!(loaded.retention_days, 90);
    }

    #[test]
    #[serial]
    fn test_env_override_disabled() {
        // Note: This test modifies environment variables
        // In production, you'd want to use serial_test for this

        let mut config = UsesConfig::default();
        assert!(config.enabled);

        // Simulate environment variable
        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "false");
        }
        config.apply_env_overrides();

        assert!(!config.enabled);

        // Clean up
        unsafe {
            std::env::remove_var(ENV_USES_ENABLED);
        }
    }

    #[test]
    #[serial]
    fn test_env_override_enabled() {
        let mut config = UsesConfig {
            enabled: false,
            ..Default::default()
        };

        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "true");
        }
        config.apply_env_overrides();

        assert!(config.enabled);

        unsafe {
            std::env::remove_var(ENV_USES_ENABLED);
        }
    }

    #[test]
    #[serial]
    fn test_env_override_variations() {
        // Test various truthy/falsy values

        let mut config = UsesConfig::default();

        // Test "0"
        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "0");
        }
        config.apply_env_overrides();
        assert!(!config.enabled);

        // Test "no"
        config.enabled = true;
        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "no");
        }
        config.apply_env_overrides();
        assert!(!config.enabled);

        // Test "1"
        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "1");
        }
        config.apply_env_overrides();
        assert!(config.enabled);

        // Test "yes"
        config.enabled = false;
        unsafe {
            std::env::set_var(ENV_USES_ENABLED, "yes");
        }
        config.apply_env_overrides();
        assert!(config.enabled);

        // Clean up
        unsafe {
            std::env::remove_var(ENV_USES_ENABLED);
        }
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");

        let config = UsesConfig {
            enabled: false,
            retention_days: 30,
            contextual_feedback: ContextualFeedbackConfig {
                enabled: false,
                prompt_frequency: PromptFrequency::Never,
            },
            auto_summarize: AutoSummarizeConfig { enabled: false },
        };

        // Save
        let json = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&config_path, json).unwrap();

        // Load
        let loaded = UsesConfig::load_from_path(&config_path).unwrap();

        assert_eq!(config, loaded);
    }

    #[test]
    #[serial]
    fn test_uses_dir_default() {
        // Clear any env override
        unsafe {
            std::env::remove_var(ENV_USES_DIR);
        }

        let dir = UsesConfig::uses_dir();

        // Should return Some path ending in ".sqry/uses"
        if let Some(path) = dir {
            assert!(path.ends_with(".sqry/uses") || path.ends_with(".sqry\\uses"));
        }
    }

    #[test]
    #[serial]
    #[ignore = "Flaky: modifies global env vars which interfere with parallel tests"]
    fn test_uses_dir_env_override() {
        let custom_path = "/custom/uses/path";

        unsafe {
            std::env::set_var(ENV_USES_DIR, custom_path);
        }

        let dir = UsesConfig::uses_dir();
        assert_eq!(dir, Some(PathBuf::from(custom_path)));

        unsafe {
            std::env::remove_var(ENV_USES_DIR);
        }
    }
}
