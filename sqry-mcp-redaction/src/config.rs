//! Configuration for the redaction engine.

use std::path::PathBuf;

use crate::whitelist::{WHITELIST_MINIMAL, WHITELIST_STANDARD, WHITELIST_STRICT};

/// Wire-side view of `sqry_core::workspace::LogicalWorkspace` carried into
/// the redactor.
///
/// Defined locally so the leaf redaction crate stays free of
/// a `sqry-core` dependency; `sqry-mcp` (which already depends on both)
/// constructs a `LogicalWorkspaceView` from a real
/// `sqry_core::workspace::LogicalWorkspace` before handing it to
/// [`crate::Redactor::with_logical_workspace`].
///
/// Only the four fields required by the STEP_7 path-redaction policy are
/// carried — `workspace_id_short` (for member-folder prefix emission),
/// `source_roots` (each tagged with its short identifier so distinct
/// source roots inside one workspace are distinguishable in redacted
/// output), `member_folders` (for per-member-folder prefix emission), and
/// `exclusions` (for the exclusions-take-precedence rule of acceptance
/// criterion 6 / 9). Member-folder reasons and the workspace identity
/// metadata are deliberately not carried — they are not needed for path
/// rewrite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogicalWorkspaceView {
    /// First 16 hex chars of the workspace `WorkspaceId`. Used as the
    /// member-folder prefix in `minimal` mode (acceptance criterion 5)
    /// and folded into the strict-mode hash input (criterion 7).
    pub workspace_id_short: String,
    /// Per-source-root entries: `(source_root_id, canonical_path)`. The
    /// `source_root_id` is an 8-hex-char digest of the source-root path
    /// scoped under the parent workspace_id (see
    /// [`compute_source_root_id`]) — used as the prefix in `minimal`
    /// mode (criterion 4) and folded into the strict-mode hash input
    /// (criterion 7).
    pub source_roots: Vec<(String, PathBuf)>,
    /// Canonical absolute paths of member folders (non-indexed but
    /// in-workspace).
    pub member_folders: Vec<PathBuf>,
    /// Canonical absolute paths of excluded entries. The redactor (and
    /// the `sqry-mcp` engine) consult this list **before** the
    /// workspace-bound check; an excluded path is rejected / emitted as
    /// an opaque hash regardless of where it sits relative to the
    /// source roots.
    pub exclusions: Vec<PathBuf>,
}

impl LogicalWorkspaceView {
    /// Construct an empty view. Equivalent to `Default::default()`. Kept
    /// as an explicit constructor so call-sites read clearly.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` if `path` (or one of its ancestors up to the source-root
    /// boundary) appears in `exclusions`. Path matching mirrors
    /// [`sqry_core::workspace::LogicalWorkspace::classify`]: exact
    /// match or descendant. The redactor uses this for criterion 6
    /// and 9 (excluded paths take precedence over containment).
    #[must_use]
    pub fn is_excluded(&self, path: &std::path::Path) -> bool {
        self.exclusions.iter().any(|excl| {
            // Exact match or descendant — same semantics as sqry-core's
            // `path_matches`.
            path == excl.as_path() || path.starts_with(excl)
        })
    }

    /// Return the source root that **contains** `path`, if any. The
    /// redactor uses this to decide which `source_root_id` to emit as
    /// a path prefix in `minimal` mode (criterion 4).
    #[must_use]
    pub fn enclosing_source_root(&self, path: &std::path::Path) -> Option<&(String, PathBuf)> {
        // Iterate longest-prefix first so a nested source root wins over
        // its ancestor when both match.
        let mut best: Option<&(String, PathBuf)> = None;
        for entry in &self.source_roots {
            let (_, root) = entry;
            if path == root.as_path() || path.starts_with(root) {
                match best {
                    Some(prev)
                        if prev.1.as_path().components().count() >= root.components().count() => {}
                    _ => best = Some(entry),
                }
            }
        }
        best
    }

    /// Return the member folder that **contains** `path`, if any.
    #[must_use]
    pub fn enclosing_member_folder(&self, path: &std::path::Path) -> Option<&PathBuf> {
        let mut best: Option<&PathBuf> = None;
        for folder in &self.member_folders {
            if path == folder.as_path() || path.starts_with(folder) {
                match best {
                    Some(prev) if prev.components().count() >= folder.components().count() => {}
                    _ => best = Some(folder),
                }
            }
        }
        best
    }
}

/// Compute a short, stable `source_root_id` for a source-root path under
/// a parent workspace_id_short. 8 hex chars, derived from
/// `SHA-256(workspace_id_short || ":" || source_root_path_utf8)`.
///
/// The redactor uses this to emit deterministic but workspace-scoped
/// source-root prefixes in minimal mode (criterion 4) without leaking
/// the underlying path. The same digest is folded into the strict-mode
/// hash input so strict tokens cover the source-root prefix (criterion 7).
#[must_use]
pub fn compute_source_root_id(
    workspace_id_short: &str,
    source_root_path: &std::path::Path,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(workspace_id_short.as_bytes());
    hasher.update(b":");
    let s = source_root_path.to_string_lossy();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:08x}",
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
    )
}

/// Maximum allowed salt length in characters.
pub const MAX_SALT_LENGTH: usize = 256;

/// Default maximum nesting depth for the redaction walker.
///
/// Prevents stack overflow from deeply nested JSON structures.
/// Matches `serde_json`'s default deserialization recursion limit.
///
/// Override via `SQRY_REDACTION_MAX_DEPTH` environment variable.
pub const DEFAULT_REDACTION_MAX_DEPTH: usize = 128;

// Safety bounds for max depth
const MIN_REDACTION_MAX_DEPTH: usize = 8;
const MAX_REDACTION_MAX_DEPTH: usize = 512;

/// Get the configured maximum redaction walker depth.
///
/// Reads `SQRY_REDACTION_MAX_DEPTH` from environment, falls back to
/// [`DEFAULT_REDACTION_MAX_DEPTH`], and clamps to `[8, 512]`.
#[must_use]
pub fn redaction_max_depth() -> usize {
    std::env::var("SQRY_REDACTION_MAX_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_REDACTION_MAX_DEPTH)
        .clamp(MIN_REDACTION_MAX_DEPTH, MAX_REDACTION_MAX_DEPTH)
}

/// Security mode for redaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SecurityMode {
    /// Whitelist mode: only explicitly allowed fields pass through (DEFAULT).
    ///
    /// All fields are considered sensitive unless explicitly whitelisted.
    /// This is the recommended and secure-by-default mode.
    #[default]
    Whitelist,

    /// Blacklist mode: only explicitly blocked fields are redacted (legacy).
    ///
    /// Use with caution - new fields in MCP responses will pass through unredacted.
    Blacklist,

    /// Passthrough mode: NO filtering, all fields pass through unchanged.
    ///
    /// ONLY for `none` preset — trusted local tools with no network connectivity.
    /// **WARNING**: Using this mode with external services exposes all data.
    Passthrough,
}

/// Redaction configuration (whitelist-first model).
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::RedactionConfig;
///
/// // Use a preset
/// let config = RedactionConfig::standard();
///
/// // Or customize
/// let config = RedactionConfig {
///     redact_code_context: false, // Keep code visible
///     ..RedactionConfig::standard()
/// };
/// ```
#[derive(Clone, Debug)]
pub struct RedactionConfig {
    /// Security mode: whitelist (default), blacklist (legacy), or passthrough.
    pub security_mode: SecurityMode,

    /// Redact absolute paths to workspace-relative.
    pub redact_absolute_paths: bool,

    /// Redact the `workspace_path` field from responses.
    pub redact_workspace_path: bool,

    /// Redact `file://` URIs (convert to relative paths or mask).
    pub redact_file_uris: bool,

    /// Redact code context content.
    pub redact_code_context: bool,

    /// Redact documentation strings.
    pub redact_documentation: bool,

    /// Enable pattern-based path detection in arbitrary string fields.
    pub detect_paths_in_strings: bool,

    /// Hash filenames instead of showing them (strict mode feature).
    ///
    /// When enabled, `src/main.rs` becomes `<workspace>/[hash:8chars]`.
    pub hash_filenames: bool,

    /// Optional salt for filename hashing.
    ///
    /// - `None` (default): Deterministic hashing, stable across all sessions
    /// - `Some(salt)`: Per-deployment variance, same salt = same hashes
    pub hash_salt: Option<String>,

    /// Placeholder for redacted content (default: `[REDACTED]`).
    pub redacted_placeholder: String,

    /// Placeholder for redacted paths (default: `<workspace>`).
    pub workspace_placeholder: String,

    /// Optional workspace root for intelligent path conversion.
    pub workspace_root: Option<PathBuf>,

    /// Fields to ALWAYS redact (blacklist overlay on whitelist).
    pub custom_redact_fields: Vec<String>,

    /// `JSONPath` expressions for nested field redaction.
    ///
    /// Example: `$.results[*].edges[*].from.fileUri`
    pub redact_paths: Vec<String>,

    /// Whitelist of fields to preserve (only in whitelist mode).
    ///
    /// Fields not in this list are redacted by default.
    pub whitelist_fields: Vec<String>,

    /// `JSONPath` expressions for fields to preserve.
    pub preserve_paths: Vec<String>,

    /// Enable dry-run mode (report what would be redacted without modifying).
    pub dry_run: bool,

    /// Maximum nesting depth for the redaction walker.
    ///
    /// Prevents stack overflow from deeply nested JSON structures.
    /// Default: [`DEFAULT_REDACTION_MAX_DEPTH`] (128).
    /// Override via `SQRY_REDACTION_MAX_DEPTH` environment variable.
    pub max_depth: usize,

    /// Optional `LogicalWorkspace` view bound to this redactor.
    ///
    /// Populated by [`crate::Redactor::with_logical_workspace`]; left
    /// `None` when the redactor is constructed without workspace
    /// awareness. When `Some`, the path-rewrite rules consult
    /// `source_roots` / `member_folders` / `exclusions` per acceptance
    /// criteria 3-9 of STEP_7. When `None`, behavior matches the
    /// pre-STEP_7 single-workspace pipeline.
    pub logical_workspace: Option<LogicalWorkspaceView>,

    /// Whether to prefer workspace-scoped path rewrite (`<workspace_id_short>/...`)
    /// over source-root-scoped rewrite (`<source_root_id>/...`).
    ///
    /// Defaults to `true` when a `LogicalWorkspace` is bound (criterion 8).
    /// Used to disambiguate paths that fall inside a member folder vs a
    /// source root: when `aggregate_workspace_paths` is `true`,
    /// member-folder paths render as `<workspace_id_short>/...` and
    /// source-root paths render as `<source_root_id>/...`. Setting this
    /// to `false` forces all in-workspace paths to render as
    /// `<source_root_id>/...` even when they sit in a member folder
    /// (the member-folder path is then treated as out-of-source-root and
    /// the redactor falls back to the legacy `<external>` prefix).
    pub aggregate_workspace_paths: bool,

    /// Reveal the clean workspace-relative path layout (issue #394 item 4).
    ///
    /// Defaults to `false` (the anonymizing behaviour): in a bound logical
    /// workspace, in-workspace paths render with the anonymizing
    /// `<source_root_id>/...` / `<workspace_id_short>/...` prefix. When `true`
    /// (the `relative` preset), that prefix is dropped and the path renders as
    /// the clean workspace-relative remainder (e.g. `kernel/time.rs`).
    ///
    /// This NEVER relaxes the absolute-host-path strip: the emitted remainder is
    /// always workspace-relative, and genuinely-external paths still render as
    /// `<external>/<basename>`. The only thing revealed is the workspace-relative
    /// layout and the loss of multi-source-root disambiguation that the prefix
    /// provided. It is opt-in and intended for trusted local analysis.
    pub reveal_workspace_relative_layout: bool,
}

impl RedactionConfig {
    /// No redaction (passthrough mode) - USE WITH CAUTION.
    ///
    /// Only for trusted local tools with NO network connectivity.
    /// Uses Passthrough security mode which SKIPS whitelist filtering.
    ///
    /// # Warning
    ///
    /// This exposes ALL data including:
    /// - Absolute file paths
    /// - Workspace paths
    /// - Source code
    /// - Documentation
    #[must_use]
    pub fn none() -> Self {
        Self {
            security_mode: SecurityMode::Passthrough,
            redact_absolute_paths: false,
            redact_workspace_path: false,
            redact_file_uris: false,
            redact_code_context: false,
            redact_documentation: false,
            detect_paths_in_strings: false,
            hash_filenames: false,
            hash_salt: None,
            redacted_placeholder: String::new(),
            workspace_placeholder: String::new(),
            workspace_root: None,
            custom_redact_fields: Vec::new(),
            redact_paths: Vec::new(),
            whitelist_fields: Vec::new(), // Ignored in Passthrough mode
            preserve_paths: Vec::new(),
            dry_run: false,
            max_depth: redaction_max_depth(),
            logical_workspace: None,
            aggregate_workspace_paths: true,
            reveal_workspace_relative_layout: false,
        }
    }

    /// Minimal redaction: P0 protection (paths, URIs, workspace) with code/docs PRESERVED.
    ///
    /// Uses Whitelist security mode with `WHITELIST_MINIMAL` (most permissive).
    ///
    /// # Use Case
    ///
    /// Cloud services that need to see code context but shouldn't see infrastructure details.
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            security_mode: SecurityMode::Whitelist,
            redact_absolute_paths: true,
            redact_workspace_path: true,
            redact_file_uris: true,
            redact_code_context: false,  // Code PRESERVED
            redact_documentation: false, // Docs PRESERVED
            detect_paths_in_strings: true,
            hash_filenames: false,
            hash_salt: None,
            redacted_placeholder: "[REDACTED]".to_string(),
            workspace_placeholder: "<workspace>".to_string(),
            workspace_root: None,
            custom_redact_fields: Vec::new(),
            redact_paths: Vec::new(),
            whitelist_fields: WHITELIST_MINIMAL.iter().map(|&s| s.to_string()).collect(),
            preserve_paths: Vec::new(),
            dry_run: false,
            max_depth: redaction_max_depth(),
            logical_workspace: None,
            aggregate_workspace_paths: true,
            reveal_workspace_relative_layout: false,
        }
    }

    /// Standard redaction: P0 + code context redacted, docs preserved.
    ///
    /// Uses Whitelist security mode with `WHITELIST_STANDARD`.
    /// Recommended default for cloud integrations.
    ///
    /// # Use Case
    ///
    /// Cloud services where code is confidential but documentation can help understanding.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            security_mode: SecurityMode::Whitelist,
            redact_absolute_paths: true,
            redact_workspace_path: true,
            redact_file_uris: true,
            redact_code_context: true,   // Code REDACTED
            redact_documentation: false, // Docs preserved
            detect_paths_in_strings: true,
            hash_filenames: false,
            hash_salt: None,
            redacted_placeholder: "[REDACTED]".to_string(),
            workspace_placeholder: "<workspace>".to_string(),
            workspace_root: None,
            custom_redact_fields: Vec::new(),
            redact_paths: Vec::new(),
            whitelist_fields: WHITELIST_STANDARD.iter().map(|&s| s.to_string()).collect(),
            preserve_paths: Vec::new(),
            dry_run: false,
            max_depth: redaction_max_depth(),
            logical_workspace: None,
            aggregate_workspace_paths: true,
            reveal_workspace_relative_layout: false,
        }
    }

    /// Strict redaction: Maximum protection — code, docs, and filename hashing.
    ///
    /// Uses Whitelist security mode with `WHITELIST_STRICT` (most restrictive).
    /// Use for untrusted external services.
    ///
    /// # Use Case
    ///
    /// External logging, analytics, or untrusted third-party integrations.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            security_mode: SecurityMode::Whitelist,
            redact_absolute_paths: true,
            redact_workspace_path: true,
            redact_file_uris: true,
            redact_code_context: true,  // Code REDACTED
            redact_documentation: true, // Docs REDACTED
            detect_paths_in_strings: true,
            hash_filenames: true, // Filenames HASHED
            hash_salt: None,      // Deterministic hashing
            redacted_placeholder: "[REDACTED]".to_string(),
            workspace_placeholder: "<workspace>".to_string(),
            workspace_root: None,
            custom_redact_fields: Vec::new(),
            redact_paths: Vec::new(),
            whitelist_fields: WHITELIST_STRICT.iter().map(|&s| s.to_string()).collect(),
            preserve_paths: Vec::new(),
            dry_run: false,
            max_depth: redaction_max_depth(),
            logical_workspace: None,
            aggregate_workspace_paths: true,
            reveal_workspace_relative_layout: false,
        }
    }

    /// Legible workspace-relative redaction (issue #394 item 4).
    ///
    /// Same security posture as [`Self::minimal`] (Whitelist mode; code/docs
    /// preserved; absolute host paths stripped) EXCEPT that in-workspace paths
    /// render as the clean workspace-relative remainder
    /// (e.g. `kernel/time.rs`) instead of the anonymizing
    /// `<source_root_id>/...` / `<workspace_id_short>/...` prefix. External paths
    /// still render as `<external>/<basename>`; no absolute host path is ever
    /// emitted. Opt-in, for trusted local analysis where mapping results back to
    /// source matters more than source-root anonymization.
    #[must_use]
    pub fn relative() -> Self {
        Self {
            reveal_workspace_relative_layout: true,
            ..Self::minimal()
        }
    }

    /// Load configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// | Variable | Values | Default | Description |
    /// |----------|--------|---------|-------------|
    /// | `SQRY_REDACTION_PRESET` | `none`, `minimal`, `relative`, `standard`, `strict` | `standard` | Base preset |
    /// | `SQRY_REDACT_PATHS` | `0`, `1` | per preset | Redact absolute paths |
    /// | `SQRY_REDACT_WORKSPACE` | `0`, `1` | per preset | Redact workspace_path |
    /// | `SQRY_REDACT_URIS` | `0`, `1` | per preset | Redact file:// URIs |
    /// | `SQRY_REDACT_CODE` | `0`, `1` | per preset | Redact code context |
    /// | `SQRY_REDACT_DOCS` | `0`, `1` | per preset | Redact documentation |
    /// | `SQRY_REDACT_PATTERNS` | `0`, `1` | per preset | Enable pattern detection |
    /// | `SQRY_HASH_FILENAMES` | `0`, `1` | per preset | Enable filename hashing |
    /// | `SQRY_REVEAL_PATHS` | `0`, `1` | per preset | Reveal clean workspace-relative paths (drop anonymizing prefix) |
    /// | `SQRY_HASH_SALT` | string | none | Optional salt for hashing |
    /// | `SQRY_WORKSPACE_ROOT` | path | none | Workspace root path |
    /// | `SQRY_REDACTION_MAX_DEPTH` | `8`-`512` | `128` | Max walker recursion depth |
    #[must_use]
    pub fn from_env() -> Self {
        let preset = std::env::var("SQRY_REDACTION_PRESET")
            .ok()
            .and_then(|s| match s.to_lowercase().as_str() {
                "none" => Some(Self::none()),
                "minimal" => Some(Self::minimal()),
                "relative" => Some(Self::relative()),
                "standard" => Some(Self::standard()),
                "strict" => Some(Self::strict()),
                _ => {
                    log::warn!(
                        "Invalid SQRY_REDACTION_PRESET '{}', falling back to 'standard'",
                        s
                    );
                    None
                }
            })
            .unwrap_or_else(Self::standard);

        let parse_bool = |key: &str, default: bool| -> bool {
            std::env::var(key)
                .ok()
                .and_then(|v| match v.as_str() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => {
                        log::warn!("Invalid boolean value for {}: '{}', using default", key, v);
                        None
                    }
                })
                .unwrap_or(default)
        };

        let hash_salt = std::env::var("SQRY_HASH_SALT").ok().and_then(|s| {
            if s.is_empty() {
                None // Empty string normalized to None
            } else {
                Some(s)
            }
        });

        let workspace_root = std::env::var("SQRY_WORKSPACE_ROOT").ok().map(PathBuf::from);

        let mut whitelist_fields = preset.whitelist_fields.clone();
        if let Ok(extra) = std::env::var("SQRY_WHITELIST_FIELDS") {
            whitelist_fields.extend(extra.split(',').map(|s| s.trim().to_string()));
        }

        let preserve_paths = std::env::var("SQRY_PRESERVE_PATHS")
            .ok()
            .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_default();

        Self {
            redact_absolute_paths: parse_bool("SQRY_REDACT_PATHS", preset.redact_absolute_paths),
            redact_workspace_path: parse_bool(
                "SQRY_REDACT_WORKSPACE",
                preset.redact_workspace_path,
            ),
            redact_file_uris: parse_bool("SQRY_REDACT_URIS", preset.redact_file_uris),
            redact_code_context: parse_bool("SQRY_REDACT_CODE", preset.redact_code_context),
            redact_documentation: parse_bool("SQRY_REDACT_DOCS", preset.redact_documentation),
            detect_paths_in_strings: parse_bool(
                "SQRY_REDACT_PATTERNS",
                preset.detect_paths_in_strings,
            ),
            hash_filenames: parse_bool("SQRY_HASH_FILENAMES", preset.hash_filenames),
            hash_salt,
            workspace_root,
            whitelist_fields,
            preserve_paths,
            reveal_workspace_relative_layout: parse_bool(
                "SQRY_REVEAL_PATHS",
                preset.reveal_workspace_relative_layout,
            ),
            ..preset
        }
    }

    /// Create configuration from a given base preset, then apply fine-grained
    /// environment variable overrides (`SQRY_REDACT_PATHS`, `SQRY_REDACT_CODE`, etc.).
    ///
    /// Unlike [`from_env()`](Self::from_env), this does NOT read `SQRY_REDACTION_PRESET`
    /// from the environment — the caller supplies the preset name directly. This is useful
    /// when the preset comes from a config file rather than an env var.
    #[must_use]
    pub fn from_preset_with_env(preset_name: &str) -> Self {
        let preset = match preset_name {
            "none" => Self::none(),
            "minimal" => Self::minimal(),
            "relative" => Self::relative(),
            "strict" => Self::strict(),
            // Default to standard for unknown presets
            _ => Self::standard(),
        };

        let parse_bool = |key: &str, default: bool| -> bool {
            std::env::var(key)
                .ok()
                .and_then(|v| match v.as_str() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => {
                        log::warn!("Invalid boolean value for {}: '{}', using default", key, v);
                        None
                    }
                })
                .unwrap_or(default)
        };

        let hash_salt = std::env::var("SQRY_HASH_SALT")
            .ok()
            .and_then(|s| if s.is_empty() { None } else { Some(s) });

        let workspace_root = std::env::var("SQRY_WORKSPACE_ROOT").ok().map(PathBuf::from);

        let mut whitelist_fields = preset.whitelist_fields.clone();
        if let Ok(extra) = std::env::var("SQRY_WHITELIST_FIELDS") {
            whitelist_fields.extend(extra.split(',').map(|s| s.trim().to_string()));
        }

        let preserve_paths = std::env::var("SQRY_PRESERVE_PATHS")
            .ok()
            .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_default();

        Self {
            redact_absolute_paths: parse_bool("SQRY_REDACT_PATHS", preset.redact_absolute_paths),
            redact_workspace_path: parse_bool(
                "SQRY_REDACT_WORKSPACE",
                preset.redact_workspace_path,
            ),
            redact_file_uris: parse_bool("SQRY_REDACT_URIS", preset.redact_file_uris),
            redact_code_context: parse_bool("SQRY_REDACT_CODE", preset.redact_code_context),
            redact_documentation: parse_bool("SQRY_REDACT_DOCS", preset.redact_documentation),
            detect_paths_in_strings: parse_bool(
                "SQRY_REDACT_PATTERNS",
                preset.detect_paths_in_strings,
            ),
            hash_filenames: parse_bool("SQRY_HASH_FILENAMES", preset.hash_filenames),
            hash_salt,
            workspace_root,
            whitelist_fields,
            preserve_paths,
            reveal_workspace_relative_layout: parse_bool(
                "SQRY_REVEAL_PATHS",
                preset.reveal_workspace_relative_layout,
            ),
            ..preset
        }
    }

    /// Validate the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Salt exceeds 256 characters
    pub fn validate(&self) -> Result<(), crate::RedactionError> {
        if let Some(ref salt) = self.hash_salt {
            if salt.len() > MAX_SALT_LENGTH {
                return Err(crate::RedactionError::ConfigError(format!(
                    "Salt length {} exceeds maximum {}",
                    salt.len(),
                    MAX_SALT_LENGTH
                )));
            }
        }
        Ok(())
    }

    /// Get the normalized salt (empty string normalized to None).
    #[must_use]
    pub fn normalized_salt(&self) -> Option<&str> {
        self.hash_salt.as_deref().filter(|s| !s.is_empty())
    }

    /// Check if a field is in the whitelist.
    #[must_use]
    pub fn is_whitelisted(&self, field: &str) -> bool {
        self.whitelist_fields.iter().any(|f| f == field)
    }

    /// Check if a field should be custom-redacted.
    #[must_use]
    pub fn is_custom_redacted(&self, field: &str) -> bool {
        self.custom_redact_fields.iter().any(|f| f == field)
    }
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self::standard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_preset() {
        let config = RedactionConfig::none();
        assert_eq!(config.security_mode, SecurityMode::Passthrough);
        assert!(!config.redact_absolute_paths);
        assert!(!config.redact_workspace_path);
        assert!(!config.redact_code_context);
        assert!(!config.redact_documentation);
        assert!(!config.hash_filenames);
    }

    #[test]
    fn test_minimal_preset() {
        let config = RedactionConfig::minimal();
        assert_eq!(config.security_mode, SecurityMode::Whitelist);
        assert!(config.redact_absolute_paths);
        assert!(config.redact_workspace_path);
        assert!(config.redact_file_uris);
        assert!(!config.redact_code_context); // Code preserved
        assert!(!config.redact_documentation); // Docs preserved
        assert!(!config.hash_filenames);
        assert!(config.detect_paths_in_strings);
    }

    #[test]
    fn test_standard_preset() {
        let config = RedactionConfig::standard();
        assert_eq!(config.security_mode, SecurityMode::Whitelist);
        assert!(config.redact_absolute_paths);
        assert!(config.redact_code_context); // Code redacted
        assert!(!config.redact_documentation); // Docs preserved
        assert!(!config.hash_filenames);
    }

    #[test]
    fn test_strict_preset() {
        let config = RedactionConfig::strict();
        assert_eq!(config.security_mode, SecurityMode::Whitelist);
        assert!(config.redact_absolute_paths);
        assert!(config.redact_code_context);
        assert!(config.redact_documentation); // Docs redacted
        assert!(config.hash_filenames); // Filename hashing enabled
    }

    #[test]
    fn test_default_is_standard() {
        let default = RedactionConfig::default();
        let standard = RedactionConfig::standard();
        assert_eq!(default.security_mode, standard.security_mode);
        assert_eq!(default.redact_code_context, standard.redact_code_context);
    }

    #[test]
    fn test_salt_validation() {
        let mut config = RedactionConfig::standard();
        config.hash_salt = Some("a".repeat(256));
        assert!(config.validate().is_ok());

        config.hash_salt = Some("a".repeat(257));
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_normalized_salt() {
        let mut config = RedactionConfig::standard();
        assert!(config.normalized_salt().is_none());

        config.hash_salt = Some(String::new());
        assert!(config.normalized_salt().is_none()); // Empty normalized to None

        config.hash_salt = Some("mysalt".to_string());
        assert_eq!(config.normalized_salt(), Some("mysalt"));
    }

    #[test]
    fn test_whitelist_check() {
        let config = RedactionConfig::standard();
        assert!(config.is_whitelisted("name"));
        assert!(config.is_whitelisted("kind"));
        assert!(!config.is_whitelisted("some_unknown_field"));
    }

    #[test]
    fn test_redaction_max_depth_default() {
        // Without env var set, should return DEFAULT_REDACTION_MAX_DEPTH
        let depth = super::redaction_max_depth();
        assert!(
            (super::MIN_REDACTION_MAX_DEPTH..=super::MAX_REDACTION_MAX_DEPTH).contains(&depth),
            "Default depth {} should be within bounds",
            depth
        );
    }

    #[test]
    fn test_preset_whitelist_hierarchy() {
        let minimal = RedactionConfig::minimal();
        let standard = RedactionConfig::standard();
        let strict = RedactionConfig::strict();

        // Minimal is most permissive
        assert!(minimal.whitelist_fields.len() > standard.whitelist_fields.len());
        // Standard is more permissive than strict
        assert!(standard.whitelist_fields.len() > strict.whitelist_fields.len());
    }
}
