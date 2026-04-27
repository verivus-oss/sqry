//! Core redaction engine.
//!
//! The `Redactor` struct provides the main API for redacting MCP responses.

use serde_json::Value;
use std::io::{Read, Write};

use crate::jsonpath::CompiledJsonPath;
use crate::preview::RedactionPreview;
use crate::walker::WalkerContext;
use crate::{RedactionConfig, RedactionError};

/// Response redactor.
///
/// The main entry point for redacting MCP responses.
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::{Redactor, RedactionConfig};
///
/// let redactor = Redactor::with_defaults();
/// let mut response: serde_json::Value = serde_json::json!({
///     "fileUri": "file:///home/user/project/src/main.rs",
///     "name": "main"
/// });
///
/// let stats = redactor.redact(&mut response);
/// assert!(stats.uris_redacted > 0 || stats.paths_redacted > 0);
/// ```
#[derive(Debug)]
pub struct Redactor {
    /// Configuration.
    config: RedactionConfig,
    /// Compiled JSONPath expressions for redaction.
    redact_paths: Vec<CompiledJsonPath>,
    /// Compiled JSONPath expressions for preservation.
    preserve_paths: Vec<CompiledJsonPath>,
}

impl Redactor {
    /// Create a new redactor with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `Err` if:
    /// - Configuration validation fails (e.g., salt too long)
    /// - JSONPath expressions are invalid
    pub fn new(config: RedactionConfig) -> Result<Self, RedactionError> {
        // Validate configuration
        config.validate()?;

        // Compile JSONPath expressions
        let redact_paths = config
            .redact_paths
            .iter()
            .map(|s| CompiledJsonPath::parse(s))
            .collect::<Result<Vec<_>, _>>()?;

        let preserve_paths = config
            .preserve_paths
            .iter()
            .map(|s| CompiledJsonPath::parse(s))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            config,
            redact_paths,
            preserve_paths,
        })
    }

    /// Create a redactor with default (standard) configuration.
    ///
    /// This is equivalent to `Redactor::new(RedactionConfig::standard()).unwrap()`.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(RedactionConfig::standard()).expect("default config is valid")
    }

    /// Create a redactor from environment variables.
    ///
    /// # Errors
    ///
    /// Returns `Err` if configuration validation fails or JSONPath expressions are invalid.
    pub fn from_env() -> Result<Self, RedactionError> {
        Self::new(RedactionConfig::from_env())
    }

    /// Create a redactor bound to a [`crate::LogicalWorkspaceView`].
    ///
    /// This is the STEP_7 entry point: callers pass a view of their
    /// `sqry_core::workspace::LogicalWorkspace` (translated by the
    /// upstream `sqry-mcp` crate) so the path-redaction pipeline can
    /// emit the workspace-aware forms specified by acceptance criteria
    /// 3-7. Equivalent to:
    ///
    /// ```ignore
    /// let mut config = base_config;
    /// config.logical_workspace = Some(view);
    /// // aggregate_workspace_paths defaults to true when bound (criterion 8).
    /// Redactor::new(config)
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Err` if configuration validation fails or JSONPath
    /// expressions are invalid (same conditions as [`Self::new`]).
    pub fn with_logical_workspace(
        mut config: RedactionConfig,
        view: crate::config::LogicalWorkspaceView,
    ) -> Result<Self, RedactionError> {
        config.logical_workspace = Some(view);
        // aggregate_workspace_paths is already `true` by default — we
        // re-affirm it here for clarity (criterion 8).
        config.aggregate_workspace_paths = true;
        Self::new(config)
    }

    /// Redact a JSON-RPC response in place.
    ///
    /// Returns statistics about what was redacted.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_mcp_redaction::Redactor;
    ///
    /// let redactor = Redactor::with_defaults();
    /// let mut response = serde_json::json!({
    ///     "workspace_path": "/home/user/project"
    /// });
    ///
    /// let stats = redactor.redact(&mut response);
    /// assert!(stats.workspace_path_redacted);
    /// ```
    pub fn redact(&self, response: &mut Value) -> RedactionResult {
        let mut ctx = WalkerContext::new(&self.config, &self.redact_paths, &self.preserve_paths);
        crate::walker::walk_and_redact(response, &mut ctx);
        ctx.result
    }

    /// Redact and return a new JSON value (non-mutating).
    ///
    /// Returns a tuple of (redacted_value, stats).
    #[must_use]
    pub fn redact_clone(&self, response: &Value) -> (Value, RedactionResult) {
        let mut cloned = response.clone();
        let stats = self.redact(&mut cloned);
        (cloned, stats)
    }

    /// Redact a raw JSON string (parses, redacts, re-serializes).
    ///
    /// Convenience method for string input/output.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is not valid JSON.
    pub fn redact_str(&self, json_str: &str) -> Result<(String, RedactionResult), RedactionError> {
        let mut value: Value = serde_json::from_str(json_str)?;
        let stats = self.redact(&mut value);
        let output = serde_json::to_string(&value)?;
        Ok((output, stats))
    }

    /// Redact a raw JSON string with pretty formatting.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is not valid JSON.
    pub fn redact_str_pretty(
        &self,
        json_str: &str,
    ) -> Result<(String, RedactionResult), RedactionError> {
        let mut value: Value = serde_json::from_str(json_str)?;
        let stats = self.redact(&mut value);
        let output = serde_json::to_string_pretty(&value)?;
        Ok((output, stats))
    }

    /// Preview what would be redacted (dry-run mode).
    ///
    /// Does not modify input, returns detailed report.
    #[must_use]
    pub fn preview(&self, response: &Value) -> RedactionPreview {
        // Create a temporary config with dry_run enabled
        let mut dry_run_config = self.config.clone();
        dry_run_config.dry_run = true;

        let mut ctx = WalkerContext::new(&dry_run_config, &self.redact_paths, &self.preserve_paths);

        // Clone to avoid modifying the original
        let mut cloned = response.clone();
        crate::walker::walk_and_redact(&mut cloned, &mut ctx);

        // Build preview from context
        RedactionPreview {
            would_redact: ctx.preview_targets,
            would_preserve: ctx.preserved_paths,
            stats: ctx.result,
        }
    }

    /// Streaming redaction: reads from input, writes redacted output incrementally.
    ///
    /// Uses `serde_json::Deserializer` for pull-based parsing.
    ///
    /// # Note
    ///
    /// Current implementation parses the entire input before redacting.
    /// True incremental streaming is a future enhancement.
    ///
    /// # Errors
    ///
    /// Returns `Err` if reading, parsing, or writing fails.
    pub fn redact_stream<R: Read, W: Write>(
        &self,
        mut input: R,
        mut output: W,
    ) -> Result<RedactionResult, RedactionError> {
        // Read all input
        let mut input_str = String::new();
        input.read_to_string(&mut input_str)?;

        // Parse and redact
        let mut value: Value = serde_json::from_str(&input_str)?;
        let stats = self.redact(&mut value);

        // Write output
        let output_str = serde_json::to_string(&value)?;
        output.write_all(output_str.as_bytes())?;

        Ok(stats)
    }

    /// Get a reference to the configuration.
    #[must_use]
    pub fn config(&self) -> &RedactionConfig {
        &self.config
    }
}

/// Result of a redaction operation.
#[derive(Debug, Default, Clone)]
pub struct RedactionResult {
    /// Number of paths redacted.
    pub paths_redacted: usize,

    /// Number of URIs redacted.
    pub uris_redacted: usize,

    /// Number of code context blocks redacted.
    pub code_contexts_redacted: usize,

    /// Number of documentation strings redacted.
    pub docs_redacted: usize,

    /// Number of custom fields redacted.
    pub custom_fields_redacted: usize,

    /// Number of pattern-detected paths in strings.
    pub pattern_paths_redacted: usize,

    /// Whether workspace_path was redacted.
    pub workspace_path_redacted: bool,

    /// Number of unknown fields redacted (whitelist mode).
    pub unknown_fields_redacted: usize,

    /// Number of values redacted because they exceeded the maximum nesting depth.
    ///
    /// These values were replaced with the redaction placeholder to prevent
    /// stack overflow from deeply nested JSON structures.
    pub depth_limit_redacted: usize,

    /// Whether the walker hit the maximum depth limit at least once.
    pub depth_limit_reached: bool,
}

impl RedactionResult {
    /// Check if any redaction occurred.
    #[must_use]
    pub fn any_redacted(&self) -> bool {
        self.paths_redacted > 0
            || self.uris_redacted > 0
            || self.code_contexts_redacted > 0
            || self.docs_redacted > 0
            || self.custom_fields_redacted > 0
            || self.pattern_paths_redacted > 0
            || self.workspace_path_redacted
            || self.unknown_fields_redacted > 0
            || self.depth_limit_redacted > 0
    }

    /// Get the total count of redacted items.
    #[must_use]
    pub fn total_redacted(&self) -> usize {
        self.paths_redacted
            + self.uris_redacted
            + self.code_contexts_redacted
            + self.docs_redacted
            + self.custom_fields_redacted
            + self.pattern_paths_redacted
            + usize::from(self.workspace_path_redacted)
            + self.unknown_fields_redacted
            + self.depth_limit_redacted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_redactor_with_defaults() {
        let redactor = Redactor::with_defaults();
        assert!(redactor.config.redact_absolute_paths);
    }

    #[test]
    fn test_redact_workspace_path() {
        let redactor = Redactor::with_defaults();

        let mut response = json!({
            "data": {},
            "workspace_path": "/home/user/project"
        });

        let stats = redactor.redact(&mut response);
        assert!(stats.workspace_path_redacted);
        assert_eq!(response["workspace_path"], "<workspace>");
    }

    #[test]
    fn test_redact_file_uri() {
        let config = RedactionConfig {
            workspace_root: Some("/home/user/project".into()),
            ..RedactionConfig::standard()
        };
        let redactor = Redactor::new(config).unwrap();

        let mut response = json!({
            "fileUri": "file:///home/user/project/src/main.rs"
        });

        let stats = redactor.redact(&mut response);
        assert!(stats.uris_redacted > 0 || stats.paths_redacted > 0);

        let uri = response["fileUri"].as_str().unwrap();
        assert!(!uri.contains("/home/user"));
    }

    #[test]
    fn test_redact_clone() {
        let redactor = Redactor::with_defaults();

        let original = json!({
            "workspace_path": "/home/user/project"
        });

        let (redacted, stats) = redactor.redact_clone(&original);

        // Original unchanged
        assert_eq!(original["workspace_path"], "/home/user/project");
        // Copy redacted
        assert_eq!(redacted["workspace_path"], "<workspace>");
        assert!(stats.workspace_path_redacted);
    }

    #[test]
    fn test_redact_str() {
        let redactor = Redactor::with_defaults();

        let input = r#"{"workspace_path": "/home/user/project"}"#;
        let (output, stats) = redactor.redact_str(input).unwrap();

        assert!(output.contains("<workspace>"));
        assert!(stats.workspace_path_redacted);
    }

    #[test]
    fn test_preview() {
        let redactor = Redactor::with_defaults();

        let response = json!({
            "workspace_path": "/home/user/project",
            "name": "test"
        });

        let preview = redactor.preview(&response);
        assert!(preview.would_redact_anything());
        assert!(preview.stats.workspace_path_redacted);
    }

    #[test]
    fn test_redact_stream() {
        let redactor = Redactor::with_defaults();

        let input = br#"{"workspace_path": "/home/user/project"}"#;
        let mut output = Vec::new();

        let stats = redactor.redact_stream(&input[..], &mut output).unwrap();

        assert!(stats.workspace_path_redacted);
        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("<workspace>"));
    }

    #[test]
    fn test_none_preset_passthrough() {
        let redactor = Redactor::new(RedactionConfig::none()).unwrap();

        let mut response = json!({
            "fileUri": "file:///home/user/file.rs",
            "workspace_path": "/home/user/project",
            "code": "fn main() {}"
        });
        let original = response.clone();

        let stats = redactor.redact(&mut response);

        // Nothing should be redacted
        assert_eq!(response, original);
        assert!(!stats.any_redacted());
    }

    #[test]
    fn test_minimal_preset_preserves_code() {
        let redactor = Redactor::new(RedactionConfig::minimal()).unwrap();

        let mut response = json!({
            "code_context": "fn main() { println!(\"Hello\"); }"
        });

        let stats = redactor.redact(&mut response);

        // Code should be preserved in minimal mode
        assert_eq!(stats.code_contexts_redacted, 0);
        assert!(
            response["code_context"]
                .as_str()
                .unwrap()
                .contains("fn main")
        );
    }

    #[test]
    fn test_standard_preset_redacts_code() {
        let redactor = Redactor::new(RedactionConfig::standard()).unwrap();

        let mut response = json!({
            "code_context": "fn main() { println!(\"Hello\"); }"
        });

        let stats = redactor.redact(&mut response);

        // Code should be redacted in standard mode
        assert!(stats.code_contexts_redacted > 0);
        assert!(
            response["code_context"]
                .as_str()
                .unwrap()
                .contains("[REDACTED")
        );
    }

    #[test]
    fn test_invalid_jsonpath() {
        let config = RedactionConfig {
            redact_paths: vec!["$.results[?(@.kind=='file')]".to_string()],
            ..RedactionConfig::standard()
        };

        let result = Redactor::new(config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Filter predicates")
        );
    }

    #[test]
    fn test_custom_jsonpath_redaction() {
        let config = RedactionConfig {
            redact_paths: vec!["$.custom_secret".to_string()],
            ..RedactionConfig::minimal()
        };
        let redactor = Redactor::new(config).unwrap();

        let mut response = json!({
            "custom_secret": "super secret value",
            "name": "test"
        });

        let stats = redactor.redact(&mut response);

        assert!(stats.custom_fields_redacted > 0);
        assert_eq!(response["custom_secret"], "[REDACTED]");
        assert_eq!(response["name"], "test");
    }

    #[test]
    fn test_redaction_result_total() {
        let result = RedactionResult {
            paths_redacted: 2,
            uris_redacted: 1,
            code_contexts_redacted: 0,
            docs_redacted: 0,
            custom_fields_redacted: 1,
            pattern_paths_redacted: 0,
            workspace_path_redacted: true,
            unknown_fields_redacted: 0,
            depth_limit_redacted: 0,
            depth_limit_reached: false,
        };

        assert!(result.any_redacted());
        assert_eq!(result.total_redacted(), 5);
    }
}
