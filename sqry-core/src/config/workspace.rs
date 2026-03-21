//! Workspace configuration for sqry.
//!
//! This module contains configuration for workspace discovery and resolution.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::env;

/// Workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Maximum depth for workspace discovery (default: 100)
    ///
    /// Controls how far up the directory tree the workspace resolver will
    /// search for a `.sqry` directory. This prevents infinite loops and
    /// excessive filesystem traversal.
    ///
    /// # Validation
    /// - Minimum: 10 (must search at least 10 levels)
    /// - Maximum: 1000 (recommended limit)
    /// - Hard cap: 1000 (absolute maximum, security constraint)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_WORKSPACE_DISCOVERY_DEPTH` environment variable.
    pub discovery_max_depth: usize,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            discovery_max_depth: 100,
        }
    }
}

impl WorkspaceConfig {
    /// Minimum allowed discovery depth
    const MIN_DISCOVERY_DEPTH: usize = 10;

    /// Maximum recommended discovery depth
    const MAX_DISCOVERY_DEPTH: usize = 1000;

    /// Absolute hard cap for discovery depth (security constraint)
    const ABSOLUTE_MAX_DISCOVERY_DEPTH: usize = 1000;

    /// Create a new workspace configuration with custom discovery depth
    ///
    /// # Errors
    ///
    /// Returns an error if the provided discovery depth violates safety constraints.
    pub fn new(discovery_max_depth: usize) -> Result<Self> {
        let config = Self {
            discovery_max_depth,
        };
        config.validate()?;
        Ok(config)
    }

    /// Load configuration with environment variable overrides
    ///
    /// # Errors
    ///
    /// Returns an error if environment variables contain invalid values or if validation fails.
    pub fn load_or_default() -> Result<Self> {
        let mut config = Self::default();

        // Apply environment variable override if present
        if let Ok(depth_str) = env::var("SQRY_WORKSPACE_DISCOVERY_DEPTH") {
            config.discovery_max_depth =
                Self::parse_env_var(&depth_str, "SQRY_WORKSPACE_DISCOVERY_DEPTH")?;
        }

        config.validate()?;
        Ok(config)
    }

    /// Get effective discovery depth with validation
    ///
    /// This method enforces all validation constraints and returns the
    /// safe-to-use discovery depth value.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (unlimited not allowed)
    /// - Value is below minimum (< 10)
    /// - Value exceeds hard cap (> 1000)
    pub fn effective_discovery_depth(&self) -> Result<usize> {
        if self.discovery_max_depth == 0 {
            bail!("workspace.discovery_max_depth cannot be 0 (unlimited not allowed for safety)");
        }

        if self.discovery_max_depth < Self::MIN_DISCOVERY_DEPTH {
            bail!(
                "workspace.discovery_max_depth {} is below minimum {}",
                self.discovery_max_depth,
                Self::MIN_DISCOVERY_DEPTH
            );
        }

        if self.discovery_max_depth > Self::MAX_DISCOVERY_DEPTH {
            tracing::warn!(
                "workspace.discovery_max_depth {} exceeds recommended maximum {}",
                self.discovery_max_depth,
                Self::MAX_DISCOVERY_DEPTH
            );
        }

        if self.discovery_max_depth > Self::ABSOLUTE_MAX_DISCOVERY_DEPTH {
            bail!(
                "workspace.discovery_max_depth {} exceeds absolute hard cap {}",
                self.discovery_max_depth,
                Self::ABSOLUTE_MAX_DISCOVERY_DEPTH
            );
        }

        Ok(self.discovery_max_depth)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        // Validation happens in effective_discovery_depth()
        self.effective_discovery_depth()?;
        Ok(())
    }

    /// Parse environment variable with strict error handling
    ///
    /// This method implements the fail-fast parse policy:
    /// - Parse errors result in immediate failure with clear message
    /// - Out-of-range values result in immediate failure
    /// - NO silent fallbacks to default values
    fn parse_env_var(value: &str, var_name: &str) -> Result<usize> {
        match value.parse::<usize>() {
            Ok(parsed) => Ok(parsed),
            Err(_) => bail!(
                "Invalid value for {}: '{}'. Expected usize in range [{}, {}]",
                var_name,
                value,
                Self::MIN_DISCOVERY_DEPTH,
                Self::ABSOLUTE_MAX_DISCOVERY_DEPTH
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = WorkspaceConfig::default();
        assert_eq!(config.discovery_max_depth, 100);
        assert!(config.effective_discovery_depth().is_ok());
    }

    #[test]
    fn test_new_with_valid_depth() {
        let config = WorkspaceConfig::new(50).unwrap();
        assert_eq!(config.effective_discovery_depth().unwrap(), 50);
    }

    #[test]
    fn test_new_with_zero_depth_fails() {
        let result = WorkspaceConfig::new(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_below_minimum_fails() {
        let result = WorkspaceConfig::new(5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("below minimum 10"));
    }

    #[test]
    fn test_at_minimum_succeeds() {
        let config = WorkspaceConfig::new(10).unwrap();
        assert_eq!(config.effective_discovery_depth().unwrap(), 10);
    }

    #[test]
    fn test_at_maximum_succeeds() {
        let config = WorkspaceConfig::new(1000).unwrap();
        assert_eq!(config.effective_discovery_depth().unwrap(), 1000);
    }

    #[test]
    fn test_above_hard_cap_fails() {
        let result = WorkspaceConfig::new(1001);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds absolute hard cap")
        );
    }

    #[test]
    fn test_parse_env_var_valid() {
        let result = WorkspaceConfig::parse_env_var("50", "TEST_VAR");
        assert_eq!(result.unwrap(), 50);
    }

    #[test]
    fn test_parse_env_var_invalid() {
        let result = WorkspaceConfig::parse_env_var("abc", "TEST_VAR");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid value for TEST_VAR")
        );
    }

    #[test]
    fn test_parse_env_var_negative() {
        let result = WorkspaceConfig::parse_env_var("-10", "TEST_VAR");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid value"));
    }

    #[test]
    fn test_effective_depth_warns_at_max() {
        let config = WorkspaceConfig {
            discovery_max_depth: 1000,
        };
        // Should succeed but log warning
        assert_eq!(config.effective_discovery_depth().unwrap(), 1000);
    }
}
