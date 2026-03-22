//! Safety validation for generated commands.
//!
//! All commands must pass validation before execution:
//! - Whitelist matching against allowed templates
//! - Shell metacharacter rejection
//! - Environment variable expansion prevention
//! - Path traversal prevention

mod metachar;
mod path_guard;
mod whitelist;

use crate::error::{NlResult, ValidatorError};
use crate::types::ValidationStatus;

/// Maximum command length in characters
pub const MAX_COMMAND_LENGTH: usize = 512;

/// Convenience function to validate a command and return its status.
///
/// Uses default validator settings (strict mode enabled).
#[must_use]
pub fn validate_command(command: &str) -> ValidationStatus {
    let validator = SafetyValidator::new();
    match validator.validate(command) {
        Ok(status) => status,
        Err(crate::error::NlError::Validator(err)) => match err {
            ValidatorError::MetacharDetected => ValidationStatus::RejectedMetachar,
            ValidatorError::EnvVarDetected => ValidationStatus::RejectedEnvVar,
            ValidatorError::PathTraversal | ValidatorError::AbsolutePath => {
                ValidationStatus::RejectedPathTraversal
            }
            ValidatorError::CommandTooLong => ValidationStatus::RejectedTooLong,
            ValidatorError::WriteOperation => ValidationStatus::RejectedWriteMode,
            ValidatorError::TemplateMismatch => ValidationStatus::RejectedUnknown,
        },
        Err(_) => ValidationStatus::RejectedUnknown,
    }
}

/// Safety validator for generated commands.
pub struct SafetyValidator {
    /// Whether to enforce strict whitelist matching
    strict_mode: bool,
}

impl Default for SafetyValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl SafetyValidator {
    /// Create a new validator with default settings (strict mode enabled).
    #[must_use]
    pub fn new() -> Self {
        Self { strict_mode: true }
    }

    /// Create a validator with custom strictness.
    #[must_use]
    pub fn with_strict_mode(strict_mode: bool) -> Self {
        Self { strict_mode }
    }

    /// Validate a command string.
    ///
    /// # Errors
    ///
    /// Returns [`ValidatorError`] if the command fails any validation check.
    pub fn validate(&self, command: &str) -> NlResult<ValidationStatus> {
        // 1. Check length
        if command.len() > MAX_COMMAND_LENGTH {
            return Err(ValidatorError::CommandTooLong.into());
        }

        // 2. Check for dangerous metacharacters
        if let Some(status) = metachar::contains_dangerous_chars(command) {
            return match status {
                ValidationStatus::RejectedEnvVar => Err(ValidatorError::EnvVarDetected.into()),
                _ => Err(ValidatorError::MetacharDetected.into()),
            };
        }

        // 3. Check for path traversal
        if path_guard::contains_path_traversal(command) {
            return Err(ValidatorError::PathTraversal.into());
        }

        // 4. Check for absolute paths
        if path_guard::contains_absolute_path(command) {
            return Err(ValidatorError::AbsolutePath.into());
        }

        // 5. Check for write operations
        if metachar::contains_write_operation(command) {
            return Err(ValidatorError::WriteOperation.into());
        }

        // 6. Check whitelist (if strict mode)
        if self.strict_mode && !whitelist::matches_allowed_template(command) {
            return Err(ValidatorError::TemplateMismatch.into());
        }

        Ok(ValidationStatus::Valid)
    }

    /// Quick check without detailed error.
    #[must_use]
    pub fn is_valid(&self, command: &str) -> bool {
        self.validate(command).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_query_command() {
        let validator = SafetyValidator::new();
        let result = validator.validate("sqry query \"authenticate\"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_reject_semicolon() {
        let validator = SafetyValidator::new();
        let result = validator.validate("sqry query \"foo\"; rm -rf /");
        assert!(matches!(
            result,
            Err(crate::error::NlError::Validator(
                ValidatorError::MetacharDetected
            ))
        ));
    }

    #[test]
    fn test_reject_env_var() {
        let validator = SafetyValidator::new();
        let result = validator.validate("sqry query \"$HOME\"");
        assert!(matches!(
            result,
            Err(crate::error::NlError::Validator(
                ValidatorError::EnvVarDetected
            ))
        ));
    }

    #[test]
    fn test_reject_path_traversal() {
        let validator = SafetyValidator::new();
        // Path traversal outside quotes should be rejected
        let result = validator.validate("sqry query ../../../etc/passwd");
        assert!(matches!(
            result,
            Err(crate::error::NlError::Validator(
                ValidatorError::PathTraversal
            ))
        ));
    }

    #[test]
    fn test_allow_quoted_double_dot() {
        // Path traversal patterns inside quotes are allowed (for search patterns)
        let validator = SafetyValidator::new();
        let result = validator.validate("sqry query \"..*password\"");
        // Should not fail on path traversal (may fail on template mismatch but not path)
        assert!(!matches!(
            result,
            Err(crate::error::NlError::Validator(
                ValidatorError::PathTraversal
            ))
        ));
    }

    #[test]
    fn test_reject_too_long() {
        let validator = SafetyValidator::new();
        let long_command = format!("sqry query \"{}\"", "x".repeat(MAX_COMMAND_LENGTH));
        let result = validator.validate(&long_command);
        assert!(matches!(
            result,
            Err(crate::error::NlError::Validator(
                ValidatorError::CommandTooLong
            ))
        ));
    }
}
