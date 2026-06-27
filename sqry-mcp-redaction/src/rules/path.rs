//! Path canonicalization and hashing for redaction.
//!
//! This module implements the complete path canonicalization algorithm as specified
//! in Section 6.8 of the MCP Redaction specification, including:
//!
//! - Input classification (Unix, Windows, UNC, relative)
//! - Path normalization (separator conversion, component resolution)
//! - Workspace containment checking
//! - SHA-256 based hashing for strict mode
//! - UNC path handling with network prefix hashing

use sha2::{Digest, Sha256};

use crate::PathError;
use crate::config::LogicalWorkspaceView;

/// Maximum path length in characters.
pub const MAX_PATH_LENGTH: usize = 4096;

/// Canonical result with metadata about the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPath {
    /// The canonicalized path string (forward slashes, no `.` or `..`).
    pub path: String,

    /// Whether this path is inside the workspace.
    pub is_workspace_relative: bool,

    /// For UNC paths: the redacted network prefix.
    pub network_prefix: Option<String>,

    /// Original path type for debugging.
    pub path_type: PathType,
}

/// Classification of path types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathType {
    /// Unix absolute path (starts with `/`).
    UnixAbsolute,

    /// Windows absolute path with drive letter.
    WindowsAbsolute {
        /// The drive letter (uppercase).
        drive: char,
    },

    /// UNC network path.
    UncPath {
        /// Server name (may include IPv6 brackets).
        server: String,
        /// Share name.
        share: String,
    },

    /// Relative path.
    Relative,
}

/// Main entry point: canonicalize a path for hashing.
///
/// # Algorithm Steps
///
/// 1. Input validation (empty, null bytes, control chars, length)
/// 2. Parse URI if applicable (file:// scheme)
/// 3. Classify path type and normalize separators
/// 3b. Resolve relative paths against workspace root
/// 4. Canonicalize components (resolve `.` and `..`)
/// 5. Check workspace containment
/// 6. Handle UNC network prefix
///
/// # Errors
///
/// Returns `PathError` for invalid inputs (empty, null bytes, too long, etc.)
pub fn canonicalize_for_hash(
    input: &str,
    workspace_root: Option<&str>,
) -> Result<CanonicalPath, PathError> {
    // Step 1: Input validation
    validate_path_input(input)?;

    // Step 2: Parse URI if applicable
    let parsed = parse_input_path(input)?;

    // Step 3: Classify path type and normalize separators
    let (path_type, normalized) = classify_and_normalize(&parsed)?;

    // Step 3b: Resolve relative paths against workspace root BEFORE canonicalization
    let (path_type, resolved) = resolve_relative_path(path_type, normalized, workspace_root)?;

    // Step 4: Canonicalize (resolve `.` and `..`, collapse slashes)
    let canonicalized = canonicalize_components(&resolved, &path_type)?;

    // Step 5: Check workspace containment
    let (is_workspace_relative, hash_input) =
        resolve_workspace_containment(&canonicalized, &path_type, workspace_root)?;

    // Step 6: Handle UNC network prefix
    let (hash_input, network_prefix) = apply_unc_prefix(&path_type, hash_input);

    Ok(CanonicalPath {
        path: hash_input,
        is_workspace_relative,
        network_prefix,
        path_type,
    })
}

fn parse_input_path(input: &str) -> Result<String, PathError> {
    if input.starts_with("file://") {
        super::uri::parse_file_uri(input)
    } else {
        Ok(input.to_string())
    }
}

fn resolve_relative_path(
    path_type: PathType,
    normalized: String,
    workspace_root: Option<&str>,
) -> Result<(PathType, String), PathError> {
    if !matches!(path_type, PathType::Relative) {
        return Ok((path_type, normalized));
    }

    let Some(ws_root) = workspace_root else {
        return Ok((path_type, normalized));
    };

    let ws_normalized = normalize_workspace_root(ws_root);
    let joined = join_relative_path(&ws_normalized, &normalized);
    classify_and_normalize(&joined)
}

fn normalize_workspace_root(workspace_root: &str) -> String {
    workspace_root
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn join_relative_path(workspace_root: &str, normalized: &str) -> String {
    if normalized.is_empty() || normalized == "." {
        workspace_root.to_string()
    } else {
        format!("{}/{}", workspace_root, normalized.trim_start_matches("./"))
    }
}

/// Validate input path for security and correctness.
fn validate_path_input(input: &str) -> Result<(), PathError> {
    if input.is_empty() {
        return Err(PathError::EmptyPath);
    }

    if input.len() > MAX_PATH_LENGTH {
        return Err(PathError::PathTooLong {
            len: input.len(),
            max: MAX_PATH_LENGTH,
        });
    }

    // Check for null bytes (path traversal attack vector)
    if input.contains('\0') {
        return Err(PathError::NullByteInPath);
    }

    // Check for control characters (except common whitespace)
    for c in input.chars() {
        if c.is_control() && c != '\t' && c != '\n' && c != '\r' {
            return Err(PathError::ControlCharacterInPath);
        }
    }

    Ok(())
}

/// Classify path type and normalize separators.
fn classify_and_normalize(path: &str) -> Result<(PathType, String), PathError> {
    let normalized = normalize_separators(path);

    if is_device_path(&normalized) {
        return Err(PathError::DevicePath);
    }

    if let Some(result) = parse_extended_path(&normalized)? {
        return Ok(result);
    }

    if let Some(result) = parse_unc_path(&normalized) {
        return Ok(result);
    }

    if let Some(result) = parse_windows_drive(&normalized) {
        return Ok(result);
    }

    if normalized.starts_with('/') {
        return Ok((PathType::UnixAbsolute, normalized));
    }

    Ok((PathType::Relative, normalized))
}

fn normalize_separators(path: &str) -> String {
    path.chars()
        .map(|c| if c == '\\' { '/' } else { c })
        .collect()
}

fn is_device_path(normalized: &str) -> bool {
    normalized.starts_with("//./")
}

fn parse_extended_path(normalized: &str) -> Result<Option<(PathType, String)>, PathError> {
    if !normalized.starts_with("//?/") {
        return Ok(None);
    }

    let after_prefix = &normalized[4..];
    if has_unc_prefix(after_prefix) {
        if let Some(result) = parse_unc_details(&after_prefix[4..]) {
            return Ok(Some(result));
        }
    }

    Ok(Some(classify_and_normalize(after_prefix)?))
}

fn has_unc_prefix(after_prefix: &str) -> bool {
    after_prefix
        .get(..4)
        .is_some_and(|head| head.eq_ignore_ascii_case("UNC/"))
}

fn parse_unc_path(normalized: &str) -> Option<(PathType, String)> {
    if !normalized.starts_with("//") || normalized.starts_with("///") {
        return None;
    }

    let after_slashes = &normalized[2..];
    parse_unc_details(after_slashes)
}

fn parse_unc_details(unc_part: &str) -> Option<(PathType, String)> {
    let parts: Vec<&str> = unc_part.splitn(3, '/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let server = parts[0].to_string();
        let share = parts[1].to_string();
        let remainder = if parts.len() > 2 {
            format!("/{}", parts[2])
        } else {
            String::new()
        };
        return Some((PathType::UncPath { server, share }, remainder));
    }
    None
}

fn parse_windows_drive(normalized: &str) -> Option<(PathType, String)> {
    let mut chars = normalized.chars();
    let drive = chars.next()?;
    let colon = chars.next()?;

    if !drive.is_ascii_alphabetic() || colon != ':' {
        return None;
    }

    let drive = drive.to_ascii_uppercase();
    let rest: String = chars.collect();
    let full_path = if rest.is_empty() || !rest.starts_with('/') {
        format!("{}:/{}", drive, rest.trim_start_matches('/'))
    } else {
        format!("{}:{}", drive, rest)
    };
    Some((PathType::WindowsAbsolute { drive }, full_path))
}

/// Canonicalize path components (resolve `.`, `..`, collapse slashes).
fn canonicalize_components(path: &str, path_type: &PathType) -> Result<String, PathError> {
    let mut components: Vec<&str> = Vec::new();
    let is_absolute = matches!(
        path_type,
        PathType::UnixAbsolute | PathType::WindowsAbsolute { .. }
    );
    let is_unc = matches!(path_type, PathType::UncPath { .. });

    // Track Windows drive prefix separately
    let (prefix, path_portion) = match path_type {
        PathType::WindowsAbsolute { drive } => {
            let prefix_str = format!("{}:", drive);
            let rest = path.strip_prefix(&prefix_str).unwrap_or(path);
            (Some(prefix_str), rest)
        }
        _ => (None, path),
    };

    for part in path_portion.split(['/', '\\']) {
        apply_component(part, &mut components, is_absolute, is_unc)?;
    }

    // Reconstruct the path
    let joined = components.join("/");
    Ok(build_canonicalized_path(
        prefix.as_deref(),
        &joined,
        is_absolute,
        is_unc,
    ))
}

fn apply_component<'a>(
    part: &'a str,
    components: &mut Vec<&'a str>,
    is_absolute: bool,
    is_unc: bool,
) -> Result<(), PathError> {
    match part {
        "" | "." => {
            // Skip empty components and current-dir markers
        }
        ".." => {
            // Parent directory: pop if we can
            if !components.is_empty() {
                // Don't pop past root for absolute paths
                if !(is_absolute && components.len() == 1 && components[0].is_empty()) {
                    components.pop();
                }
            } else if is_unc {
                // UNC path attempting to escape share root
                return Err(PathError::UncEscapeAttempt);
            } else if !is_absolute {
                // Relative path: keep leading `..`
                components.push("..");
            }
            // For absolute paths at root, `..` is absorbed
        }
        component => {
            components.push(component);
        }
    }

    Ok(())
}

fn build_canonicalized_path(
    prefix: Option<&str>,
    joined: &str,
    is_absolute: bool,
    is_unc: bool,
) -> String {
    match (prefix, is_absolute, is_unc) {
        (Some(p), _, _) => {
            // Windows: C:/path
            if joined.is_empty() {
                format!("{}/", p)
            } else {
                format!("{}/{}", p, joined)
            }
        }
        (None, true, _) => {
            // Unix absolute
            if joined.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", joined)
            }
        }
        (None, false, true) => {
            // UNC: path portion after server/share
            if joined.is_empty() {
                "/".to_string() // Root of share
            } else {
                format!("/{}", joined)
            }
        }
        (None, false, false) => {
            // Relative
            if joined.is_empty() {
                ".".to_string()
            } else {
                joined.to_string()
            }
        }
    }
}

fn resolve_workspace_containment(
    canonicalized: &str,
    path_type: &PathType,
    workspace_root: Option<&str>,
) -> Result<(bool, String), PathError> {
    let Some(ws_root) = workspace_root else {
        return Ok((false, canonicalized.to_string()));
    };

    if matches!(path_type, PathType::Relative) {
        return Ok((false, canonicalized.to_string()));
    }

    // Canonicalize workspace WITHOUT network prefix for consistent comparison
    let (ws_canonical, ws_path_type) = canonicalize_workspace_root(ws_root)?;

    if types_compatible(path_type, &ws_path_type) {
        Ok(check_workspace_containment(
            canonicalized,
            &ws_canonical,
            path_type,
        ))
    } else {
        Ok((false, canonicalized.to_string()))
    }
}

fn types_compatible(path_type: &PathType, ws_path_type: &PathType) -> bool {
    match (path_type, ws_path_type) {
        (
            PathType::UncPath {
                server: s1,
                share: sh1,
            },
            PathType::UncPath {
                server: s2,
                share: sh2,
            },
        ) => {
            normalize_unc_server(s1) == normalize_unc_server(s2)
                && sh1.to_lowercase() == sh2.to_lowercase()
        }
        (PathType::WindowsAbsolute { drive: d1 }, PathType::WindowsAbsolute { drive: d2 }) => {
            d1.to_ascii_uppercase() == d2.to_ascii_uppercase()
        }
        (PathType::UnixAbsolute, PathType::UnixAbsolute) => true,
        _ => false, // Mismatched types: never contained
    }
}

fn normalize_unc_server(server: &str) -> String {
    server
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase()
}

/// Check if canonicalized path is within workspace.
fn check_workspace_containment(
    path: &str,
    workspace_root: &str,
    _path_type: &PathType,
) -> (bool, String) {
    let ws_normalized = workspace_root.trim_end_matches('/');
    let path_normalized = path.to_string();

    // Use proper path boundary check, not just starts_with
    let is_contained = if path_normalized == ws_normalized {
        true
    } else if path_normalized.starts_with(ws_normalized) {
        // Must have path separator after workspace prefix
        let after_ws = &path_normalized[ws_normalized.len()..];
        after_ws.starts_with('/')
    } else {
        false
    };

    if is_contained {
        // Extract workspace-relative portion
        let relative = if path_normalized == ws_normalized {
            // Path IS the workspace root - use "/" for consistency
            "/".to_string()
        } else {
            // Path is inside workspace
            path_normalized[ws_normalized.len()..].to_string()
        };
        (true, relative)
    } else {
        // Out of workspace: hash absolute path
        (false, path_normalized)
    }
}

/// Canonicalize workspace root WITHOUT adding network prefix.
fn canonicalize_workspace_root(ws_root: &str) -> Result<(String, PathType), PathError> {
    validate_path_input(ws_root)?;
    let (path_type, normalized) = classify_and_normalize(ws_root)?;
    let canonicalized = canonicalize_components(&normalized, &path_type)?;
    Ok((canonicalized, path_type))
}

fn apply_unc_prefix(path_type: &PathType, hash_input: String) -> (String, Option<String>) {
    match path_type {
        PathType::UncPath { server, share } => {
            let prefix = format!("<network:{}>", hash_unc_prefix(server, share));
            let value = format!("{prefix}{hash_input}");
            (value, Some(prefix))
        }
        _ => (hash_input, None),
    }
}

/// Hash UNC server/share for privacy while maintaining correlation.
///
/// Uses 8 hex chars (32 bits) — see specification for collision risk analysis.
fn hash_unc_prefix(server: &str, share: &str) -> String {
    let mut hasher = Sha256::new();

    // Strip IPv6 brackets for normalized hashing
    let server_normalized = server
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();

    hasher.update(server_normalized.as_bytes());
    hasher.update(b":");
    hasher.update(share.to_lowercase().as_bytes());
    let result = hasher.finalize();

    // 8 hex chars = 32 bits = ~4 billion possibilities
    format!(
        "{:08x}",
        u32::from_be_bytes([result[0], result[1], result[2], result[3]])
    )
}

/// Hash a workspace-relative path for strict mode redaction.
///
/// # Arguments
///
/// * `relative_path` - Workspace-relative path (e.g., "src/main.rs")
/// * `salt` - Optional salt for per-deployment variance
///
/// # Returns
///
/// 8 hexadecimal character hash string.
pub fn hash_path(relative_path: &str, salt: Option<&str>) -> String {
    let mut hasher = Sha256::new();

    // Normalize salt — empty string → None
    let normalized_salt = salt.filter(|s| !s.is_empty());

    if let Some(s) = normalized_salt {
        hasher.update(s.as_bytes());
        hasher.update(b":"); // Separator to prevent prefix collisions
    }

    // Hash the FULL workspace-relative path, not just basename
    hasher.update(relative_path.as_bytes());
    let result = hasher.finalize();

    // Take first 4 bytes (8 hex chars)
    format!(
        "{:08x}",
        u32::from_be_bytes([result[0], result[1], result[2], result[3]])
    )
}

/// Redact a path according to the configuration.
///
/// # Arguments
///
/// * `path` - The original path string
/// * `workspace_root` - Optional workspace root for relative conversion
/// * `workspace_placeholder` - Placeholder for workspace (e.g., `<workspace>`)
/// * `hash_filenames` - Whether to hash filenames (strict mode)
/// * `hash_salt` - Optional salt for hashing
pub fn redact_path(
    path: &str,
    workspace_root: Option<&str>,
    workspace_placeholder: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
) -> Result<String, PathError> {
    let canonical = canonicalize_for_hash(path, workspace_root)?;

    if canonical.is_workspace_relative {
        Ok(redact_workspace_relative_path(
            &canonical,
            workspace_placeholder,
            hash_filenames,
            hash_salt,
        ))
    } else {
        Ok(redact_external_path(&canonical, hash_filenames, hash_salt))
    }
}

fn redact_workspace_relative_path(
    canonical: &CanonicalPath,
    workspace_placeholder: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
) -> String {
    if hash_filenames {
        let hash = hash_path(&canonical.path, hash_salt);
        format_hashed_workspace_path(
            canonical.network_prefix.as_deref(),
            workspace_placeholder,
            &hash,
        )
    } else {
        format_workspace_plain_path(canonical, workspace_placeholder)
    }
}

fn redact_external_path(
    canonical: &CanonicalPath,
    hash_filenames: bool,
    hash_salt: Option<&str>,
) -> String {
    if hash_filenames {
        let hash = hash_path(&canonical.path, hash_salt);
        format_hashed_external_path(canonical.network_prefix.as_deref(), &hash)
    } else {
        format_external_plain_path(canonical)
    }
}

fn format_hashed_workspace_path(
    network_prefix: Option<&str>,
    workspace_placeholder: &str,
    hash: &str,
) -> String {
    if let Some(prefix) = network_prefix {
        format!("{prefix}/[{hash}]")
    } else {
        format!("{workspace_placeholder}/[{hash}]")
    }
}

fn format_workspace_plain_path(canonical: &CanonicalPath, workspace_placeholder: &str) -> String {
    if let Some(prefix) = canonical.network_prefix.as_deref() {
        format!("{prefix}{}", canonical.path)
    } else {
        let display_path = canonical.path.trim_start_matches('/');
        if display_path.is_empty() {
            workspace_placeholder.to_string()
        } else {
            display_path.to_string()
        }
    }
}

fn format_hashed_external_path(network_prefix: Option<&str>, hash: &str) -> String {
    if let Some(prefix) = network_prefix {
        format!("{prefix}/[{hash}]")
    } else {
        format!("<external>/[{hash}]")
    }
}

fn format_external_plain_path(canonical: &CanonicalPath) -> String {
    if let Some(prefix) = canonical.network_prefix.as_deref() {
        format!("{prefix}{}", canonical.path)
    } else {
        // External paths render as `<external>/<basename>` in every preset.
        // Revealing the directory tail of a genuinely-external path would leak
        // the absolute host layout, so even the `relative` preset keeps the
        // basename-only collapse here. The `relative` preset's legibility win is
        // the in-workspace prefix drop in `emit_workspace_relative`, not external
        // paths.
        let filename = canonical
            .path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&canonical.path);
        format!("<external>/{filename}")
    }
}

// ---------------------------------------------------------------------------
// Workspace-aware path redaction (STEP_7).
// ---------------------------------------------------------------------------

/// Outcome of [`workspace_relative_form`] — the redactor's decision about
/// what prefix (if any) to apply when emitting a path under a bound
/// `LogicalWorkspaceView`.
///
/// This is **not** a wire-form structure; it is consumed exclusively by
/// [`redact_path_with_workspace`]. The variants exactly mirror acceptance
/// criteria 3-6 of STEP_7:
///
/// - [`Self::Excluded`] — criterion 6: path matches an `exclusions` entry.
/// - [`Self::SourceRootRelative`] — criterion 4: path lives inside a
///   source root, render as `<source_root_id>/<rel>` in minimal mode.
/// - [`Self::MemberFolderRelative`] — criterion 5: path lives inside a
///   member folder, render as `<workspace_id_short>/<rel-to-member-folder>`
///   in minimal mode.
/// - [`Self::OutsideWorkspace`] — path falls outside the bound
///   workspace; the legacy single-workspace pipeline applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRelativeForm {
    /// Path matched a `LogicalWorkspaceView::exclusions` entry. The
    /// redactor emits an opaque hash and sets the `excluded: true` flag
    /// (criterion 6). Carries the canonicalized path for hashing.
    Excluded {
        /// Canonicalized path string used as the hash input.
        canonical: String,
    },
    /// Path lives inside a source root. Carries the source-root id and
    /// the path relative to that source root (no leading `/`).
    SourceRootRelative {
        /// 8-hex-char source-root identifier (see
        /// [`crate::config::compute_source_root_id`]).
        source_root_id: String,
        /// Path relative to the matching source-root, with no leading
        /// `/`. Empty string when the input *is* the source root.
        relative: String,
    },
    /// Path lives inside a member folder. Carries the workspace_id_short
    /// and the path relative to the member folder (no leading `/`).
    MemberFolderRelative {
        /// 16-hex-char workspace-id prefix (see
        /// [`crate::config::LogicalWorkspaceView::workspace_id_short`]).
        workspace_id_short: String,
        /// Path relative to the matching member folder, with no leading
        /// `/`. Empty string when the input *is* the member folder.
        relative: String,
    },
    /// Path is outside the workspace's source roots and member folders.
    /// The legacy `<external>` / `<workspace>` pipeline applies.
    OutsideWorkspace,
}

/// Decide which workspace-relative form a canonicalized path should
/// take given the bound `LogicalWorkspaceView` and the configured
/// preferences.
///
/// Implements the precedence rules from acceptance criteria
/// 4, 5, 6, 8, and 9:
///
/// 1. **Exclusions take precedence** (criterion 9). Returns
///    [`WorkspaceRelativeForm::Excluded`] before any source-root /
///    member-folder check.
/// 2. **Source roots beat member folders.** A path that is *both*
///    descendant of a source root and a member folder (rare, but
///    possible if a member folder nests under a source root) is
///    treated as source-root-relative.
/// 3. **`aggregate_workspace_paths = false` collapses member folders.**
///    When the caller opts out of workspace aggregation (criterion 8),
///    a member-folder hit is reported as
///    [`WorkspaceRelativeForm::OutsideWorkspace`] so the legacy
///    `<external>` pipeline applies.
#[must_use]
pub fn workspace_relative_form(
    canonical: &CanonicalPath,
    workspace: &LogicalWorkspaceView,
    aggregate_workspace_paths: bool,
) -> WorkspaceRelativeForm {
    let path_for_match = std::path::PathBuf::from(canonical.path.clone());

    // Criterion 9: exclusions first.
    if workspace.is_excluded(&path_for_match) {
        return WorkspaceRelativeForm::Excluded {
            canonical: canonical.path.clone(),
        };
    }

    // Criterion 4: source roots take precedence over member folders.
    if let Some((id, root)) = workspace.enclosing_source_root(&path_for_match) {
        let root_str = root.to_string_lossy();
        let rel = strip_path_prefix(&canonical.path, root_str.as_ref());
        return WorkspaceRelativeForm::SourceRootRelative {
            source_root_id: id.clone(),
            relative: rel,
        };
    }

    // Criterion 5 / 8: member folders only when aggregation is enabled.
    if aggregate_workspace_paths
        && let Some(member) = workspace.enclosing_member_folder(&path_for_match)
    {
        let member_str = member.to_string_lossy();
        let rel = strip_path_prefix(&canonical.path, member_str.as_ref());
        return WorkspaceRelativeForm::MemberFolderRelative {
            workspace_id_short: workspace.workspace_id_short.clone(),
            relative: rel,
        };
    }

    WorkspaceRelativeForm::OutsideWorkspace
}

/// Strip a path prefix and return the leading-slash-free remainder.
/// `/home/user/proj` minus `/home/user/proj` -> `""`.
/// `/home/user/proj/src/main.rs` minus `/home/user/proj` -> `src/main.rs`.
fn strip_path_prefix(full: &str, prefix: &str) -> String {
    let trimmed_prefix = prefix.trim_end_matches('/');
    if let Some(rest) = full.strip_prefix(trimmed_prefix) {
        rest.trim_start_matches('/').to_string()
    } else {
        full.trim_start_matches('/').to_string()
    }
}

/// Workspace-aware variant of [`redact_path`]. Implements the STEP_7
/// path-redaction policy as specified by acceptance criteria 3-7:
///
/// | Preset | Form | Output |
/// |--------|------|--------|
/// | `none` | any | absolute path (criterion 3) |
/// | `minimal` | source root | `<source_root_id>/<rel>` (criterion 4) |
/// | `minimal` | member folder | `<workspace_id_short>/<rel>` (criterion 5) |
/// | any | excluded | `<opaque-hash>` + `excluded: true` flag (criterion 6) |
/// | `strict` | any in-workspace | hashed `<workspace-prefix>/<rel>` token (criterion 7) |
///
/// `strict` mode applies the workspace-aware rewrite **before** the
/// hash step so the hashed token covers the workspace prefix —
/// `<source_root_id>` / `<workspace_id_short>` never appears in
/// cleartext on the wire (criterion 7).
///
/// # Returns
///
/// `Ok(RedactedPath { rendered, excluded })` where `rendered` is the
/// emitted path and `excluded` is `true` iff the input matched an
/// exclusion (criterion 6). `excluded` is wire-side metadata — the
/// caller (typically the redaction walker) decides whether to surface
/// it as a JSON sibling field or fold it into the result envelope.
///
/// # Errors
///
/// Returns [`PathError`] for malformed inputs, identical to
/// [`redact_path`].
pub fn redact_path_with_workspace(
    path: &str,
    workspace: &LogicalWorkspaceView,
    workspace_placeholder: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
    aggregate_workspace_paths: bool,
    workspace_root_for_legacy: Option<&str>,
    reveal_layout: bool,
) -> Result<RedactedPath, PathError> {
    let canonical = canonicalize_for_hash(path, workspace_root_for_legacy)?;
    // STEP_7 codex iter3 fix: `canonicalize_for_hash` strips
    // `workspace_root_for_legacy` to a workspace-relative form when the
    // input falls under it (see `resolve_workspace_containment`). The
    // workspace-aware containment check below requires the **absolute**
    // path so it can match against the view's `source_roots` (which are
    // canonical absolute paths). Reconstruct an absolute view here when
    // the canonicalize step stripped the prefix; the original `canonical`
    // is preserved for the legacy fallback branch (`OutsideWorkspace`)
    // below so the previously-shipped wire form is unchanged for paths
    // not covered by the bound `LogicalWorkspaceView`.
    let absolute_canonical = if canonical.is_workspace_relative
        && let Some(root) = workspace_root_for_legacy
    {
        let trimmed_root = root.trim_end_matches('/');
        let trimmed_rel = canonical.path.trim_start_matches('/');
        let joined = if trimmed_rel.is_empty() {
            trimmed_root.to_string()
        } else {
            format!("{trimmed_root}/{trimmed_rel}")
        };
        CanonicalPath {
            path: joined,
            ..canonical.clone()
        }
    } else {
        canonical.clone()
    };
    let form = workspace_relative_form(&absolute_canonical, workspace, aggregate_workspace_paths);

    match form {
        WorkspaceRelativeForm::Excluded { canonical: full } => {
            // Criterion 6: opaque hash + excluded flag, regardless of preset.
            // The hash always covers the full canonical path so the same
            // excluded path always renders identically.
            let hash = hash_path(&full, hash_salt);
            Ok(RedactedPath {
                rendered: format!("<excluded>/[{hash}]"),
                excluded: true,
            })
        }
        WorkspaceRelativeForm::SourceRootRelative {
            source_root_id,
            relative,
        } => Ok(emit_workspace_relative(
            &source_root_id,
            &relative,
            hash_filenames,
            hash_salt,
            reveal_layout,
        )),
        WorkspaceRelativeForm::MemberFolderRelative {
            workspace_id_short,
            relative,
        } => Ok(emit_workspace_relative(
            &workspace_id_short,
            &relative,
            hash_filenames,
            hash_salt,
            reveal_layout,
        )),
        WorkspaceRelativeForm::OutsideWorkspace => {
            // Fall back to the legacy single-workspace pipeline so paths
            // outside the bound workspace continue to render as
            // `<external>/...` (or workspace-relative when the legacy
            // `workspace_root` happens to contain them).
            let rendered = if canonical.is_workspace_relative {
                redact_workspace_relative_path(
                    &canonical,
                    workspace_placeholder,
                    hash_filenames,
                    hash_salt,
                )
            } else {
                redact_external_path(&canonical, hash_filenames, hash_salt)
            };
            Ok(RedactedPath {
                rendered,
                excluded: false,
            })
        }
    }
}

/// Render a workspace-relative path token. Strict mode (i.e.
/// `hash_filenames = true`) hashes the **whole** prefixed token so the
/// workspace prefix never escapes in cleartext (criterion 7); minimal
/// mode emits the prefixed cleartext directly (criteria 4 + 5).
///
/// `reveal_layout` (issue #394 item 4, the `relative` preset) drops the
/// anonymizing `source_root_id` / `workspace_id_short` prefix and emits the
/// clean workspace-relative remainder. It only applies in the non-hashed
/// (`hash_filenames = false`) path: strict mode still hashes the whole token.
/// The remainder is always workspace-relative, so no absolute host path is
/// emitted; `reveal_layout` only sacrifices the multi-source-root
/// disambiguation that the prefix provides.
fn emit_workspace_relative(
    prefix: &str,
    relative: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
    reveal_layout: bool,
) -> RedactedPath {
    let token = if relative.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}/{relative}")
    };
    let rendered = if hash_filenames {
        // Strict pipeline: hash the entire prefixed token. The `prefix`
        // (source_root_id / workspace_id_short) is folded into the
        // SHA-256 input so the hashed token covers it (criterion 7).
        let hash = hash_path(&token, hash_salt);
        format!("<redacted>/[{hash}]")
    } else if reveal_layout {
        // Relative preset: drop the anonymizing prefix, emit the clean
        // workspace-relative remainder. The root itself (empty remainder)
        // renders as "." rather than an empty string.
        if relative.is_empty() {
            ".".to_string()
        } else {
            relative.to_string()
        }
    } else {
        token
    };
    RedactedPath {
        rendered,
        excluded: false,
    }
}

/// Result of [`redact_path_with_workspace`]: the emitted path plus a
/// wire-side `excluded` metadata flag.
///
/// The flag is set only for paths that matched a
/// `LogicalWorkspaceView::exclusions` entry; non-excluded paths always
/// carry `excluded: false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedPath {
    /// The redacted path string (workspace-relative, hashed, or `<excluded>`).
    pub rendered: String,
    /// `true` iff the input path matched an exclusion entry.
    pub excluded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_path_input() {
        assert!(validate_path_input("/home/user/file.rs").is_ok());
        assert!(matches!(validate_path_input(""), Err(PathError::EmptyPath)));
        assert!(matches!(
            validate_path_input("foo\0bar"),
            Err(PathError::NullByteInPath)
        ));
        assert!(matches!(
            validate_path_input(&"a".repeat(5000)),
            Err(PathError::PathTooLong { .. })
        ));
    }

    #[test]
    fn test_classify_unix_absolute() {
        let (path_type, normalized) = classify_and_normalize("/home/user/file.rs").unwrap();
        assert!(matches!(path_type, PathType::UnixAbsolute));
        assert_eq!(normalized, "/home/user/file.rs");
    }

    #[test]
    fn test_classify_windows_absolute() {
        let (path_type, normalized) = classify_and_normalize("C:\\Users\\file.rs").unwrap();
        assert!(matches!(
            path_type,
            PathType::WindowsAbsolute { drive: 'C' }
        ));
        assert_eq!(normalized, "C:/Users/file.rs");
    }

    #[test]
    fn test_classify_unc_path() {
        let (path_type, normalized) =
            classify_and_normalize("\\\\server\\share\\dir\\file.rs").unwrap();
        assert!(matches!(path_type, PathType::UncPath { .. }));
        assert_eq!(normalized, "/dir/file.rs");
    }

    #[test]
    fn test_classify_relative() {
        let (path_type, normalized) = classify_and_normalize("src/main.rs").unwrap();
        assert!(matches!(path_type, PathType::Relative));
        assert_eq!(normalized, "src/main.rs");
    }

    #[test]
    fn test_canonicalize_dot_components() {
        let canonical =
            canonicalize_for_hash("/home/user/./project/../project/src/main.rs", None).unwrap();
        assert_eq!(canonical.path, "/home/user/project/src/main.rs");
    }

    #[test]
    fn test_canonicalize_with_workspace() {
        let canonical =
            canonicalize_for_hash("/home/user/project/src/main.rs", Some("/home/user/project"))
                .unwrap();
        assert!(canonical.is_workspace_relative);
        assert_eq!(canonical.path, "/src/main.rs");
    }

    #[test]
    fn test_canonicalize_outside_workspace() {
        let canonical =
            canonicalize_for_hash("/home/other/file.rs", Some("/home/user/project")).unwrap();
        assert!(!canonical.is_workspace_relative);
        assert_eq!(canonical.path, "/home/other/file.rs");
    }

    #[test]
    fn test_canonicalize_workspace_boundary() {
        // /home/user should NOT match /home/username
        let canonical =
            canonicalize_for_hash("/home/username/file.rs", Some("/home/user")).unwrap();
        assert!(!canonical.is_workspace_relative);
    }

    #[test]
    fn test_canonicalize_workspace_escape() {
        // Escaping via .. should result in out-of-workspace
        let canonical = canonicalize_for_hash(
            "/home/user/project/../secret/file.rs",
            Some("/home/user/project"),
        )
        .unwrap();
        assert!(!canonical.is_workspace_relative);
        assert_eq!(canonical.path, "/home/user/secret/file.rs");
    }

    #[test]
    fn test_canonicalize_workspace_root() {
        let canonical =
            canonicalize_for_hash("/home/user/project", Some("/home/user/project")).unwrap();
        assert!(canonical.is_workspace_relative);
        assert_eq!(canonical.path, "/");
    }

    #[test]
    fn test_hash_path_deterministic() {
        let hash1 = hash_path("src/main.rs", None);
        let hash2 = hash_path("src/main.rs", None);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 8);
    }

    #[test]
    fn test_hash_path_different_paths() {
        let hash1 = hash_path("src/main.rs", None);
        let hash2 = hash_path("tests/main.rs", None);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_path_with_salt() {
        let hash1 = hash_path("src/main.rs", None);
        let hash2 = hash_path("src/main.rs", Some("mysalt"));
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_path_empty_salt() {
        // Empty salt should be normalized to None
        let hash1 = hash_path("src/main.rs", None);
        let hash2 = hash_path("src/main.rs", Some(""));
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_unc_escape_attempt() {
        let result = canonicalize_for_hash("\\\\server\\share\\..\\other", None);
        assert!(matches!(result, Err(PathError::UncEscapeAttempt)));
    }

    #[test]
    fn test_device_path_rejected() {
        let result = classify_and_normalize("\\\\.\\COM1");
        assert!(matches!(result, Err(PathError::DevicePath)));
    }

    #[test]
    fn test_redact_path_workspace_relative() {
        let result = redact_path(
            "/home/user/project/src/main.rs",
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        )
        .unwrap();
        assert_eq!(result, "src/main.rs");
    }

    #[test]
    fn test_redact_path_with_hash() {
        let result = redact_path(
            "/home/user/project/src/main.rs",
            Some("/home/user/project"),
            "<workspace>",
            true,
            None,
        )
        .unwrap();
        assert!(result.starts_with("<workspace>/["));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn test_redact_path_external() {
        let result = redact_path(
            "/other/path/file.rs",
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        )
        .unwrap();
        assert!(result.starts_with("<external>/"));
        assert!(result.contains("file.rs"));
    }
}
