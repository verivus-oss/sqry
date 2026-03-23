//! JSON tree traversal for redaction.
//!
//! This module provides utilities for walking JSON structures and applying
//! redaction rules to matching fields.

use serde_json::{Map, Value};

use crate::RedactionConfig;
use crate::jsonpath::{CompiledJsonPath, PathComponent, path_to_string};
use crate::preview::{RedactionReason, RedactionTarget};
use crate::redactor::RedactionResult;
use crate::whitelist;

/// Context for JSON traversal.
pub struct WalkerContext<'a> {
    /// Current path from root.
    pub path: Vec<PathComponent>,
    /// Configuration.
    pub config: &'a RedactionConfig,
    /// Compiled JSONPath expressions for redaction.
    pub redact_paths: &'a [CompiledJsonPath],
    /// Compiled JSONPath expressions for preservation.
    pub preserve_paths: &'a [CompiledJsonPath],
    /// Redaction statistics.
    pub result: RedactionResult,
    /// Preview targets (for dry-run mode).
    pub preview_targets: Vec<RedactionTarget>,
    /// Preserved field paths (for dry-run mode).
    pub preserved_paths: Vec<String>,
}

impl<'a> WalkerContext<'a> {
    /// Create a new walker context.
    pub fn new(
        config: &'a RedactionConfig,
        redact_paths: &'a [CompiledJsonPath],
        preserve_paths: &'a [CompiledJsonPath],
    ) -> Self {
        Self {
            path: Vec::new(),
            config,
            redact_paths,
            preserve_paths,
            result: RedactionResult::default(),
            preview_targets: Vec::new(),
            preserved_paths: Vec::new(),
        }
    }

    /// Get the current path as a JSONPath string.
    pub fn current_path_string(&self) -> String {
        path_to_string(&self.path)
    }

    /// Check if a JSONPath expression matches the current path.
    pub fn matches_jsonpath(&self, paths: &[CompiledJsonPath]) -> bool {
        paths.iter().any(|p| p.matches(&self.path))
    }

    /// Push a field component onto the path.
    pub fn push_field(&mut self, name: &str) {
        self.path.push(PathComponent::Field(name.to_string()));
    }

    /// Push an index component onto the path.
    pub fn push_index(&mut self, index: usize) {
        self.path.push(PathComponent::Index(index));
    }

    /// Pop the last component from the path.
    pub fn pop(&mut self) {
        self.path.pop();
    }

    /// Record a preview target.
    pub fn record_preview(&mut self, original: &str, replacement: &str, reason: RedactionReason) {
        self.preview_targets.push(RedactionTarget {
            path: self.current_path_string(),
            original_preview: truncate_preview(original, 50),
            replacement: replacement.to_string(),
            reason,
        });
    }
}

/// Walk a JSON value and apply redaction.
///
/// This is the main entry point for JSON traversal.
pub fn walk_and_redact(value: &mut Value, ctx: &mut WalkerContext<'_>) {
    match value {
        Value::Object(map) => walk_object(map, ctx),
        Value::Array(arr) => walk_array(arr, ctx),
        Value::String(s) => handle_string_patterns(s, ctx),
        // Primitives don't need traversal
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn walk_object(map: &mut Map<String, Value>, ctx: &mut WalkerContext<'_>) {
    let keys = collect_object_keys(map);
    for key in keys {
        ctx.push_field(&key);
        handle_object_field(map, &key, ctx);
        ctx.pop();
    }
}

fn collect_object_keys(map: &Map<String, Value>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn handle_object_field(map: &mut Map<String, Value>, key: &str, ctx: &mut WalkerContext<'_>) {
    let Some(field_value) = map.get_mut(key) else {
        return;
    };

    if should_redact_field(key, ctx) {
        redact_value(key, field_value, ctx);
    } else if ctx.matches_jsonpath(ctx.preserve_paths) {
        // Preserved fields get full short-circuit: no traversal, no pattern detection.
        // This guarantees preserve_paths is a hard override — the value is untouched.
        if ctx.config.dry_run {
            ctx.preserved_paths.push(ctx.current_path_string());
        }
    } else {
        walk_and_redact(field_value, ctx);
    }
}

fn walk_array(arr: &mut [Value], ctx: &mut WalkerContext<'_>) {
    for (i, item) in arr.iter_mut().enumerate() {
        ctx.push_index(i);
        walk_and_redact(item, ctx);
        ctx.pop();
    }
}

fn handle_string_patterns(value: &mut String, ctx: &mut WalkerContext<'_>) {
    if !ctx.config.detect_paths_in_strings {
        return;
    }

    let (redacted, count) = crate::rules::pattern::redact_paths_in_string(
        value,
        ctx.config.workspace_root.as_ref().and_then(|p| p.to_str()),
        &ctx.config.workspace_placeholder,
        ctx.config.hash_filenames,
        ctx.config.normalized_salt(),
    );
    if count == 0 {
        return;
    }

    ctx.result.pattern_paths_redacted += count;
    if ctx.config.dry_run {
        ctx.record_preview(value, &redacted, RedactionReason::PatternMatch);
    } else {
        *value = redacted;
    }
}

/// Determine if a field should be redacted.
fn should_redact_field(field_name: &str, ctx: &WalkerContext<'_>) -> bool {
    let config = ctx.config;

    // Step 1: Security mode check - passthrough mode skips everything
    if is_passthrough(config) {
        return false;
    }

    // Step 2: Preserve paths override - explicitly preserved fields are never redacted
    if ctx.matches_jsonpath(ctx.preserve_paths) {
        return false;
    }

    // Step 3: Check specific field types that are always redacted when their toggle is on
    // These are checked BEFORE whitelist because the intent is to redact sensitive field types
    if should_redact_by_field_type(field_name, config) {
        return true;
    }

    // Step 4: Check custom redaction via JSONPath
    if ctx.matches_jsonpath(ctx.redact_paths) {
        return true;
    }

    // Step 5: Custom field list (explicit blacklist)
    if config.is_custom_redacted(field_name) {
        return true;
    }

    // Step 6: Whitelist check - if in whitelist mode and field is NOT whitelisted,
    // we don't redact the field value directly, but we continue traversal into objects/arrays
    // (this allows nested fields to be checked)
    // If a field IS whitelisted, it's allowed through unchanged

    false
}

fn is_passthrough(config: &RedactionConfig) -> bool {
    matches!(config.security_mode, crate::SecurityMode::Passthrough)
}

fn should_redact_by_field_type(field_name: &str, config: &RedactionConfig) -> bool {
    if config.redact_workspace_path && whitelist::is_workspace_field(field_name) {
        return true;
    }

    if config.redact_absolute_paths && whitelist::is_path_field(field_name) {
        return true;
    }

    if config.redact_code_context && whitelist::is_code_context_field(field_name) {
        return true;
    }

    if config.redact_documentation && whitelist::is_documentation_field(field_name) {
        return true;
    }

    false
}

/// Redact a value based on its type and the field name.
fn redact_value(field_name: &str, value: &mut Value, ctx: &mut WalkerContext<'_>) {
    match value {
        Value::String(s) => {
            redact_string_value(field_name, s, ctx);
        }
        Value::Object(_) => {
            // For objects (like nested location/context), redact recursively
            // or replace with placeholder depending on field type
            redact_object_value(field_name, value, ctx);
        }
        Value::Array(arr) => {
            walk_array(arr, ctx);
        }
        // Null, bool, number - replace with placeholder string
        _ => {
            redact_primitive_value(value, ctx);
        }
    }
}

fn redact_string_value(field_name: &str, value: &mut String, ctx: &mut WalkerContext<'_>) {
    let (redacted, reason) = redact_string_field(field_name, value, ctx);
    if ctx.config.dry_run {
        ctx.record_preview(value, &redacted, reason);
    } else {
        *value = redacted;
    }
    update_stats(&reason, ctx);
}

fn redact_object_value(field_name: &str, value: &mut Value, ctx: &mut WalkerContext<'_>) {
    if whitelist::is_code_context_field(field_name) {
        let placeholder = crate::rules::redact_code_context(
            &serde_json::to_string(value).unwrap_or_default(),
            &ctx.config.redacted_placeholder,
        );
        if ctx.config.dry_run {
            ctx.record_preview("{object}", &placeholder, RedactionReason::CodeContext);
        } else {
            *value = Value::String(placeholder);
        }
        ctx.result.code_contexts_redacted += 1;
    } else {
        let Value::Object(map) = value else {
            return;
        };
        walk_object(map, ctx);
    }
}

fn redact_primitive_value(value: &mut Value, ctx: &mut WalkerContext<'_>) {
    if ctx.config.dry_run {
        ctx.record_preview(
            &value.to_string(),
            &ctx.config.redacted_placeholder,
            RedactionReason::CustomField,
        );
    } else {
        *value = Value::String(ctx.config.redacted_placeholder.clone());
    }
    ctx.result.custom_fields_redacted += 1;
}

/// Redact a string field based on the field name.
fn redact_string_field(
    field_name: &str,
    content: &str,
    ctx: &WalkerContext<'_>,
) -> (String, RedactionReason) {
    let config = ctx.config;

    if let Some(result) = redact_workspace_field(field_name, config) {
        return result;
    }

    if let Some(result) = redact_path_field(field_name, content, config) {
        return result;
    }

    if let Some(result) = redact_code_context_field(field_name, content, config) {
        return result;
    }

    if let Some(result) = redact_documentation_field(field_name, content, config) {
        return result;
    }

    // Custom field or unknown
    (
        config.redacted_placeholder.clone(),
        RedactionReason::CustomField,
    )
}

fn redact_workspace_field(
    field_name: &str,
    config: &RedactionConfig,
) -> Option<(String, RedactionReason)> {
    if whitelist::is_workspace_field(field_name) {
        Some((
            config.workspace_placeholder.clone(),
            RedactionReason::WorkspacePath,
        ))
    } else {
        None
    }
}

fn redact_path_field(
    field_name: &str,
    content: &str,
    config: &RedactionConfig,
) -> Option<(String, RedactionReason)> {
    if !whitelist::is_path_field(field_name) {
        return None;
    }

    let result = crate::rules::path::redact_path(
        content,
        config.workspace_root.as_ref().and_then(|p| p.to_str()),
        &config.workspace_placeholder,
        config.hash_filenames,
        config.normalized_salt(),
    )
    .unwrap_or_else(|_| config.workspace_placeholder.clone());

    let reason = if crate::rules::uri::is_file_uri(content) {
        RedactionReason::FileUri
    } else {
        RedactionReason::AbsolutePath
    };

    Some((result, reason))
}

fn redact_code_context_field(
    field_name: &str,
    content: &str,
    config: &RedactionConfig,
) -> Option<(String, RedactionReason)> {
    if whitelist::is_code_context_field(field_name) {
        let redacted = crate::rules::redact_code_context(content, &config.redacted_placeholder);
        Some((redacted, RedactionReason::CodeContext))
    } else {
        None
    }
}

fn redact_documentation_field(
    field_name: &str,
    content: &str,
    config: &RedactionConfig,
) -> Option<(String, RedactionReason)> {
    if whitelist::is_documentation_field(field_name) {
        let redacted = crate::rules::redact_documentation(content, &config.redacted_placeholder);
        Some((redacted, RedactionReason::Documentation))
    } else {
        None
    }
}

/// Update statistics based on redaction reason.
fn update_stats(reason: &RedactionReason, ctx: &mut WalkerContext<'_>) {
    match reason {
        RedactionReason::AbsolutePath => ctx.result.paths_redacted += 1,
        RedactionReason::FileUri => ctx.result.uris_redacted += 1,
        RedactionReason::WorkspacePath => ctx.result.workspace_path_redacted = true,
        RedactionReason::CodeContext => ctx.result.code_contexts_redacted += 1,
        RedactionReason::Documentation => ctx.result.docs_redacted += 1,
        RedactionReason::CustomField => ctx.result.custom_fields_redacted += 1,
        RedactionReason::PatternMatch => {} // Handled separately
        RedactionReason::UnknownField => ctx.result.unknown_fields_redacted += 1,
    }
}

/// Truncate a string for preview display.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_walk_simple_object() {
        let config = RedactionConfig::standard();
        let redact_paths = vec![];
        let preserve_paths = vec![];

        let mut value = json!({
            "name": "test",
            "workspace_path": "/home/user/project"
        });

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        assert_eq!(value["name"], "test");
        assert_eq!(value["workspace_path"], "<workspace>");
    }

    #[test]
    fn test_walk_nested_array() {
        let config = RedactionConfig::standard();
        let redact_paths = vec![];
        let preserve_paths = vec![];

        let mut value = json!({
            "results": [
                {"fileUri": "file:///home/user/a.rs"},
                {"fileUri": "file:///home/user/b.rs"}
            ]
        });

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Paths should be redacted
        let uri0 = value["results"][0]["fileUri"].as_str().unwrap();
        assert!(!uri0.contains("/home/user"));
    }

    #[test]
    fn test_walk_with_jsonpath() {
        let config = RedactionConfig::minimal(); // Code not redacted by default
        let redact_paths = vec![CompiledJsonPath::parse("$.custom_field").unwrap()];
        let preserve_paths = vec![];

        let mut value = json!({
            "name": "test",
            "custom_field": "secret value"
        });

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        assert_eq!(value["name"], "test");
        assert_eq!(value["custom_field"], "[REDACTED]");
    }

    #[test]
    fn test_dry_run_mode() {
        let mut config = RedactionConfig::standard();
        config.dry_run = true;

        let redact_paths = vec![];
        let preserve_paths = vec![];

        let mut value = json!({
            "workspace_path": "/home/user/project"
        });
        let original = value.clone();

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Value should be unchanged
        assert_eq!(value, original);
        // But we should have preview targets
        assert!(!ctx.preview_targets.is_empty());
    }

    #[test]
    fn test_passthrough_mode() {
        let config = RedactionConfig::none();
        let redact_paths = vec![];
        let preserve_paths = vec![];

        let mut value = json!({
            "fileUri": "file:///home/user/file.rs",
            "workspace_path": "/home/user/project",
            "code": "fn main() {}"
        });
        let original = value.clone();

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Nothing should be changed
        assert_eq!(value, original);
    }

    #[test]
    fn test_pattern_detection() {
        let mut config = RedactionConfig::minimal();
        config.detect_paths_in_strings = true;
        config.workspace_root = Some("/home/user/project".into());

        let redact_paths = vec![];
        let preserve_paths = vec![];

        let mut value = json!({
            "message": "Error in /home/user/project/src/main.rs at line 42"
        });

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        let message = value["message"].as_str().unwrap();
        assert!(!message.contains("/home/user/project"));
        assert!(message.contains("src/main.rs"));
    }

    #[test]
    fn test_preserve_paths_override_redaction() {
        let config = RedactionConfig::standard();
        let redact_paths = vec![CompiledJsonPath::parse("$.secret").unwrap()];
        // Preserve the secret field — should override redaction
        let preserve_paths = vec![CompiledJsonPath::parse("$.secret").unwrap()];

        let mut value = json!({
            "name": "test",
            "secret": "keep-this-value",
            "workspace_path": "/home/user/project"
        });

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Secret should be preserved (preserve overrides redact)
        assert_eq!(value["secret"], "keep-this-value");
        // workspace_path should still be redacted (not in preserve list)
        assert_ne!(value["workspace_path"], "/home/user/project");
    }

    #[test]
    fn test_dry_run_tracks_preserved_fields() {
        let mut config = RedactionConfig::standard();
        config.dry_run = true;

        let redact_paths = vec![CompiledJsonPath::parse("$.secret").unwrap()];
        let preserve_paths = vec![CompiledJsonPath::parse("$.secret").unwrap()];

        let mut value = json!({
            "name": "test",
            "secret": "keep-this-value"
        });
        let original = value.clone();

        let mut ctx = WalkerContext::new(&config, &redact_paths, &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Dry run should not modify value
        assert_eq!(value, original);
        // Should track the preserved field
        assert!(
            ctx.preserved_paths.contains(&"$.secret".to_string()),
            "Preserved paths should contain $.secret, got: {:?}",
            ctx.preserved_paths
        );
    }

    #[test]
    fn test_preserve_paths_blocks_string_pattern_detection() {
        // Regression: preserved string fields containing detectable file paths
        // must NOT be modified by pattern detection
        let mut config = RedactionConfig::standard();
        config.detect_paths_in_strings = true;
        config.workspace_root = Some("/home/user/project".into());

        let preserve_paths = vec![CompiledJsonPath::parse("$.safe_field").unwrap()];

        let mut value = json!({
            "safe_field": "The file is at /home/user/project/src/main.rs",
            "unsafe_field": "The file is at /home/user/project/src/lib.rs"
        });

        let mut ctx = WalkerContext::new(&config, &[], &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // safe_field should be completely untouched — no pattern detection
        assert_eq!(
            value["safe_field"], "The file is at /home/user/project/src/main.rs",
            "Preserved field should not have path patterns redacted"
        );
        // unsafe_field should have its path redacted
        assert_ne!(
            value["unsafe_field"], "The file is at /home/user/project/src/lib.rs",
            "Non-preserved field should have path patterns redacted"
        );
    }

    #[test]
    fn test_preserve_paths_blocks_nested_child_redaction() {
        // Regression: preserved object field with sensitive nested children
        // must NOT have children traversed or redacted
        let config = RedactionConfig::standard();

        let preserve_paths = vec![CompiledJsonPath::parse("$.protected").unwrap()];

        let mut value = json!({
            "protected": {
                "workspace_path": "/home/user/project",
                "code_context": "fn main() { secret(); }",
                "fileUri": "file:///home/user/project/src/main.rs"
            },
            "unprotected": {
                "workspace_path": "/home/user/other"
            }
        });

        let mut ctx = WalkerContext::new(&config, &[], &preserve_paths);
        walk_and_redact(&mut value, &mut ctx);

        // Everything under "protected" should be untouched
        assert_eq!(
            value["protected"]["workspace_path"], "/home/user/project",
            "Nested workspace_path under preserved field should be untouched"
        );
        assert_eq!(
            value["protected"]["code_context"], "fn main() { secret(); }",
            "Nested code_context under preserved field should be untouched"
        );
        assert_eq!(
            value["protected"]["fileUri"], "file:///home/user/project/src/main.rs",
            "Nested fileUri under preserved field should be untouched"
        );

        // "unprotected" should still be redacted normally
        assert_ne!(
            value["unprotected"]["workspace_path"], "/home/user/other",
            "Non-preserved nested field should still be redacted"
        );
    }
}
