//! Standalone MCP request-boundary adapter over the shared workspace + subtree
//! scope logic in [`sqry_core::workspace::scope`].
//!
//! Issue #394 Slice B introduced this module locally; Part 1b hoisted the pure,
//! reusable primitives ([`WorkspaceScope`], [`subtree_within`],
//! [`path_in_subtree`], `classify_within`) into `sqry-core` so the daemon
//! acquirer (which cannot depend on `sqry-mcp`) can share the same resolution.
//! What stays here is the standalone-server discovery: reading the request
//! override or CWD to find the ambient workspace, and validating an out-of-tree
//! absolute path as its own workspace root. Standalone behaviour is byte-
//! identical to the pre-hoist module: the same code, relocated and delegated.
//!
//! Issue #469 layers logical-workspace exclusion enforcement (Surface 2) on
//! top of that hoisted core: the core `classify_within` / `subtree_within`
//! stay policy-free (the daemon acquirer has no notion of per-request
//! exclusions), and this module rejects a resolved scope or subtree that
//! matches the per-request `LogicalWorkspace` exclusion policy, using the same
//! `RpcError::validation` mapping as Surface 1
//! (`canonicalize_in_workspace_enforced`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use sqry_core::workspace::LogicalWorkspace;

// Re-export the shared pure primitives so every existing
// `crate::execution::workspace_scope::X` call site keeps compiling unchanged.
// `subtree_within` is NOT re-exported: it is redefined below as a thin
// enforcement wrapper over the core primitive (issue #469 Surface 2).
pub(crate) use sqry_core::workspace::scope::{WorkspaceScope, path_in_subtree};

use crate::engine::{WorkspacePathError, excluded_to_rpc, exclusion_matches};
use crate::path_resolver::WorkspaceResolver;
use crate::workspace_session::{current_logical_workspace, current_workspace_override};

/// Resolve a tool `path` argument into a workspace selector and an optional
/// subtree filter.
///
/// The request override (authoritative in daemon/server mode) takes precedence
/// as the ambient workspace; otherwise env/CWD discovery is used. The absolute
/// out-of-tree fallback validates the path as its own indexed workspace root via
/// [`crate::path_resolver::resolve_workspace_path`].
///
/// # Errors
///
/// Returns an error when a relative subtree cannot be resolved against any
/// workspace, when a path escapes the resolved workspace root, when an
/// absolute path neither lives inside the ambient workspace nor resolves as its
/// own workspace root, or when the resolved scope matches a `LogicalWorkspace`
/// exclusion (issue #469 Surface 2).
pub(crate) fn resolve_workspace_scope(path: &str) -> Result<WorkspaceScope> {
    // Ambient workspace: the request override (authoritative in daemon/server
    // mode) takes precedence; otherwise fall back to env/CWD discovery. May be
    // absent in bare CLI contexts, which the core classifier handles per-branch.
    let ambient =
        current_workspace_override().or_else(|| WorkspaceResolver::new(None).resolve().ok());
    let scope = sqry_core::workspace::scope::classify_within(path, ambient.as_deref(), |p| {
        crate::path_resolver::resolve_workspace_path(p)
    })?;
    // Surface 2 enforcement (issue #469): the per-request `LogicalWorkspace`
    // (bound on the blocking-thread thread-local) carries the exclusion
    // policy. When absent (bare CLI, unbound tests) or empty, scope behaviour
    // is byte-identical to before this change.
    let logical = current_logical_workspace();
    reject_scope_if_excluded(&scope, ambient.as_deref(), logical.as_deref())?;
    Ok(scope)
}

/// Reject a resolved [`WorkspaceScope`] that stays inside the ambient
/// workspace (selector == ambient root, whether whole-root or scoped to a
/// subtree) when it matches a `LogicalWorkspace` exclusion.
///
/// The shared core `classify_within` is policy-free (it is also used by the
/// daemon acquirer, which has no notion of per-request exclusions), so this
/// enforcement stays here at the sqry-mcp request boundary, where the
/// per-request `LogicalWorkspace` and `RpcError` mapping live. A path resolved
/// as its OWN separate workspace root (selector != ambient) carries that other
/// workspace's own policy and is untouched here; the whole-ambient-workspace
/// case (selector `None`) is also untouched. Both match pre-hoist behaviour
/// byte-for-byte.
fn reject_scope_if_excluded(
    scope: &WorkspaceScope,
    ambient: Option<&Path>,
    logical: Option<&LogicalWorkspace>,
) -> Result<()> {
    if scope.selector.as_deref() != ambient {
        return Ok(());
    }
    let Some(root) = scope.selector.as_deref() else {
        return Ok(());
    };
    let target = match scope.subtree.as_deref() {
        Some(subtree) => root.join(subtree),
        None => root.to_path_buf(),
    };
    reject_if_excluded(&target, logical)
}

/// Reject a resolved subtree that matches a `LogicalWorkspace` exclusion.
///
/// Uses the same exact-or-descendant [`exclusion_matches`] precedence check and
/// the same [`WorkspacePathError::Excluded`] -> [`crate::error::RpcError::validation`]
/// mapping as Surface 1 (`canonicalize_in_workspace_enforced`), so an excluded
/// subtree surfaces the identical "excluded by the logical workspace policy"
/// `invalid_params` error instead of being scoped into. A `None` policy or empty
/// exclusions is a no-op (byte-identical to pre-#469 behaviour).
fn reject_if_excluded(canonical: &Path, logical: Option<&LogicalWorkspace>) -> Result<()> {
    if let Some(ws) = logical
        && exclusion_matches(canonical, ws)
    {
        return Err(excluded_to_rpc(WorkspacePathError::Excluded {
            path: canonical.to_path_buf(),
        }));
    }
    Ok(())
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

/// Resolve a workspace selector for a tool `path`, propagating a
/// `LogicalWorkspace` exclusion rejection while preserving the historical
/// lenient fallback for paths that merely fail to resolve.
///
/// The per-module tool bodies use `path` to *select* a workspace and, when it
/// names neither a workspace root nor a resolvable subtree, fall through to
/// ambient discovery (a nonexistent or non-workspace `path` yields `Ok(None)`).
/// Issue #469 requires that an *excluded* `path` is not silently downgraded to
/// that fallback: the `RpcError::validation` exclusion rejection raised by
/// [`resolve_workspace_scope`] is propagated so the tool fails closed with
/// `invalid_params` (`-32602`), while every other resolution error keeps the
/// pre-#469 `None` discovery fallback. This is the drop-in the per-module
/// `resolve_workspace_path` helpers delegate to (Surface 2, issue #469).
///
/// # Errors
///
/// Returns the exclusion `RpcError::validation` (`-32602`) when `path` matches a
/// `LogicalWorkspace` exclusion bound on the per-request thread-local.
pub(crate) fn resolve_workspace_selector_enforced(path: &str) -> Result<Option<PathBuf>> {
    match resolve_workspace_selector(path) {
        Ok(selector) => Ok(selector),
        Err(err) if err.downcast_ref::<crate::error::RpcError>().is_some() => Err(err),
        Err(_) => Ok(None),
    }
}

/// Derive the workspace-relative subtree for a tool `path` against an
/// already-resolved workspace root.
///
/// Unlike [`resolve_workspace_scope`], this does no workspace discovery: the
/// caller already holds the canonical workspace root (e.g. a daemon
/// `WorkspaceContext` or `engine.workspace_root()`), so this only classifies the
/// `path` into "whole workspace" (`None`) vs a subtree (`Some(rel)`). Used by the
/// shared `inner::` tool bodies so standalone and daemon-hosted execution scope
/// identically. Delegates the pure classification to
/// [`sqry_core::workspace::scope::subtree_within`] and layers the Surface 2
/// exclusion check on top.
///
/// Returns `Ok(None)` (whole workspace) for "", ".", the root itself, an
/// absolute path equal to the root, or any path that does not resolve to an
/// existing location inside `root` (callers that need a hard error use
/// [`resolve_workspace_scope`] at the request boundary instead).
///
/// # Errors
///
/// Returns the same `RpcError::validation` ("excluded by the logical workspace
/// policy") as Surface 1 when the resolved subtree matches a
/// [`LogicalWorkspace`] exclusion bound on the per-request thread-local (issue
/// #469 Surface 2). This is the enforcement point for daemon-hosted tool bodies
/// (`complexity_metrics`, `find_unused`) that reach the inner body without
/// passing through [`resolve_workspace_scope`]. With no `LogicalWorkspace` bound
/// or empty exclusions the result is byte-identical to the pre-#469 behaviour.
pub(crate) fn subtree_within(path: &str, root: &Path) -> Result<Option<String>> {
    let Some(normalized) = sqry_core::workspace::scope::subtree_within(path, root) else {
        return Ok(None);
    };
    // Surface 2 enforcement (issue #469): an excluded subtree is rejected with
    // the same error as Surface 1 rather than being scoped into. `root` is
    // already canonical (an invariant of every caller) and `normalized` is the
    // exact relative-path components `subtree_within` stripped from it, so
    // rejoining reconstructs the same canonical target the core primitive
    // resolved internally.
    let canonical = root.join(&normalized);
    reject_if_excluded(&canonical, current_logical_workspace().as_deref())?;
    Ok(Some(normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_and_empty_are_whole_workspace() {
        // Delegation smoke test: the standalone entrypoint routes "." / "" to
        // the shared classifier, which returns a whole-workspace scope without
        // needing an ambient workspace.
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
    fn selector_is_none_for_ambient_scope() {
        // `resolve_workspace_selector` threads through `resolve_workspace_scope`;
        // the ambient ("." ) case yields no selector.
        assert_eq!(resolve_workspace_selector(".").unwrap(), None);
    }

    #[test]
    fn re_exported_primitives_are_the_core_ones() {
        // The pure primitives are the shared core implementations (delegation,
        // not a fork). Exercise them through the re-export.
        assert!(path_in_subtree("rust/kernel/time.rs", "rust"));
        assert!(!path_in_subtree("drivers/rust/x.rs", "rust"));
        assert!(path_in_subtree("anything", ""));
    }

    #[test]
    fn subtree_within_covers_the_path_shapes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("rust/kernel")).unwrap();
        std::fs::create_dir_all(root.join("drivers")).unwrap();

        // Relative subdirectory -> scoped.
        assert_eq!(
            subtree_within("rust", &root).unwrap().as_deref(),
            Some("rust")
        );
        assert_eq!(
            subtree_within("rust/kernel", &root).unwrap().as_deref(),
            Some("rust/kernel")
        );

        // Absolute path inside the workspace -> scoped to the relative subtree.
        let abs = root.join("rust/kernel");
        assert_eq!(
            subtree_within(&abs.to_string_lossy(), &root)
                .unwrap()
                .as_deref(),
            Some("rust/kernel")
        );

        // The root itself, ".", and "" -> whole workspace (None).
        assert_eq!(subtree_within(".", &root).unwrap(), None);
        assert_eq!(subtree_within("", &root).unwrap(), None);
        assert_eq!(
            subtree_within(&root.to_string_lossy(), &root).unwrap(),
            None
        );

        // Nonexistent subtree -> None (callers that need a hard error use
        // resolve_workspace_scope at the request boundary).
        assert_eq!(subtree_within("does-not-exist", &root).unwrap(), None);

        // A sibling that merely shares a name prefix is not matched by
        // path_in_subtree, even though it resolves under root.
        assert!(!path_in_subtree("drivers/rust/x.rs", "rust"));
    }

    /// Inject an exclusion into a `LogicalWorkspace` via a public-API-only
    /// serde round trip (the constructors do not expose a "single root +
    /// exclusions" seam). Same helper the integration tests use.
    fn inject_exclusion(workspace: &LogicalWorkspace, excluded: &Path) -> LogicalWorkspace {
        let mut value: serde_json::Value =
            serde_json::to_value(workspace).expect("workspace -> json");
        let exclusions = value
            .get_mut("exclusions")
            .and_then(serde_json::Value::as_array_mut)
            .expect("LogicalWorkspace must serialize an `exclusions` array");
        exclusions.push(serde_json::Value::String(
            excluded.to_string_lossy().into_owned(),
        ));
        serde_json::from_value(value).expect("json -> workspace")
    }

    fn assert_excluded_rpc(err: &anyhow::Error) {
        let rpc = err
            .downcast_ref::<crate::error::RpcError>()
            .unwrap_or_else(|| panic!("expected RpcError, got {err:?}"));
        assert_eq!(
            rpc.code, -32602,
            "excluded subtree must surface invalid_params"
        );
        assert!(
            rpc.message
                .contains("excluded by the logical workspace policy"),
            "unexpected message: {}",
            rpc.message
        );
    }

    /// TC8 (issue #469): Surface 2 rejects an excluded subtree with the same
    /// `invalid_params` (`-32602`) error as Surface 1, instead of scoping into
    /// it. Covers both the request-boundary rejection
    /// (`reject_scope_if_excluded`, with the resolved `WorkspaceScope` and
    /// `LogicalWorkspace` supplied explicitly) and the daemon-hosted inner seam
    /// (`subtree_within`, reading the per-request thread-local). A `None`
    /// policy or empty exclusions is byte-identical to the pre-#469 behaviour.
    #[test]
    fn tc8_workspace_scope_rejects_excluded_subtree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let secrets = root.join("secrets").canonicalize().unwrap();

        let base = LogicalWorkspace::single_root(root.clone()).expect("single_root");
        let ws = inject_exclusion(&base, &secrets);

        // reject_scope_if_excluded (request boundary): excluded relative
        // subtree rejected.
        let excluded_scope = WorkspaceScope {
            selector: Some(root.clone()),
            subtree: Some("secrets".to_string()),
        };
        let err = reject_scope_if_excluded(&excluded_scope, Some(&root), Some(&ws))
            .expect_err("excluded subtree must be rejected at the request boundary");
        assert_excluded_rpc(&err);

        // A non-excluded subtree still scopes normally.
        let allowed_scope = WorkspaceScope {
            selector: Some(root.clone()),
            subtree: Some("src".to_string()),
        };
        reject_scope_if_excluded(&allowed_scope, Some(&root), Some(&ws)).expect("allowed subtree");

        // subtree_within (daemon-hosted inner seam): reads the thread-local.
        let ws = std::sync::Arc::new(ws);
        let err = crate::workspace_session::with_workspace_override(
            Some(&root),
            Some(ws.clone()),
            || subtree_within("secrets", &root),
        )
        .expect_err("excluded subtree must be rejected on the inner seam");
        assert_excluded_rpc(&err);

        // Allowed subtree passes through the inner seam under the same binding.
        let ok = crate::workspace_session::with_workspace_override(Some(&root), Some(ws), || {
            subtree_within("src", &root)
        })
        .expect("allowed subtree must pass the inner seam");
        assert_eq!(ok.as_deref(), Some("src"));
    }

    /// TC8 (thread-local seam): `resolve_workspace_scope` and its
    /// `resolve_workspace_selector_enforced` wrapper read BOTH thread-locals
    /// (`current_workspace_override` for the ambient root and
    /// `current_logical_workspace` for the exclusion policy). This drives the
    /// real request-boundary entrypoints (not the explicit-arg
    /// `reject_scope_if_excluded` seam TC8 above uses), so a dropped
    /// thread-local read regresses this test even though
    /// `reject_scope_if_excluded` would still pass.
    #[test]
    fn tc8_resolve_workspace_scope_reads_thread_locals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let secrets = root.join("secrets").canonicalize().unwrap();

        let base = LogicalWorkspace::single_root(root.clone()).expect("single_root");
        let ws = std::sync::Arc::new(inject_exclusion(&base, &secrets));

        // Excluded relative subtree, resolved purely from the bound thread-locals.
        let err = crate::workspace_session::with_workspace_override(
            Some(&root),
            Some(ws.clone()),
            || resolve_workspace_scope("secrets"),
        )
        .expect_err("resolve_workspace_scope must reject an excluded subtree");
        assert_excluded_rpc(&err);

        // The enforced selector wrapper (the per-module helpers' delegate)
        // propagates the same exclusion rejection rather than swallowing it.
        let err = crate::workspace_session::with_workspace_override(
            Some(&root),
            Some(ws.clone()),
            || resolve_workspace_selector_enforced("secrets"),
        )
        .expect_err("resolve_workspace_selector_enforced must propagate the exclusion");
        assert_excluded_rpc(&err);

        // An allowed subtree resolves to a scoped result under the same binding.
        let scope =
            crate::workspace_session::with_workspace_override(Some(&root), Some(ws), || {
                resolve_workspace_scope("src")
            })
            .expect("allowed subtree must resolve");
        assert_eq!(scope.subtree.as_deref(), Some("src"));
    }

    /// TC8 parity: with no `LogicalWorkspace` bound (or an empty-exclusions
    /// policy) both Surface 2 entrypoints behave exactly as before #469.
    #[test]
    fn tc8_no_binding_or_empty_exclusions_is_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("secrets")).unwrap();

        let resolve_as_root = |p: &str| Path::new(p).canonicalize().map_err(anyhow::Error::from);

        // No policy: scoping into `secrets` succeeds.
        let scoped =
            sqry_core::workspace::scope::classify_within("secrets", Some(&root), resolve_as_root)
                .expect("no-policy scope");
        reject_scope_if_excluded(&scoped, Some(&root), None).expect("no-policy allowed");
        assert_eq!(scoped.subtree.as_deref(), Some("secrets"));

        // Empty-exclusions policy: identical result.
        let empty = LogicalWorkspace::single_root(root.clone()).expect("single_root");
        let scoped =
            sqry_core::workspace::scope::classify_within("secrets", Some(&root), resolve_as_root)
                .expect("empty scope");
        reject_scope_if_excluded(&scoped, Some(&root), Some(&empty))
            .expect("empty-exclusions allowed");
        assert_eq!(scoped.subtree.as_deref(), Some("secrets"));

        // Inner seam with no thread-local binding is unchanged.
        assert_eq!(
            subtree_within("secrets", &root).unwrap().as_deref(),
            Some("secrets")
        );
    }
}
