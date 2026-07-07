//! Shared workspace + subtree resolution for tool `path` arguments.
//!
//! Issue #394 Slice B / Part 1b: the per-tool `path` argument is overloaded. It
//! can be:
//!
//! - `"."` or empty: the ambient workspace (resolved from the request override
//!   or discovery), no subtree scope.
//! - an absolute directory that is itself an indexed workspace root: that
//!   workspace, no subtree scope (the pre-existing multi-workspace behaviour).
//! - a SUBDIRECTORY of the resolved workspace (a relative path like `rust`, or an
//!   absolute path inside the workspace): the owning workspace is resolved
//!   automatically and results are scoped to that subtree.
//!
//! These primitives are pure (no I/O beyond `canonicalize`, no thread-locals) so
//! they can be shared across the standalone MCP server, the daemon acquirer, and
//! any other surface that resolves a tool `path`. `sqry-daemon` cannot depend on
//! `sqry-mcp`, so the reusable logic lives here in `sqry-core` and `sqry-mcp`
//! delegates to it (Part 1b hoist), keeping standalone behaviour byte-identical.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

/// Resolved scope for a tool `path` argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceScope {
    /// Workspace selector to pass to the engine resolver. `None` means "use the
    /// ambient request override / discovery" (the `"."` case). `Some(root)`
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
    #[must_use]
    pub fn whole() -> Self {
        Self {
            selector: None,
            subtree: None,
        }
    }
}

/// Pure classification core: split a tool `path` argument into a workspace
/// selector and an optional subtree filter, with the ambient workspace root
/// supplied explicitly so it is deterministically testable (no thread-local
/// override or CWD discovery).
///
/// `resolve_as_root` supplies the "treat an absolute path as its own workspace
/// root" fallback used when an absolute `path` is not inside the ambient
/// workspace. Callers pass their own workspace-root validator (the standalone
/// MCP server passes its `path_resolver::resolve_workspace_path`); this keeps
/// the core free of surface-specific path policy.
///
/// # Errors
///
/// Returns an error when a relative subtree cannot be resolved against any
/// workspace, when a path escapes the resolved workspace root, or when an
/// absolute path neither lives inside the ambient workspace nor resolves as its
/// own workspace root (per `resolve_as_root`).
pub fn classify_within<F>(
    path: &str,
    ambient: Option<&Path>,
    resolve_as_root: F,
) -> Result<WorkspaceScope>
where
    F: FnOnce(&str) -> Result<PathBuf>,
{
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
        // behaviour); the caller-supplied resolver validates that it is an
        // indexed workspace.
        let root = resolve_as_root(trimmed)?;
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
/// Unlike [`classify_within`], this does no workspace discovery: the caller
/// already holds the canonical workspace root (for example a daemon
/// `WorkspaceContext` or `engine.workspace_root()`), so this only classifies the
/// `path` into "whole workspace" (`None`) vs a subtree (`Some(rel)`). Used by the
/// shared `inner::` tool bodies so standalone and daemon-hosted execution scope
/// identically.
///
/// Returns `None` (whole workspace) for "", ".", the root itself, an absolute
/// path equal to the root, or any path that does not resolve to an existing
/// location inside `root` (callers that need a hard error use [`classify_within`]
/// at the request boundary instead).
#[must_use]
pub fn subtree_within(path: &str, root: &Path) -> Option<String> {
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
pub fn path_in_subtree(relative_path: &str, subtree: &str) -> bool {
    let subtree = subtree.trim_matches('/');
    if subtree.is_empty() {
        return true;
    }
    relative_path == subtree || relative_path.starts_with(&format!("{subtree}/"))
}

/// Resolve a canonical `target` path to the longest registered workspace root
/// that contains it (the "owning" workspace).
///
/// `candidates` is the set of canonical registered workspace roots. A root owns
/// `target` when it is an ancestor-or-equal of `target`. [`Path::starts_with`]
/// is component-wise, so `/a/bc` is NOT owned by `/a/b`. When several roots are
/// ancestors (nested workspaces), the longest (most specific) one wins. When the
/// `target` is itself a registered root, the longest ancestor is that root, so
/// the resolution is a no-op.
///
/// Returns `None` when no candidate root contains `target` (the caller then
/// keeps `target` as-is and lets classification surface the existing
/// "not loaded" error for an unknown path).
#[must_use]
pub fn owning_workspace_root<'a, I>(target: &Path, candidates: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    candidates
        .into_iter()
        .filter(|root| target.starts_with(root))
        .max_by_key(|root| root.components().count())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolver stub for `classify_within` tests: canonicalises the path as its
    /// own root (never exercised by the arms tested here, which stay inside the
    /// ambient workspace, but keeps the closure honest).
    fn canonicalizing_resolver(p: &str) -> Result<PathBuf> {
        Path::new(p)
            .canonicalize()
            .map_err(|e| anyhow!("resolve_as_root: {e}"))
    }

    #[test]
    fn dot_and_empty_are_whole_workspace() {
        assert_eq!(
            classify_within(".", None, canonicalizing_resolver).unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            classify_within("", None, canonicalizing_resolver).unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            classify_within("   ", None, canonicalizing_resolver).unwrap(),
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
        // classify_within at the request boundary).
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
            classify_within(".", Some(&root), canonicalizing_resolver).unwrap(),
            WorkspaceScope::whole()
        );
        assert_eq!(
            classify_within("", Some(&root), canonicalizing_resolver).unwrap(),
            WorkspaceScope::whole()
        );

        // Relative subdirectory -> selector = root, subtree = the subdir.
        let scoped = classify_within("rust/kernel", Some(&root), canonicalizing_resolver).unwrap();
        assert_eq!(scoped.selector.as_deref(), Some(root.as_path()));
        assert_eq!(scoped.subtree.as_deref(), Some("rust/kernel"));

        // Absolute path inside the ambient workspace -> scoped to the subtree.
        let abs = root.join("rust");
        let scoped_abs =
            classify_within(&abs.to_string_lossy(), Some(&root), canonicalizing_resolver).unwrap();
        assert_eq!(scoped_abs.selector.as_deref(), Some(root.as_path()));
        assert_eq!(scoped_abs.subtree.as_deref(), Some("rust"));

        // Absolute path equal to the root -> whole workspace.
        let at_root = classify_within(
            &root.to_string_lossy(),
            Some(&root),
            canonicalizing_resolver,
        )
        .unwrap();
        assert_eq!(at_root.subtree, None);

        // Absolute path outside the ambient workspace -> resolver treats it as
        // its own workspace root (selector = that root, no subtree).
        let outside_scope = classify_within(
            &outside_root.to_string_lossy(),
            Some(&root),
            canonicalizing_resolver,
        )
        .unwrap();
        assert_eq!(
            outside_scope.selector.as_deref(),
            Some(outside_root.as_path())
        );
        assert_eq!(outside_scope.subtree, None);

        // `../` escape: a relative path that resolves outside the workspace root
        // is rejected with a clear error (fails closed).
        let escape = format!("../{}", outside_root.file_name().unwrap().to_string_lossy());
        let err = classify_within(&escape, Some(&root), canonicalizing_resolver)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("escapes workspace root") || err.contains("was not found"),
            "unexpected escape error: {err}"
        );

        // Nonexistent relative subtree -> hard error (not a silent whole-workspace).
        let missing =
            classify_within("does-not-exist", Some(&root), canonicalizing_resolver).unwrap_err();
        assert!(missing.to_string().contains("was not found"));

        // Relative path with no ambient workspace -> clear "cannot resolve" error.
        let no_ws = classify_within("rust", None, canonicalizing_resolver)
            .unwrap_err()
            .to_string();
        assert!(no_ws.contains("cannot resolve a workspace"));
    }

    #[test]
    fn owning_workspace_root_resolves_longest_ancestor() {
        let a = PathBuf::from("/srv/repos/alpha");
        let a_nested = PathBuf::from("/srv/repos/alpha/vendor/beta");
        let b = PathBuf::from("/srv/repos/beta");

        let roots = [a.as_path(), a_nested.as_path(), b.as_path()];

        // Exact root -> itself.
        assert_eq!(
            owning_workspace_root(Path::new("/srv/repos/alpha"), roots),
            Some(a.clone())
        );

        // Subtree of alpha -> alpha.
        assert_eq!(
            owning_workspace_root(Path::new("/srv/repos/alpha/kernel/time.rs"), roots),
            Some(a.clone())
        );

        // Nested workspaces: deepest ancestor wins.
        assert_eq!(
            owning_workspace_root(Path::new("/srv/repos/alpha/vendor/beta/src/x.rs"), roots),
            Some(a_nested.clone())
        );

        // Sibling that shares a name prefix is NOT owned (component-wise).
        assert_eq!(
            owning_workspace_root(Path::new("/srv/repos/alphabet/x.rs"), roots),
            None
        );

        // Path under no registered root.
        assert_eq!(
            owning_workspace_root(Path::new("/other/place/x.rs"), roots),
            None
        );

        // Empty candidate set.
        assert_eq!(
            owning_workspace_root(Path::new("/srv/repos/alpha"), std::iter::empty()),
            None
        );
    }
}
