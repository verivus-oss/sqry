//! Whitelist definitions for redaction presets.
//!
//! Each preset defines which fields are allowed to pass through redaction.
//! Fields not in the whitelist are redacted in whitelist security mode.

/// Code context fields - only preserved in `minimal` preset.
pub const CODE_CONTEXT_FIELDS: &[&str] = &[
    "context",
    "code_context",
    "codeContext",
    "snippet",
    "code_snippet",
    "codeSnippet",
    "source_code",
    "sourceCode",
    "code",
    "content",
];

/// Documentation fields - preserved in `minimal` and `standard` presets.
pub const DOCUMENTATION_FIELDS: &[&str] = &[
    "documentation",
    "doc",
    "docstring",
    "comment",
    "comments",
    "description",
];

/// Path-related fields that should be redacted.
pub const PATH_FIELDS: &[&str] = &[
    "file_uri",
    "fileUri",
    "uri",
    "url",
    "path",
    "file_path",
    "filePath",
    "absolute_path",
    "absolutePath",
    "source",
    "target",
    "src",
    "dst",
];

/// Workspace path fields that should be redacted.
pub const WORKSPACE_FIELDS: &[&str] = &[
    "workspace_path",
    "workspacePath",
    "root",
    "rootPath",
    "root_path",
    "projectRoot",
    "project_root",
];

/// Minimal preset whitelist - most permissive, allows code and docs.
///
/// Use when code context is needed by the recipient but paths should be hidden.
pub const WHITELIST_MINIMAL: &[&str] = &[
    // Semantic information
    "name",
    "qualified_name",
    "qualifiedName",
    "kind",
    "symbol_kind",
    "symbolKind",
    "language",
    "lang",
    // Position information
    "range",
    "start",
    "end",
    "line",
    "column",
    "offset",
    "start_line",
    "end_line",
    "startLine",
    "endLine",
    "start_column",
    "end_column",
    "startColumn",
    "endColumn",
    // Graph structure
    "edges",
    "nodes",
    "relation_type",
    "relationType",
    "score",
    "confidence",
    "relevance",
    // Code context (PRESERVED in minimal)
    "context",
    "code_context",
    "codeContext",
    "snippet",
    "code_snippet",
    "codeSnippet",
    "source_code",
    "sourceCode",
    "code",
    "content",
    // Documentation (PRESERVED in minimal)
    "documentation",
    "doc",
    "docstring",
    "comment",
    "comments",
    "description",
    // Other allowed fields
    "signature",
    "visibility",
    "is_public",
    "isPublic",
    // Structural fields that contain nested data
    "results",
    "items",
    "data",
    "symbols",
    "references",
    "definitions",
    "from",
    "to",
    "caller",
    "callee",
    "location",
];

/// Standard preset whitelist - code context excluded, docs preserved.
///
/// Recommended default for most cloud integrations.
pub const WHITELIST_STANDARD: &[&str] = &[
    // Semantic information
    "name",
    "qualified_name",
    "qualifiedName",
    "kind",
    "symbol_kind",
    "symbolKind",
    "language",
    "lang",
    // Position information
    "range",
    "start",
    "end",
    "line",
    "column",
    "offset",
    "start_line",
    "end_line",
    "startLine",
    "endLine",
    "start_column",
    "end_column",
    "startColumn",
    "endColumn",
    // Graph structure
    "edges",
    "nodes",
    "relation_type",
    "relationType",
    "score",
    "confidence",
    "relevance",
    // Documentation (preserved for semantic context)
    "documentation",
    "doc",
    "docstring",
    "comment",
    "comments",
    "description",
    // Other allowed fields
    "signature",
    "visibility",
    "is_public",
    "isPublic",
    // Structural fields that contain nested data
    "results",
    "items",
    "data",
    "symbols",
    "references",
    "definitions",
    "from",
    "to",
    "caller",
    "callee",
    "location",
    // NOTE: code context fields NOT included (redacted)
];

/// Strict preset whitelist - most restrictive, minimum information exposure.
///
/// Use for untrusted external services where even file structure should be hidden.
pub const WHITELIST_STRICT: &[&str] = &[
    // Minimal semantic information
    "name",
    "kind",
    "language",
    // Minimal position information
    "line",
    "column",
    // Minimal structure
    "relation_type",
    "relationType",
    "score",
    "confidence",
    // Structural fields (minimal)
    "results",
    "items",
    "edges",
    "nodes",
    "from",
    "to",
    // NOTE: No code, docs, signatures, or detailed position ranges
];

/// Check if a field name is a path-related field.
#[inline]
pub fn is_path_field(field: &str) -> bool {
    PATH_FIELDS.contains(&field)
}

/// Check if a field name is a workspace-related field.
#[inline]
pub fn is_workspace_field(field: &str) -> bool {
    WORKSPACE_FIELDS.contains(&field)
}

/// Check if a field name is a code context field.
#[inline]
pub fn is_code_context_field(field: &str) -> bool {
    CODE_CONTEXT_FIELDS.contains(&field)
}

/// Check if a field name is a documentation field.
#[inline]
pub fn is_documentation_field(field: &str) -> bool {
    DOCUMENTATION_FIELDS.contains(&field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_field_detection() {
        assert!(is_path_field("fileUri"));
        assert!(is_path_field("path"));
        assert!(is_path_field("file_path"));
        assert!(!is_path_field("name"));
        assert!(!is_path_field("kind"));
    }

    #[test]
    fn test_workspace_field_detection() {
        assert!(is_workspace_field("workspace_path"));
        assert!(is_workspace_field("workspacePath"));
        assert!(is_workspace_field("projectRoot"));
        assert!(!is_workspace_field("path"));
        assert!(!is_workspace_field("name"));
    }

    #[test]
    fn test_code_context_field_detection() {
        assert!(is_code_context_field("code_context"));
        assert!(is_code_context_field("snippet"));
        assert!(is_code_context_field("code"));
        assert!(!is_code_context_field("name"));
    }

    #[test]
    fn test_documentation_field_detection() {
        assert!(is_documentation_field("documentation"));
        assert!(is_documentation_field("doc"));
        assert!(is_documentation_field("docstring"));
        assert!(!is_documentation_field("code"));
    }

    #[test]
    fn test_whitelist_hierarchy() {
        // minimal should be most permissive (largest)
        assert!(WHITELIST_MINIMAL.len() > WHITELIST_STANDARD.len());
        // standard should be larger than strict
        assert!(WHITELIST_STANDARD.len() > WHITELIST_STRICT.len());
    }

    #[test]
    fn test_minimal_contains_code_and_docs() {
        assert!(WHITELIST_MINIMAL.contains(&"code_context"));
        assert!(WHITELIST_MINIMAL.contains(&"documentation"));
    }

    #[test]
    fn test_standard_has_docs_but_not_code() {
        assert!(!WHITELIST_STANDARD.contains(&"code_context"));
        assert!(!WHITELIST_STANDARD.contains(&"code"));
        assert!(WHITELIST_STANDARD.contains(&"documentation"));
    }

    #[test]
    fn test_strict_has_neither_code_nor_docs() {
        assert!(!WHITELIST_STRICT.contains(&"code_context"));
        assert!(!WHITELIST_STRICT.contains(&"documentation"));
        assert!(!WHITELIST_STRICT.contains(&"signature"));
    }
}
