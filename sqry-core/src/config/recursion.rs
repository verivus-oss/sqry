//! Recursion limits configuration for sqry.
//!
//! This module enforces recursion depth limits to prevent resource exhaustion
//! and stack overflow attacks from deeply nested code structures.
//!
//! # Security Model
//!
//! Defense-in-depth approach with multiple layers:
//!
//! 1. **Default limits**: Conservative defaults (100 file ops, 1000 expr fuel)
//! 2. **Configurable limits**: Users can adjust based on their needs
//! 3. **Hard caps**: Absolute maximums that cannot be bypassed (200 file ops, 10,000 expr fuel)
//! 4. **Validation**: All values validated against hard caps to prevent config injection
//!
//! # Attack Vectors Mitigated
//!
//! - **Stack overflow**: Deep recursion exhausting call stack
//! - **Resource exhaustion**: Unbounded recursion consuming CPU/memory
//! - **Config injection**: Malicious config files setting extreme limits
//! - **AST bombs**: Pathological inputs with extreme nesting depth

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::env;

/// Recursion limits configuration
///
/// Controls maximum depth for recursive operations to prevent stack overflow
/// from pathological inputs (deeply nested AST structures, complex expressions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecursionLimits {
    /// Maximum depth for file operation recursion (AST traversal, directory walking)
    ///
    /// Controls how deep the AST traversal can go when processing source files.
    /// This prevents stack overflow from pathological code like:
    /// ```text
    /// fn f0() { fn f1() { fn f2() { ... fn f1000() {} ... } } }
    /// ```
    ///
    /// # Validation
    /// - Minimum: 50 (must handle moderately nested code)
    /// - Maximum: 150 (recommended for deeply nested code)
    /// - Hard cap: 200 (absolute maximum, NON-NEGOTIABLE security constraint)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_RECURSION_FILE_OPS_DEPTH` environment variable.
    pub file_ops_depth: usize,

    /// Maximum depth for expression evaluation (query expressions, AST patterns)
    ///
    /// Controls how deep expression trees can be when evaluating queries.
    /// This prevents stack overflow from complex nested boolean expressions:
    /// ```text
    /// (((((a AND b) OR c) AND d) OR e) AND ...)
    /// ```
    ///
    /// # Validation
    /// - Minimum: 10 (must handle basic expressions)
    /// - Maximum: 100 (recommended)
    /// - Hard cap: 200 (absolute maximum, security constraint)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_RECURSION_EXPR_DEPTH` environment variable.
    pub expr_depth: usize,

    /// Fuel limit for expression evaluation (operation counter)
    ///
    /// Alternative to depth limiting using an operation counter ("fuel").
    /// Each recursive call consumes fuel, preventing both deep and wide
    /// recursion patterns.
    ///
    /// # Validation
    /// - Minimum: 100 (must handle basic queries)
    /// - Maximum: 5000 (recommended)
    /// - Hard cap: 10,000 (absolute maximum, NON-NEGOTIABLE security constraint)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_RECURSION_EXPR_FUEL` environment variable.
    pub expr_fuel: usize,
}

impl Default for RecursionLimits {
    fn default() -> Self {
        Self {
            file_ops_depth: 100,
            expr_depth: 100,
            expr_fuel: 1000,
        }
    }
}

impl RecursionLimits {
    /// Minimum allowed file ops depth
    pub const MIN_FILE_OPS_DEPTH: usize = 50;

    /// Maximum recommended file ops depth
    pub const MAX_FILE_OPS_DEPTH: usize = 150;

    /// Absolute hard cap for file ops depth (NON-NEGOTIABLE)
    ///
    /// Set to 200 (2x default) to handle legitimate deep directory structures
    /// while preventing resource exhaustion. This limit has been security-reviewed
    /// and balances usability with protection against stack overflow attacks.
    pub const ABSOLUTE_MAX_FILE_OPS_DEPTH: usize = 200;

    /// Minimum allowed expression depth
    pub const MIN_EXPR_DEPTH: usize = 10;

    /// Maximum recommended expression depth
    pub const MAX_EXPR_DEPTH: usize = 100;

    /// Absolute hard cap for expression depth
    pub const ABSOLUTE_MAX_EXPR_DEPTH: usize = 200;

    /// Minimum allowed expression fuel
    pub const MIN_EXPR_FUEL: usize = 100;

    /// Maximum recommended expression fuel
    pub const MAX_EXPR_FUEL: usize = 5_000;

    /// Absolute hard cap for expression fuel (NON-NEGOTIABLE)
    ///
    /// Set to 10,000 to prevent expression bombs (deeply nested operators) from
    /// causing stack overflows during query validation and normalization.
    /// This limit has been security-reviewed and prevents resource exhaustion attacks.
    pub const ABSOLUTE_MAX_EXPR_FUEL: usize = 10_000;

    /// Create a new recursion limits configuration with custom values
    ///
    /// # Errors
    ///
    /// Returns an error if the provided values violate safety constraints (e.g., values are
    /// below minimum thresholds or exceed maximum limits).
    pub fn new(file_ops_depth: usize, expr_depth: usize, expr_fuel: usize) -> Result<Self> {
        let config = Self {
            file_ops_depth,
            expr_depth,
            expr_fuel,
        };
        config.validate()?;
        Ok(config)
    }

    /// Load configuration with environment variable overrides
    ///
    /// # Errors
    ///
    /// Returns an error if environment variables contain invalid values or if the resulting
    /// configuration violates safety constraints.
    pub fn load_or_default() -> Result<Self> {
        let mut config = Self::default();

        // Apply environment variable overrides if present
        if let Ok(file_ops_str) = env::var("SQRY_RECURSION_FILE_OPS_DEPTH") {
            config.file_ops_depth =
                Self::parse_env_var(&file_ops_str, "SQRY_RECURSION_FILE_OPS_DEPTH")?;
        }

        if let Ok(expr_depth_str) = env::var("SQRY_RECURSION_EXPR_DEPTH") {
            config.expr_depth = Self::parse_env_var(&expr_depth_str, "SQRY_RECURSION_EXPR_DEPTH")?;
        }

        if let Ok(expr_fuel_str) = env::var("SQRY_RECURSION_EXPR_FUEL") {
            config.expr_fuel = Self::parse_env_var(&expr_fuel_str, "SQRY_RECURSION_EXPR_FUEL")?;
        }

        config.validate()?;
        Ok(config)
    }

    /// Get effective file ops depth with validation
    ///
    /// # Errors
    ///
    /// Returns an error if the configured value is 0 (unlimited not allowed), below the minimum
    /// threshold, or exceeds the absolute maximum limit.
    pub fn effective_file_ops_depth(&self) -> Result<usize> {
        if self.file_ops_depth == 0 {
            bail!("recursion.file_ops_depth cannot be 0 (unlimited not allowed for safety)");
        }

        if self.file_ops_depth < Self::MIN_FILE_OPS_DEPTH {
            bail!(
                "recursion.file_ops_depth {} is below minimum {}",
                self.file_ops_depth,
                Self::MIN_FILE_OPS_DEPTH
            );
        }

        if self.file_ops_depth > Self::MAX_FILE_OPS_DEPTH {
            tracing::warn!(
                "recursion.file_ops_depth {} exceeds recommended maximum {}",
                self.file_ops_depth,
                Self::MAX_FILE_OPS_DEPTH
            );
        }

        if self.file_ops_depth > Self::ABSOLUTE_MAX_FILE_OPS_DEPTH {
            bail!(
                "recursion.file_ops_depth {} exceeds absolute hard cap {}",
                self.file_ops_depth,
                Self::ABSOLUTE_MAX_FILE_OPS_DEPTH
            );
        }

        Ok(self.file_ops_depth)
    }

    /// Get effective expression depth with validation
    ///
    /// # Errors
    ///
    /// Returns an error if the configured value is 0 (unlimited not allowed), below the minimum
    /// threshold, or exceeds the absolute maximum limit.
    pub fn effective_expr_depth(&self) -> Result<usize> {
        if self.expr_depth == 0 {
            bail!("recursion.expr_depth cannot be 0 (unlimited not allowed for safety)");
        }

        if self.expr_depth < Self::MIN_EXPR_DEPTH {
            bail!(
                "recursion.expr_depth {} is below minimum {}",
                self.expr_depth,
                Self::MIN_EXPR_DEPTH
            );
        }

        if self.expr_depth > Self::MAX_EXPR_DEPTH {
            tracing::warn!(
                "recursion.expr_depth {} exceeds recommended maximum {}",
                self.expr_depth,
                Self::MAX_EXPR_DEPTH
            );
        }

        if self.expr_depth > Self::ABSOLUTE_MAX_EXPR_DEPTH {
            bail!(
                "recursion.expr_depth {} exceeds absolute hard cap {}",
                self.expr_depth,
                Self::ABSOLUTE_MAX_EXPR_DEPTH
            );
        }

        Ok(self.expr_depth)
    }

    /// Get effective expression fuel with validation
    ///
    /// # Errors
    ///
    /// Returns an error if the configured value is 0 (unlimited not allowed), below the minimum
    /// threshold, or exceeds the absolute maximum limit.
    pub fn effective_expr_fuel(&self) -> Result<usize> {
        if self.expr_fuel == 0 {
            bail!("recursion.expr_fuel cannot be 0 (unlimited not allowed for safety)");
        }

        if self.expr_fuel < Self::MIN_EXPR_FUEL {
            bail!(
                "recursion.expr_fuel {} is below minimum {}",
                self.expr_fuel,
                Self::MIN_EXPR_FUEL
            );
        }

        if self.expr_fuel > Self::MAX_EXPR_FUEL {
            tracing::warn!(
                "recursion.expr_fuel {} exceeds recommended maximum {}",
                self.expr_fuel,
                Self::MAX_EXPR_FUEL
            );
        }

        if self.expr_fuel > Self::ABSOLUTE_MAX_EXPR_FUEL {
            bail!(
                "recursion.expr_fuel {} exceeds absolute hard cap {}",
                self.expr_fuel,
                Self::ABSOLUTE_MAX_EXPR_FUEL
            );
        }

        Ok(self.expr_fuel)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        // Validation happens in effective_* methods
        self.effective_file_ops_depth()?;
        self.effective_expr_depth()?;
        self.effective_expr_fuel()?;
        Ok(())
    }

    /// Parse environment variable with strict error handling
    fn parse_env_var(value: &str, var_name: &str) -> Result<usize> {
        match value.parse::<usize>() {
            Ok(parsed) => Ok(parsed),
            Err(_) => bail!("Invalid value for {var_name}: '{value}'. Expected usize"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RecursionLimits::default();
        assert_eq!(config.file_ops_depth, 100);
        assert_eq!(config.expr_depth, 100);
        assert_eq!(config.expr_fuel, 1000);
        assert!(config.effective_file_ops_depth().is_ok());
        assert!(config.effective_expr_depth().is_ok());
        assert!(config.effective_expr_fuel().is_ok());
    }

    #[test]
    fn test_new_with_valid_values() {
        let config = RecursionLimits::new(200, 50, 5000).unwrap();
        assert_eq!(config.effective_file_ops_depth().unwrap(), 200);
        assert_eq!(config.effective_expr_depth().unwrap(), 50);
        assert_eq!(config.effective_expr_fuel().unwrap(), 5000);
    }

    #[test]
    fn test_file_ops_depth_zero_fails() {
        let result = RecursionLimits::new(0, 100, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_expr_depth_zero_fails() {
        let result = RecursionLimits::new(100, 0, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_expr_fuel_zero_fails() {
        let result = RecursionLimits::new(100, 100, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_file_ops_depth_below_minimum_fails() {
        let result = RecursionLimits::new(25, 100, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("below minimum 50"));
    }

    #[test]
    fn test_expr_depth_below_minimum_fails() {
        let result = RecursionLimits::new(100, 5, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("below minimum 10"));
    }

    #[test]
    fn test_expr_fuel_below_minimum_fails() {
        let result = RecursionLimits::new(100, 100, 50);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("below minimum 100")
        );
    }

    #[test]
    fn test_file_ops_depth_at_minimum_succeeds() {
        let config = RecursionLimits::new(50, 100, 1000).unwrap();
        assert_eq!(config.effective_file_ops_depth().unwrap(), 50);
    }

    #[test]
    fn test_expr_depth_at_minimum_succeeds() {
        let config = RecursionLimits::new(100, 10, 1000).unwrap();
        assert_eq!(config.effective_expr_depth().unwrap(), 10);
    }

    #[test]
    fn test_expr_fuel_at_minimum_succeeds() {
        let config = RecursionLimits::new(100, 100, 100).unwrap();
        assert_eq!(config.effective_expr_fuel().unwrap(), 100);
    }

    #[test]
    fn test_file_ops_depth_at_hard_cap_succeeds() {
        let config = RecursionLimits::new(200, 100, 1000).unwrap();
        assert_eq!(config.effective_file_ops_depth().unwrap(), 200);
    }

    #[test]
    fn test_expr_depth_at_hard_cap_succeeds() {
        let config = RecursionLimits::new(100, 200, 1000).unwrap();
        assert_eq!(config.effective_expr_depth().unwrap(), 200);
    }

    #[test]
    fn test_expr_fuel_at_hard_cap_succeeds() {
        let config = RecursionLimits::new(100, 100, 10_000).unwrap();
        assert_eq!(config.effective_expr_fuel().unwrap(), 10_000);
    }

    #[test]
    fn test_file_ops_depth_above_hard_cap_fails() {
        let result = RecursionLimits::new(201, 100, 1000);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds absolute hard cap")
        );
    }

    #[test]
    fn test_expr_depth_above_hard_cap_fails() {
        let result = RecursionLimits::new(100, 201, 1000);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds absolute hard cap")
        );
    }

    #[test]
    fn test_expr_fuel_above_hard_cap_fails() {
        let result = RecursionLimits::new(100, 100, 10_001);
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
        let result = RecursionLimits::parse_env_var("150", "TEST_VAR");
        assert_eq!(result.unwrap(), 150);
    }

    #[test]
    fn test_parse_env_var_invalid() {
        let result = RecursionLimits::parse_env_var("abc", "TEST_VAR");
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
        let result = RecursionLimits::parse_env_var("-100", "TEST_VAR");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid value"));
    }
}
