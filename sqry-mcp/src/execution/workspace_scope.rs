//! Shared workspace + subtree resolution for MCP tool `path` arguments.
//!
//! Issue #394 Slice B Part 1: the per-tool `path` argument is overloaded. It can
//! be:
//!
//! - `"."` or empty: the ambient workspace (resolved from the request override or
//!   discovery), no subtree scope.
//! - an absolute directory that is itself an indexed workspace root: that
//!   workspace, no subtree scope (the pre-existing multi-workspace behaviour).
//! - a SUBDIRECTORY of the resolved workspace (a relative path like `rust`, or an
//!   absolute path inside the workspace): the owning workspace is resolved
//!   automatically and results are scoped to that subtree.
//!
//! Before this module, every tool resolved `path` only as a workspace root, so a
//! subdirectory like `rust` was canonicalised as if it were a workspace and the
//! request failed with "Could not resolve a workspace from the provided request
//! context". [`resolve_workspace_scope`] fixes that by separating the workspace
//! SELECTOR (fed to `engine_for_workspace`) from the SUBTREE (used to filter
//! results), while preserving the `"."` / absolute-root behaviour exactly.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::path_resolver::WorkspaceResolver;
use crate::workspace_session::current_workspace_override;

/// Resolved scope for a tool `path` argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceScope {
    /// Workspace selector to pass to `engine_for_workspace`. `None` means "use
    /// the ambient request override / discovery" (the `"."` case). `Some(root)`
    /// names a concrete workspace root. In request-override mode the engine
    /// ignores this and uses the override, which equals the resolved root, so the
    /// two stay consistent.
    pub selector: Option<PathBuf>,
    /// Normalised, forward-slash, workspace-relative subtree to scope results to.
    /// `None` means the whole workspace.
    pub subtree: Option<String>,
}

impl WorkspaceScope {
    /// Whole-workspace scope (ambient workspace, no subtree).
    fn whole() -> Self {
        Self {
            selector: None,
            subtree: None,
        }
    }
}

/// Resolve a tool `path` argument into a workspace selector and an optional
/// subtree filter.
///
/// # Errors
///
/// Returns an error when a relative subtree cannot be resolved against any
/// workspace, when a path escapes the resolved workspace root, or when an
/// absolute path neither lives inside the ambient workspace nor resolves as its
/// own workspace root.
pub(crate) fn resolve_workspace_scope(path: &str) -> Result<WorkspaceScope> {
    // Ambient workspace: the request override (authoritative in daemon/server
    // mode) takes precedence; otherwise fall back to env/CWD discovery. May be
    // absent in bare CLI contexts, which is handled per-branch in `classify_within`.
    let ambient =
        current_workspace_override().or_else(|| WorkspaceResolver::new(None).resolve().ok());
    classify_within(path, ambient.as_deref())
}

/// Pure classification core for [`resolve_workspace_scope`], with the ambient
/// workspace root supplied explicitly so it is deterministically testable
/// (no thread-local override or CWD discovery).
fn classify_within(path: &str, ambient: Option<&Path>) -> Result<WorkspaceScope> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(WorkspaceScope::whole());
    }

    let candidate = Path::new(trimmed);

    if candidate.is_absolute() {
        let canonical = candidate
            .canonicalize()
            .map_err(|e| anyhow!("path `{trimmed}` does not exist or is not accessible: {e}"))?;

        // Inside the ambient workspace -> scope to the subtree.
        if let Some(root) = ambient
            && let Ok(relative) = canonical.strip_prefix(root)
        {
            return Ok(scope_for_relative(root, relative));
        }

        // Otherwise treat the path as its own workspace root (multi-workspace
        // behaviour); this validates that it is an indexed workspace.
        let root = crate::path_resolver::resolve_workspace_path(trimmed)?;
        return Ok(WorkspaceScope {
            selector: Some(root),
            subtree: None,
        });
    }

    // Relative path: a subtree of the ambient workspace.
    let root = ambient.ok_or_else(|| {
        anyhow!(
            "cannot resolve a workspace to scope `{trimmed}` against; \
             pass an absolute path or open the workspace first"
        )
    })?;
    let joined = root.join(candidate);
    let canonical = joined.canonicalize().map_err(|e| {
        anyhow!(
            "subtree `{trimmed}` was not found under workspace `{}`: {e}",
            root.display()
        )
    })?;
    let relative = canonical.strip_prefix(root).map_err(|_| {
        anyhow!(
            "path `{trimmed}` escapes workspace root `{}`",
            root.display()
        )
    })?;
    Ok(scope_for_relative(root, relative))
}

/// Resolve only the workspace selector for a tool `path`, discarding any subtree.
///
/// For tools that are NodeId-anchored or single-symbol (not per-file/per-symbol
/// list scans) a subtree filter is not meaningful, but they must still resolve a
/// subdirectory `path` to its owning workspace instead of failing. This is the
/// drop-in replacement for the former per-module `resolve_workspace_path`.
///
/// # Errors
///
/// Propagates [`resolve_workspace_scope`] errors (invalid path, escape, etc.).
pub(crate) fn resolve_workspace_selector(path: &str) -> Result<Option<PathBuf>> {
    Ok(resolve_workspace_scope(path)?.selector)
}

/// Build a scope for a path already known to be inside `root`. A path that
/// strips to empty (the root itself) yields a whole-workspace scope.
fn scope_for_relative(root: &Path, relative: &Path) -> WorkspaceScope {
    let normalized = normalize_subtree(relative);
    if normalized.is_empty() {
        WorkspaceScope {
            selector: Some(root.to_path_buf()),
            subtree: None,
        }
    } else {
        WorkspaceScope {
            selector: Some(root.to_path_buf()),
            subtree: Some(normalized),
        }
    }
}

/// Normalise a relative path to a forward-slash string with no leading `./` or
/// trailing slash.
fn normalize_subtree(relative: &Path) -> String {
    let raw = relative.to_string_lossy();
    let forward = if cfg!(windows) {
        raw.replace('\\', "/")
    } else {
        raw.into_owned()
    };
    forward
        .trim_start_matches("./")
        .trim_matches('/')
        .to_string()
}

/// Derive the workspace-relative subtree for a tool `path` against an
/// already-resolved workspace root.
///
/// Unlike [`resolve_workspace_scope`], this does no workspace discovery: the
/// caller already holds the canonical workspace root (e.g. a daemon
/// `WorkspaceContext` or `engine.workspace_root()`), so this only classifies the
/// `path` into "whole workspace" (`None`) vs a subtree (`Some(rel)`). Used by the
/// shared `inner::` tool bodies so standalone and daemon-hosted execution scope
/// identically.
///
/// Returns `None` (whole workspace) for "", ".", the root itself, an absolute
/// path equal to the root, or any path that does not resolve to an existing
/// location inside `root` (callers that need a hard error use
/// [`resolve_workspace_scope`] at the request boundary instead).
#[must_use]
pub(crate) fn subtree_within(path: &str, root: &Path) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return None;
    }
    let candidate = Path::new(trimmed);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let canonical = joined.canonicalize().ok()?;
    let relative = canonical.strip_prefix(root).ok()?;
    let normalized = normalize_subtree(relative);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Whether a workspace-relative path falls within `subtree`.
///
/// Both arguments are forward-slash workspace-relative paths. A directory
/// `subtree` matches itself and everything beneath it. This is the directory
/// semantics of the tool `path` argument; glob-style scoping is available
/// through the query `path:` / `in:` predicate.
#[must_use]
pub(crate) fn path_in_subtree(relative_path: &str, subtree: &str) -> bool {
    let subtree = subtree.trim_matches('/');
    if subtree.is_empty() {
        return true;
    }
    relative_path == subtree || relative_path.starts_with(&format!("{subtree}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_and_empty_are_whole_workspace() {
        assert_eq!(
            resolve_workspace_scope(".").unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            resolve_workspace_scope("").unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            resolve_workspace_scope("   ").unwrap(),
            WorkspaceScope::whole()
        );
    }

    #[test]
    fn path_in_subtree_matches_directory_and_descendants() {
        assert!(path_in_subtree("rust", "rust"));
        assert!(path_in_subtree("rust/kernel/time.rs", "rust"));
        assert!(path_in_subtree("rust/kernel/time.rs", "rust/kernel"));
        assert!(!path_in_subtree("rustfmt.toml", "rust"));
        assert!(!path_in_subtree("drivers/rust/x.rs", "rust"));
        assert!(path_in_subtree("anything", ""));
    }

    #[test]
    fn normalize_subtree_strips_prefix_and_trailing() {
        assert_eq!(normalize_subtree(Path::new("rust/kernel")), "rust/kernel");
        assert_eq!(normalize_subtree(Path::new("")), "");
    }

    #[test]
    fn subtree_within_covers_the_path_shapes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("rust/kernel")).unwrap();
        std::fs::create_dir_all(root.join("drivers")).unwrap();

        // Relative subdirectory -> scoped.
        assert_eq!(subtree_within("rust", &root).as_deref(), Some("rust"));
        assert_eq!(
            subtree_within("rust/kernel", &root).as_deref(),
            Some("rust/kernel")
        );

        // Absolute path inside the workspace -> scoped to the relative subtree.
        let abs = root.join("rust/kernel");
        assert_eq!(
            subtree_within(&abs.to_string_lossy(), &root).as_deref(),
            Some("rust/kernel")
        );

        // The root itself, ".", and "" -> whole workspace (None).
        assert_eq!(subtree_within(".", &root), None);
        assert_eq!(subtree_within("", &root), None);
        assert_eq!(subtree_within(&root.to_string_lossy(), &root), None);

        // Nonexistent subtree -> None (callers that need a hard error use
        // resolve_workspace_scope at the request boundary).
        assert_eq!(subtree_within("does-not-exist", &root), None);

        // A sibling that merely shares a name prefix is not matched by
        // path_in_subtree, even though it resolves under root.
        assert!(!path_in_subtree("drivers/rust/x.rs", "rust"));
    }

    #[test]
    fn classify_within_resolution_arms_and_escape_rejection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("rust/kernel")).unwrap();
        let outside = tempfile::tempdir().expect("outside tempdir");
        let outside_root = outside.path().canonicalize().expect("canonical outside");

        // "." / empty -> whole workspace.
        assert_eq!(
            classify_within(".", Some(&root)).unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            classify_within("", Some(&root)).unwrap(),
            WorkspaceScope::whole()
        );

        // Relative subdirectory -> selector = root, subtree = the subdir.
        let scoped = classify_within("rust/kernel", Some(&root)).unwrap();
        assert_eq!(scoped.selector.as_deref(), Some(root.as_path()));
        assert_eq!(scoped.subtree.as_deref(), Some("rust/kernel"));

        // Absolute path inside the ambient workspace -> scoped to the subtree.
        let abs = root.join("rust");
        let scoped_abs = classify_within(&abs.to_string_lossy(), Some(&root)).unwrap();
        assert_eq!(scoped_abs.selector.as_deref(), Some(root.as_path()));
        assert_eq!(scoped_abs.subtree.as_deref(), Some("rust"));

        // Absolute path equal to the root -> whole workspace.
        let at_root = classify_within(&root.to_string_lossy(), Some(&root)).unwrap();
        assert_eq!(at_root.subtree, None);

        // `../` escape: a relative path that resolves outside the workspace root
        // is rejected with a clear error (fails closed).
        let escape = format!("../{}", outside_root.file_name().unwrap().to_string_lossy());
        let err = classify_within(&escape, Some(&root))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("escapes workspace root") || err.contains("was not found"),
            "unexpected escape error: {err}"
        );

        // Nonexistent relative subtree -> hard error (not a silent whole-workspace).
        let missing = classify_within("does-not-exist", Some(&root)).unwrap_err();
        assert!(missing.to_string().contains("was not found"));

        // Relative path with no ambient workspace -> clear "cannot resolve" error.
        let no_ws = classify_within("rust", None).unwrap_err().to_string();
        assert!(no_ws.contains("cannot resolve a workspace"));
    }
}
