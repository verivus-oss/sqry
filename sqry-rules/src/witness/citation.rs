//! Source citation records attached to rule witnesses.

use serde::{Deserialize, Serialize};

/// Fixed-width source span for persisted rule citations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitationSpan {
    /// One-based start line.
    pub start_line: u32,
    /// Zero-based start column.
    pub start_column: u32,
    /// One-based end line.
    pub end_line: u32,
    /// Zero-based end column.
    pub end_column: u32,
}

impl CitationSpan {
    /// Creates a fixed-width citation span.
    #[must_use]
    pub const fn new(start_line: u32, start_column: u32, end_line: u32, end_column: u32) -> Self {
        Self {
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }
}

/// Source citation emitted with a rule witness.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCitation {
    /// Repository-relative file path.
    pub file_path: String,
    /// Optional source span.
    pub span: Option<CitationSpan>,
    /// Optional stable label describing the cited fact.
    pub label: Option<String>,
}

impl RuleCitation {
    /// Creates a citation for a repository-relative file path.
    #[must_use]
    pub fn new(file_path: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
            span: None,
            label: None,
        }
    }

    /// Adds a source span.
    #[must_use]
    pub const fn with_span(mut self, span: CitationSpan) -> Self {
        self.span = Some(span);
        self
    }

    /// Adds a stable label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}
