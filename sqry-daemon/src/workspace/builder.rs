//! Workspace graph builder abstraction.
//!
//! [`WorkspaceBuilder`] is the dependency-injection seam between the
//! daemon's workspace manager and sqry-core's full graph-build
//! pipeline. Production code wraps
//! [`sqry_core::graph::unified::build::build_unified_graph`] via
//! [`RealWorkspaceBuilder`]; unit tests supply [`EmptyGraphBuilder`],
//! [`FailingGraphBuilder`], or a custom impl.
//!
//! The trait is `Send + Sync` because the builder is held across a
//! rebuild-dispatcher task boundary. Every concrete implementation
//! must be cheap to clone — callers typically `Arc`-wrap the builder
//! and share it across the reaper + dispatcher + lifecycle tasks.

use std::{path::Path, sync::Arc};

use sqry_core::graph::CodeGraph;

use crate::error::DaemonError;

/// Build a [`CodeGraph`] for the given workspace root.
///
/// Trait object–friendly; [`get_or_load`] and friends accept a
/// `&dyn WorkspaceBuilder` so the caller can choose between the
/// production [`UnifiedGraphBuilder`] and a test-local in-memory
/// variant without the manager caring.
///
/// [`get_or_load`]: super::WorkspaceManager::get_or_load
pub trait WorkspaceBuilder: Send + Sync + std::fmt::Debug {
    /// Build the graph rooted at `workspace_root`. The implementation
    /// is responsible for any required lock-free / rayon parallelism
    /// and for honouring cancellation signals it consumes (e.g.
    /// pass-boundary checks in the rebuild pipeline).
    ///
    /// # Errors
    ///
    /// The daemon converts any returned [`DaemonError`] into a Failed
    /// workspace state + JSON-RPC `-32001 workspace_build_failed`.
    fn build(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError>;
}

// Allow calling builders through an `Arc` so they can be shared
// between tasks without explicit `.as_ref()` spam at every call site.
impl<T: WorkspaceBuilder + ?Sized> WorkspaceBuilder for Arc<T> {
    fn build(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        (**self).build(workspace_root)
    }
}

/// Test-only builder that always returns a freshly-built empty
/// [`CodeGraph`]. Useful for admission-accounting tests where the
/// actual graph content does not matter.
#[doc(hidden)]
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyGraphBuilder;

impl WorkspaceBuilder for EmptyGraphBuilder {
    fn build(&self, _workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        Ok(CodeGraph::new())
    }
}

/// Test builder that always returns a configured error. Used by the
/// Failed-state unit tests in Phase 6c; shipping the type here so
/// every phase after 6b uses the same builder abstraction.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct FailingGraphBuilder {
    /// Reason string surfaced via [`DaemonError::WorkspaceBuildFailed`].
    pub reason: String,
}

impl FailingGraphBuilder {
    /// Construct a failing builder with the given reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl WorkspaceBuilder for FailingGraphBuilder {
    fn build(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        Err(DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: self.reason.clone(),
        })
    }
}

/// Production [`WorkspaceBuilder`] that delegates to
/// [`sqry_core::graph::unified::build::build_unified_graph`].
///
/// Task 8 Phase 8a. Task 9's daemon bootstrap constructs exactly one
/// [`RealWorkspaceBuilder`] per daemon process, wrapping a shared
/// [`sqry_core::plugin::PluginManager`]. Tests inject
/// [`EmptyGraphBuilder`] or a custom builder.
pub struct RealWorkspaceBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    build_config: sqry_core::graph::unified::build::BuildConfig,
}

impl std::fmt::Debug for RealWorkspaceBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PluginManager` is intentionally not `Debug` (the 37 plugin
        // registrations are too noisy). Report pointer identity only.
        f.debug_struct("RealWorkspaceBuilder")
            .field(
                "plugins",
                &format_args!("<PluginManager@{:p}>", Arc::as_ptr(&self.plugins)),
            )
            .field("build_config", &self.build_config)
            .finish()
    }
}

impl RealWorkspaceBuilder {
    /// Construct a real builder with default
    /// [`sqry_core::graph::unified::build::BuildConfig`].
    #[must_use]
    pub fn new(plugins: Arc<sqry_core::plugin::PluginManager>) -> Self {
        Self {
            plugins,
            build_config: sqry_core::graph::unified::build::BuildConfig::default(),
        }
    }

    /// Construct a real builder with a caller-supplied
    /// [`sqry_core::graph::unified::build::BuildConfig`].
    #[must_use]
    pub fn with_build_config(
        plugins: Arc<sqry_core::plugin::PluginManager>,
        build_config: sqry_core::graph::unified::build::BuildConfig,
    ) -> Self {
        Self {
            plugins,
            build_config,
        }
    }
}

impl WorkspaceBuilder for RealWorkspaceBuilder {
    fn build(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        sqry_core::graph::unified::build::build_unified_graph(
            workspace_root,
            &self.plugins,
            &self.build_config,
        )
        .map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_returns_fresh_graph() {
        let b = EmptyGraphBuilder;
        let g = b.build(Path::new("/repos/example")).expect("always ok");
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn failing_builder_surfaces_reason_and_root() {
        let b = FailingGraphBuilder::new("plugin panic");
        let err = b
            .build(Path::new("/repos/example"))
            .expect_err("always fails");
        match err {
            DaemonError::WorkspaceBuildFailed { root, reason } => {
                assert_eq!(root, Path::new("/repos/example"));
                assert_eq!(reason, "plugin panic");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn arc_builder_passes_through_to_inner() {
        let inner: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
        let g = inner
            .build(Path::new("/repos/example"))
            .expect("arc-wrapped builder delegates");
        assert_eq!(g.node_count(), 0);
    }
}
