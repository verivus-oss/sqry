//! Scope types for context-aware search.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Unique identifier for a scope within a file.
///
/// Scope IDs are assigned sequentially during scope extraction, starting from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeId(pub usize);

impl ScopeId {
    /// Create a new `ScopeId`.
    #[must_use]
    pub fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<usize> for ScopeId {
    fn from(id: usize) -> Self {
        Self(id)
    }
}

impl From<ScopeId> for usize {
    fn from(id: ScopeId) -> Self {
        id.0
    }
}

/// Scope information for context-aware search.
///
/// Represents a lexical scope in source code (function, class, module, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    /// Unique identifier for this scope (file-local).
    ///
    /// Assigned during extraction, sequential starting from 0.
    pub id: ScopeId,

    /// Scope type (function, class, module, etc.).
    pub scope_type: String,

    /// Scope name.
    pub name: String,

    /// File path where scope is defined.
    pub file_path: PathBuf,

    /// Line number where scope starts (1-based).
    pub start_line: usize,

    /// Column where scope starts (0-based).
    pub start_column: usize,

    /// Line number where scope ends (1-based).
    pub end_line: usize,

    /// Column where scope ends (0-based).
    pub end_column: usize,

    /// Parent scope ID (immediate containing scope).
    ///
    /// Set by `link_nested_scopes()` algorithm.
    pub parent_id: Option<ScopeId>,
}

impl Scope {
    /// Create a new scope.
    #[must_use]
    pub fn new(scope_type: String, name: String, file_path: PathBuf) -> Self {
        Self {
            id: ScopeId::new(0),
            scope_type,
            name,
            file_path,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
            parent_id: None,
        }
    }

    /// Set the location (start and end position).
    #[must_use]
    pub fn with_location(
        mut self,
        start_line: usize,
        start_column: usize,
        end_line: usize,
        end_column: usize,
    ) -> Self {
        self.start_line = start_line;
        self.start_column = start_column;
        self.end_line = end_line;
        self.end_column = end_column;
        self
    }

    /// Set the parent scope ID.
    #[must_use]
    pub fn with_parent(mut self, parent_id: ScopeId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }
}
