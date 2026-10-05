//! JSON tree traversal for redaction.
//!
//! This module provides utilities for walking JSON structures and applying
//! redaction rules to matching fields.

use serde_json::{Map, Value};

use crate::RedactionConfig;
use crate::jsonpath::{CompiledJsonPath, PathComponent, path_to_string};
use crate::preview::{RedactionReason, RedactionTarget};
use crate::redactor::RedactionResult;
use crate::rules::path::ContextualPath;
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
    /// The path or workspace key whose array is being walked: a string
    /// reached from that key through arrays alone (no object key between)
    /// is a value of that key ([`walk_keyed_array`]).
    array_key: Option<String>,
    /// Current recursion depth.
    depth: usize,
    /// Maximum allowed recursion depth.
    max_depth: usize,
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
            array_key: None,
            depth: 0,
            max_depth: config.max_depth,
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
///
/// Recursion is bounded by [`redaction_max_depth`](crate::config::redaction_max_depth)
/// (default 128, configurable via `SQRY_REDACTION_MAX_DEPTH`). Values nested beyond
/// the limit are redacted (fail-closed) to prevent both stack overflow and data leakage.
pub fn walk_and_redact(value: &mut Value, ctx: &mut WalkerContext<'_>) {
    if ctx.depth >= ctx.max_depth {
        ctx.result.depth_limit_reached = true;

        // Fail closed: redact the current node instead of leaving it unmodified.
        // We deliberately avoid `value.to_string()` here because serializing a
        // deeply nested subtree would recurse just as deeply as the walker itself.
        if !value.is_string() || value.as_str() != Some(&ctx.config.redacted_placeholder) {
            if ctx.config.dry_run {
                ctx.record_preview(
                    "[depth limit exceeded]",
                    &ctx.config.redacted_placeholder,
                    RedactionReason::DepthLimitExceeded,
                );
            } else {
                *value = Value::String(ctx.config.redacted_placeholder.clone());
            }
            ctx.result.depth_limit_redacted += 1;
        }

        return;
    }
    ctx.depth += 1;
    match value {
        Value::Object(map) => walk_object(map, ctx),
        Value::Array(arr) => walk_array(arr, ctx),
        Value::String(s) => match ctx.array_key.clone() {
            Some(key) => handle_keyed_string(&key, value, ctx),
            None => handle_string_patterns(s, ctx),
        },
        // Primitives don't need traversal
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    ctx.depth -= 1;
}

fn walk_object(map: &mut Map<String, Value>, ctx: &mut WalkerContext<'_>) {
    // An object's own keys decide how its values are read, so a key whose
    // array holds this object no longer applies inside it.
    let array_key = ctx.array_key.take();
    let keys = collect_object_keys(map);
    for key in keys {
        ctx.push_field(&key);
        handle_object_field(map, &key, ctx);
        ctx.pop();
    }
    ctx.array_key = array_key;
}

/// Walk the array under the path or workspace key `field_name` so that
/// each string in it, at any depth of nested arrays, is read as a value of
/// that key ([`handle_keyed_string`]): a list of paths is redacted as each
/// of its paths would be. An object in the array is walked by its own keys.
/// Every item goes through [`walk_and_redact`], so the depth guard applies.
fn walk_keyed_array(field_name: &str, arr: &mut [Value], ctx: &mut WalkerContext<'_>) {
    let outer = ctx.array_key.replace(field_name.to_string());
    for (i, item) in arr.iter_mut().enumerate() {
        ctx.push_index(i);
        walk_and_redact(item, ctx);
        ctx.pop();
    }
    ctx.array_key = outer;
}

/// A string reached from the path or workspace key `field_name` through
/// arrays alone. Under passthrough it is checked against a bound
/// workspace's exclusions as the key's own string would be; otherwise it is
/// redacted as the key's own string would be when it is a path
/// ([`path_value`]), and scanned like any other string when it is not.
fn handle_keyed_string(field_name: &str, value: &mut Value, ctx: &mut WalkerContext<'_>) {
    if is_passthrough(ctx.config) {
        if passthrough_exclusion_applies(field_name, value, ctx) {
            redact_excluded_in_passthrough(field_name, value, ctx);
        }
        return;
    }
    let Value::String(text) = value else {
        return;
    };
    if path_value(field_name, text).is_some() {
        redact_string_value(field_name, text, ctx);
    } else {
        handle_string_patterns(text, ctx);
    }
}

fn collect_object_keys(map: &Map<String, Value>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn handle_object_field(map: &mut Map<String, Value>, key: &str, ctx: &mut WalkerContext<'_>) {
    let Some(field_value) = map.get_mut(key) else {
        return;
    };

    if should_redact_field(key, field_value, ctx) {
        redact_value(key, field_value, ctx);
    } else if passthrough_exclusion_applies(key, field_value, ctx) {
        // Passthrough mode is normally a no-op (`should_redact_field` short-
        // circuits on `is_passthrough`), but `STEP_7` acceptance criterion 6
        // (preset=any + path in exclusions → opaque hash + excluded: true)
        // requires excluded paths to be redacted regardless of preset. When
        // a `LogicalWorkspaceView` is bound and the field is path-bearing,
        // route through `redact_excluded_in_passthrough` which rewrites
        // only excluded paths to the opaque-hash form and leaves every
        // other path (including absolute non-excluded paths) verbatim.
        // This preserves criterion 3 (preset=none + path inside source_root
        // → absolute emitted) while closing the criterion-6 gap that the
        // codex iter1 BLOCK called out.
        redact_excluded_in_passthrough(key, field_value, ctx);
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

/// `STEP_7` criterion 6 hook: `true` iff we are in passthrough mode but
/// the operator bound a `LogicalWorkspaceView` AND the current field is
/// path-bearing and its value can lie under an exclusion. Drives the
/// exclusions-override-passthrough branch in [`handle_object_field`].
///
/// An exclusion names a place in the workspace, so only a value that names
/// a place can fall under one: an object or an array (walked), or a string
/// that [`path_value`] reads as [`PathValue::Whole`] (any non-empty string
/// under a key that always names a path, and exactly one absolute path
/// under a contextual key). A contextual value that is not a path, or that
/// holds a path but names no place ([`PathValue::Unplaced`]: a relative
/// path, prose), is left as it is, as passthrough leaves every other value.
fn passthrough_exclusion_applies(field_name: &str, value: &Value, ctx: &WalkerContext<'_>) -> bool {
    let config = ctx.config;
    if !is_passthrough(config) {
        return false;
    }
    if config.logical_workspace.is_none() {
        return false;
    }
    if !(whitelist::is_path_field(field_name) || whitelist::is_workspace_field(field_name)) {
        return false;
    }
    match value {
        Value::String(text) => matches!(path_value(field_name, text), Some(PathValue::Whole(_))),
        Value::Object(_) | Value::Array(_) => true,
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// How the string under a path or workspace key is a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathValue<'a> {
    /// The text is one path, redacted through the path pipeline as written
    /// here (relative to the workspace when it lies inside it).
    Whole(&'a str),
    /// The value holds a path but names no place: redacted as a path
    /// outside the workspace ([`crate::rules::path::redact_unanchored_path`]).
    Unplaced,
}

/// Whether, and how, the string `text` under the path or workspace key
/// `field_name` is a path. Redaction changes only paths, so it never
/// changes a name, an enumerated value or a type:
///
/// - under a key that always names a path (`path`, `file_path`, `fileUri`,
///   `root`, ...) any non-empty string is one whole path, as written; an
///   empty string names nothing;
/// - under a contextual key ([`whitelist::CONTEXTUAL_PATH_FIELDS`]:
///   `source`, `target`, `src`, `dst`, `uri`, `url`) the value is read by
///   [`crate::rules::path::classify_contextual_value`]: a value with no
///   path separator, or one URL of a scheme other than `file`, is not a
///   path; exactly one absolute path is a whole path (its surrounding
///   whitespace trimmed); any other value with a separator (a relative,
///   home or drive-relative path, a path after a prefix or with whitespace
///   in it, prose holding a path) is a path that names no place.
fn path_value<'a>(field_name: &str, text: &'a str) -> Option<PathValue<'a>> {
    if !whitelist::is_contextual_path_field(field_name) {
        return (!text.is_empty()).then_some(PathValue::Whole(text));
    }
    match crate::rules::path::classify_contextual_value(text) {
        ContextualPath::NotAPath => None,
        ContextualPath::Anchored(trimmed) => Some(PathValue::Whole(trimmed)),
        ContextualPath::Unanchored => Some(PathValue::Unplaced),
    }
}

/// Whether the value of a path or workspace field is a path the redactor
/// may rewrite:
///
/// - a string is a path when [`path_value`] says it is;
/// - an object or an array is walked: an object by its own keys, and each
///   string in an array, through nested arrays, as a value of this key
///   ([`walk_keyed_array`]);
/// - a null, boolean or number is never a path.
///
/// A contextual value that is not a path is walked as an ordinary string,
/// so in-string detection
/// ([`crate::rules::pattern::detect_paths_in_string`]) still runs on it: a
/// value with no separator holds nothing that detection can match, and
/// detection keeps the own path of a URL of a scheme other than `file`
/// (it still redacts a host path in the URL's query or parameters).
fn carries_path(field_name: &str, value: &Value) -> bool {
    match value {
        Value::String(text) => path_value(field_name, text).is_some(),
        Value::Object(_) | Value::Array(_) => true,
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Rewrite a path-bearing string field under passthrough mode when a
/// bound `LogicalWorkspaceView` flags it as excluded. Non-excluded
/// values are left untouched (criterion 3). Object/array shaped values
/// recurse so nested path-bearing fields get the same treatment.
///
/// [`passthrough_exclusion_applies`] decides whether a value is checked at
/// all; this only picks the text to check: the trimmed path of a contextual
/// value that is exactly one absolute path, otherwise the string as it is.
fn redact_excluded_in_passthrough(
    field_name: &str,
    value: &mut Value,
    ctx: &mut WalkerContext<'_>,
) {
    match value {
        Value::String(s) => {
            let Some(view) = ctx.config.logical_workspace.as_ref() else {
                return;
            };
            let text = match path_value(field_name, s) {
                Some(PathValue::Whole(text)) => text,
                Some(PathValue::Unplaced) | None => s.as_str(),
            };
            let outcome = crate::rules::path::redact_path_with_workspace(
                text,
                view,
                &ctx.config.workspace_placeholder,
                ctx.config.hash_filenames,
                ctx.config.normalized_salt(),
                ctx.config.aggregate_workspace_paths,
                ctx.config.workspace_root.as_ref().and_then(|p| p.to_str()),
                ctx.config.reveal_workspace_relative_layout,
            );
            let Ok(redacted_path) = outcome else {
                return;
            };
            if !redacted_path.excluded {
                // Non-excluded paths flow through unchanged — passthrough
                // semantics for criterion 3.
                return;
            }
            ctx.result.paths_redacted += 1;
            if ctx.config.dry_run {
                ctx.record_preview(s, &redacted_path.rendered, RedactionReason::AbsolutePath);
            } else {
                *value = Value::String(redacted_path.rendered);
            }
        }
        Value::Object(_) => {
            // Path-bearing object values (e.g. `{ "fileUri": "...",
            // "range": {...} }`) recurse so nested string fields receive
            // the same passthrough-exclusion treatment. We rely on the
            // walker's normal traversal to re-enter `handle_object_field`
            // for descendant string values; no preset escalation occurs.
            walk_and_redact(value, ctx);
        }
        Value::Array(arr) => {
            // A list under the key: each string in it is checked as the
            // key's own string would be.
            if ctx.depth >= ctx.max_depth {
                walk_and_redact(value, ctx);
            } else {
                ctx.depth += 1;
                walk_keyed_array(field_name, arr, ctx);
                ctx.depth -= 1;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
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
fn should_redact_field(field_name: &str, value: &Value, ctx: &WalkerContext<'_>) -> bool {
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
    if should_redact_by_field_type(field_name, value, config) {
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

fn should_redact_by_field_type(field_name: &str, value: &Value, config: &RedactionConfig) -> bool {
    if config.redact_workspace_path
        && whitelist::is_workspace_field(field_name)
        && carries_path(field_name, value)
    {
        return true;
    }

    if config.redact_absolute_paths
        && whitelist::is_path_field(field_name)
        && carries_path(field_name, value)
    {
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
            // Each item goes through walk_and_redact, so the depth guard
            // applies; a string in the list under a path or workspace key is
            // read as a value of that key.
            if whitelist::is_path_field(field_name) || whitelist::is_workspace_field(field_name) {
                walk_keyed_array(field_name, arr, ctx);
            } else {
                for (i, item) in arr.iter_mut().enumerate() {
                    ctx.push_index(i);
                    walk_and_redact(item, ctx);
                    ctx.pop();
                }
            }
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
        // Route through walk_and_redact to ensure the depth guard is applied.
        walk_and_redact(value, ctx);
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

    let (result, path) = match path_value(field_name, content) {
        Some(PathValue::Whole(path)) => (redact_path_for_config(path, config), path),
        Some(PathValue::Unplaced) => (
            crate::rules::path::redact_unanchored_path(
                content,
                config.hash_filenames,
                config.normalized_salt(),
            ),
            content,
        ),
        // Not a path, yet redacted: a custom field list or a JSONPath named
        // this field, so the whole value goes through the path pipeline.
        None => (redact_path_for_config(content, config), content),
    };

    let reason = if crate::rules::uri::is_file_uri(path) {
        RedactionReason::FileUri
    } else {
        RedactionReason::AbsolutePath
    };

    Some((result, reason))
}

/// Run the path-redaction pipeline that the walker uses for any field
/// classified as a path field. When a [`crate::config::LogicalWorkspaceView`]
/// is bound on the config we route through
/// [`crate::rules::path::redact_path_with_workspace`] so STEP_7
/// acceptance criteria 3-7 apply; otherwise we fall back to the legacy
/// single-workspace pipeline.
///
/// The `excluded` flag returned by the workspace-aware path is folded
/// into the rendered string as a sibling-free wire form
/// (`<excluded>/[hash]`); the legacy walker model does not carry
/// per-field metadata and folding the flag into the rendered string
/// keeps every consumer (JSON walker, pattern-detect string fixup,
/// streaming redactor) on the same code path.
fn redact_path_for_config(content: &str, config: &RedactionConfig) -> String {
    if let Some(view) = config.logical_workspace.as_ref() {
        return match crate::rules::path::redact_path_with_workspace(
            content,
            view,
            &config.workspace_placeholder,
            config.hash_filenames,
            config.normalized_salt(),
            config.aggregate_workspace_paths,
            config.workspace_root.as_ref().and_then(|p| p.to_str()),
            config.reveal_workspace_relative_layout,
        ) {
            Ok(r) => r.rendered,
            Err(_) => config.workspace_placeholder.clone(),
        };
    }

    crate::rules::path::redact_path(
        content,
        config.workspace_root.as_ref().and_then(|p| p.to_str()),
        &config.workspace_placeholder,
        config.hash_filenames,
        config.normalized_salt(),
    )
    .unwrap_or_else(|_| config.workspace_placeholder.clone())
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
        // PatternMatch is counted in handle_string_patterns; DepthLimitExceeded
        // is counted directly in the walk_and_redact depth guard.
        RedactionReason::PatternMatch | RedactionReason::DepthLimitExceeded => {}
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
    use crate::config::DEFAULT_REDACTION_MAX_DEPTH;
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

    // --- Depth limit tests (stack overflow prevention) ---

    /// Build a deeply nested JSON object: `{"a": {"a": {"a": ... {"workspace_path": "/secret"}}}}`.
    fn build_nested_object(depth: usize) -> Value {
        let mut value = json!({"workspace_path": "/home/user/project"});
        for _ in 0..depth {
            value = json!({"a": value});
        }
        value
    }

    /// Build a deeply nested JSON array: `[[[ ... [{"workspace_path": "/secret"}] ... ]]]`.
    fn build_nested_array(depth: usize) -> Value {
        let mut value: Value = json!([{"workspace_path": "/home/user/project"}]);
        for _ in 0..depth {
            value = Value::Array(vec![value]);
        }
        value
    }

    #[test]
    fn test_depth_limit_stops_recursion_objects() {
        let config = RedactionConfig::standard();

        // Nest deeper than the default limit
        let depth = DEFAULT_REDACTION_MAX_DEPTH + 10;
        let mut value = build_nested_object(depth);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            ctx.result.depth_limit_reached,
            "Walker should report depth limit reached for nesting depth {}",
            depth
        );
    }

    #[test]
    fn test_depth_limit_stops_recursion_arrays() {
        let config = RedactionConfig::standard();

        let depth = DEFAULT_REDACTION_MAX_DEPTH + 10;
        let mut value = build_nested_array(depth);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            ctx.result.depth_limit_reached,
            "Walker should report depth limit reached for array nesting depth {}",
            depth
        );
    }

    #[test]
    fn test_within_depth_limit_processes_normally() {
        let config = RedactionConfig::standard();

        // 10 levels is well within the 128 default
        let mut value = build_nested_object(10);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            !ctx.result.depth_limit_reached,
            "Walker should not hit depth limit for nesting depth 10"
        );
        // The deeply nested workspace_path should be redacted
        assert!(
            ctx.result.workspace_path_redacted,
            "workspace_path should be redacted within depth limit"
        );
    }

    #[test]
    fn test_depth_limit_exact_boundary() {
        let config = RedactionConfig::standard();

        // At exactly max_depth-1 nesting, the innermost value is at depth max_depth.
        // The guard fires when depth >= max_depth, so the last object won't be entered.
        let depth = DEFAULT_REDACTION_MAX_DEPTH;
        let mut value = build_nested_object(depth);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        // At exactly the limit, the guard should fire
        assert!(
            ctx.result.depth_limit_reached,
            "Walker should hit depth limit at exactly max_depth nesting"
        );
    }

    #[test]
    fn test_depth_limit_one_below_boundary_succeeds() {
        let config = RedactionConfig::standard();

        // One below the limit: depth goes up to max_depth-1, which passes the guard
        let depth = DEFAULT_REDACTION_MAX_DEPTH - 2;
        let mut value = build_nested_object(depth);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            !ctx.result.depth_limit_reached,
            "Walker should not hit depth limit at max_depth-2 nesting"
        );
        assert!(
            ctx.result.workspace_path_redacted,
            "workspace_path should be redacted just under the limit"
        );
    }

    #[test]
    fn test_custom_max_depth_via_context() {
        let config = RedactionConfig::standard();

        // Use a very small max_depth to verify it works
        let mut ctx = WalkerContext::new(&config, &[], &[]);
        ctx.max_depth = 3;

        let mut value = build_nested_object(5);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            ctx.result.depth_limit_reached,
            "Walker should hit custom depth limit of 3 at nesting depth 5"
        );
    }

    #[test]
    fn test_mixed_object_array_nesting_depth_limit() {
        let config = RedactionConfig::standard();

        // Build alternating object/array nesting
        let mut value = json!({"workspace_path": "/home/user/project"});
        for i in 0..DEFAULT_REDACTION_MAX_DEPTH + 5 {
            if i % 2 == 0 {
                value = json!({"nested": value});
            } else {
                value = Value::Array(vec![value]);
            }
        }

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            ctx.result.depth_limit_reached,
            "Walker should hit depth limit on mixed object/array nesting"
        );
    }

    #[test]
    fn test_depth_limit_redacts_deeply_nested_values() {
        let config = RedactionConfig::standard();
        let depth = DEFAULT_REDACTION_MAX_DEPTH + 10; // Nest deeper than the limit

        let mut value = build_nested_object(depth);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(
            ctx.result.depth_limit_reached,
            "Walker should report depth limit reached"
        );

        // Traverse down the 'a' fields to the point where redaction should occur
        let mut current_value = &mut value;
        for _ in 0..(DEFAULT_REDACTION_MAX_DEPTH - 1) {
            // Go to one level above the limit
            current_value = current_value.as_object_mut().unwrap().get_mut("a").unwrap();
        }

        // The value at the depth limit should now be redacted.
        // `current_value` is now the object `{"a": <redacted_value>}`
        // We need to get the actual value of 'a' to assert it's the redacted string.
        let final_redacted_value = current_value.as_object().unwrap().get("a").unwrap();

        assert_eq!(
            final_redacted_value,
            &Value::String(config.redacted_placeholder.clone()),
            "The deeply nested value at the depth limit should be replaced by the placeholder"
        );
        assert_eq!(
            ctx.result.depth_limit_redacted, 1,
            "One depth-limited value should have been redacted"
        );
    }

    #[test]
    fn test_depth_limit_bypass_via_custom_redact_fields() {
        let mut config = RedactionConfig::standard();
        config.custom_redact_fields.push("a".to_string());
        config.max_depth = 10;

        let mut value = build_nested_object(100);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        // This SHOULD hit the depth limit, but if it doesn't, we found a bypass.
        assert!(
            ctx.result.depth_limit_reached,
            "Walker should hit depth limit even with custom_redact_fields"
        );
    }

    #[test]
    fn test_depth_limit_dry_run_records_preview() {
        let mut config = RedactionConfig::standard();
        config.dry_run = true;
        config.max_depth = 5;

        let mut value = build_nested_object(10);

        let mut ctx = WalkerContext::new(&config, &[], &[]);
        walk_and_redact(&mut value, &mut ctx);

        assert!(ctx.result.depth_limit_reached);
        assert!(ctx.result.depth_limit_redacted > 0);

        // In dry-run mode, the preview should use a constant string, not serialize the subtree
        let depth_previews: Vec<_> = ctx
            .preview_targets
            .iter()
            .filter(|t| t.reason == RedactionReason::DepthLimitExceeded)
            .collect();
        assert!(
            !depth_previews.is_empty(),
            "Dry-run should record depth-limit preview targets"
        );
        assert_eq!(
            depth_previews[0].original_preview, "[depth limit exceeded]",
            "Preview should use constant string, not serialize the nested value"
        );
    }
}
