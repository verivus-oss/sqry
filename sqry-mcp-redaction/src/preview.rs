//! Dry-run/preview mode for redaction.
//!
//! Provides detailed information about what would be redacted without
//! actually modifying the input.

use crate::redactor::RedactionResult;

/// Preview of what would be redacted (for dry-run mode).
#[derive(Debug, Default)]
pub struct RedactionPreview {
    /// Fields that would be redacted, with JSONPath locations.
    pub would_redact: Vec<RedactionTarget>,

    /// Fields that would be preserved.
    pub would_preserve: Vec<String>,

    /// Summary statistics (same as `RedactionResult`).
    pub stats: RedactionResult,
}

impl RedactionPreview {
    /// Create a new empty preview.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any redaction would occur.
    #[must_use]
    pub fn would_redact_anything(&self) -> bool {
        !self.would_redact.is_empty()
    }

    /// Get the number of fields that would be redacted.
    #[must_use]
    pub fn redaction_count(&self) -> usize {
        self.would_redact.len()
    }
}

/// A specific redaction target.
#[derive(Debug, Clone)]
pub struct RedactionTarget {
    /// JSONPath to the field.
    pub path: String,

    /// Original value (truncated for large values).
    pub original_preview: String,

    /// What it would be replaced with.
    pub replacement: String,

    /// Reason for redaction.
    pub reason: RedactionReason,
}

impl RedactionTarget {
    /// Create a new redaction target.
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        original_preview: impl Into<String>,
        replacement: impl Into<String>,
        reason: RedactionReason,
    ) -> Self {
        Self {
            path: path.into(),
            original_preview: original_preview.into(),
            replacement: replacement.into(),
            reason,
        }
    }
}

/// Reason for redaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionReason {
    /// Absolute path detected.
    AbsolutePath,

    /// File URI detected.
    FileUri,

    /// Workspace path field.
    WorkspacePath,

    /// Code context field.
    CodeContext,

    /// Documentation field.
    Documentation,

    /// Custom field (user-specified).
    CustomField,

    /// Pattern match (path in arbitrary string).
    PatternMatch,

    /// Unknown field (whitelist mode).
    UnknownField,

    /// Value exceeded the maximum nesting depth limit.
    DepthLimitExceeded,
}

impl RedactionReason {
    /// Get a human-readable description.
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self {
            Self::AbsolutePath => "Absolute path",
            Self::FileUri => "File URI",
            Self::WorkspacePath => "Workspace path",
            Self::CodeContext => "Code context",
            Self::Documentation => "Documentation",
            Self::CustomField => "Custom field",
            Self::PatternMatch => "Pattern-detected path",
            Self::UnknownField => "Unknown field (not in whitelist)",
            Self::DepthLimitExceeded => "Depth limit exceeded",
        }
    }
}

impl std::fmt::Display for RedactionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preview_new() {
        let preview = RedactionPreview::new();
        assert!(preview.would_redact.is_empty());
        assert!(preview.would_preserve.is_empty());
        assert!(!preview.would_redact_anything());
    }

    #[test]
    fn test_preview_with_targets() {
        let mut preview = RedactionPreview::new();
        preview.would_redact.push(RedactionTarget::new(
            "$.fileUri",
            "file:///home/user/file.rs",
            "src/file.rs",
            RedactionReason::FileUri,
        ));

        assert!(preview.would_redact_anything());
        assert_eq!(preview.redaction_count(), 1);
    }

    #[test]
    fn test_redaction_target() {
        let target = RedactionTarget::new(
            "$.workspace_path",
            "/home/user/project",
            "<workspace>",
            RedactionReason::WorkspacePath,
        );

        assert_eq!(target.path, "$.workspace_path");
        assert_eq!(target.reason, RedactionReason::WorkspacePath);
    }

    #[test]
    fn test_redaction_reason_display() {
        assert_eq!(RedactionReason::AbsolutePath.description(), "Absolute path");
        assert_eq!(RedactionReason::FileUri.description(), "File URI");
        assert_eq!(
            RedactionReason::UnknownField.description(),
            "Unknown field (not in whitelist)"
        );
    }
}
