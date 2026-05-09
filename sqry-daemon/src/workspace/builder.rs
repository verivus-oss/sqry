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

#[cfg(test)]
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

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

    /// Read-only, persisted-graph rehydrate.
    ///
    /// SGA04 (shared graph acquisition, daemon provider): reload an
    /// existing valid persisted graph from `<workspace_root>/.sqry/graph/`
    /// **without** running [`Self::build`] — no parse, no plugin
    /// pipeline, no durable publish. Used by
    /// [`super::WorkspaceManager::reload_from_disk_read_only`] to fulfil
    /// the bounded one-shot eviction-reload contract for read-only
    /// queries (see `docs/development/shared-graph-acquisition/02_DESIGN.md`,
    /// "Bounded reload rule").
    ///
    /// The default impl returns [`DaemonError::WorkspaceBuildFailed`]
    /// with a reason of `"persisted graph rehydrate not implemented"`.
    /// Test fakes that don't need the read-only reload path keep that
    /// behaviour; production code uses [`RealWorkspaceBuilder`] which
    /// drives `GraphStorage::load_from_path` against the workspace's
    /// snapshot file.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspaceBuildFailed`] when no persisted graph
    ///   exists, when integrity verification fails, when the snapshot
    ///   format is incompatible, or when this builder does not support
    ///   read-only rehydrate.
    fn load_persisted(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        Err(DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: "persisted graph rehydrate not implemented for this builder".to_string(),
        })
    }
}

// Allow calling builders through an `Arc` so they can be shared
// between tasks without explicit `.as_ref()` spam at every call site.
impl<T: WorkspaceBuilder + ?Sized> WorkspaceBuilder for Arc<T> {
    fn build(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        (**self).build(workspace_root)
    }

    fn load_persisted(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        (**self).load_persisted(workspace_root)
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

    /// SGA04 read-only persisted-graph rehydrate.
    ///
    /// Loads `<workspace_root>/.sqry/graph/snapshot.sqry` via
    /// [`sqry_core::graph::unified::persistence::load_from_path`] using
    /// the production [`PluginManager`]. Never invokes the build
    /// pipeline; never writes any artifact.
    ///
    /// [`PluginManager`]: sqry_core::plugin::PluginManager
    fn load_persisted(&self, workspace_root: &Path) -> Result<CodeGraph, DaemonError> {
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
        if !storage.exists() {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: workspace_root.to_path_buf(),
                reason: format!(
                    "no persisted graph artifact at {} (.sqry/graph/manifest.json absent)",
                    workspace_root.display()
                ),
            });
        }
        if !storage.snapshot_exists() {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: workspace_root.to_path_buf(),
                reason: format!(
                    "manifest present but snapshot missing at {}",
                    storage.snapshot_path().display()
                ),
            });
        }
        sqry_core::graph::unified::persistence::load_from_path(
            storage.snapshot_path(),
            Some(&self.plugins),
        )
        .map_err(|e| match e {
            // SGA04 Major #2 (codex iter2) — preserve the
            // incompatible-snapshot distinction through the daemon
            // builder. The dispatcher surfaces this as
            // JSON-RPC -32005 / `WorkspaceIncompatibleGraph` rather than
            // the generic -32001 `WorkspaceBuildFailed` (which would
            // suggest a transient build problem the user could retry).
            sqry_core::graph::unified::persistence::PersistenceError::IncompatibleVersion {
                expected,
                found,
            } => DaemonError::WorkspaceIncompatibleGraph {
                root: workspace_root.to_path_buf(),
                reason: format!("snapshot version mismatch: expected {expected}, found {found}"),
            },
            other => DaemonError::WorkspaceBuildFailed {
                root: workspace_root.to_path_buf(),
                reason: format!("snapshot load failed: {other}"),
            },
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

    /// SGA04 Major #2 (codex iter2) — when the persisted snapshot
    /// reports an incompatible format version, `load_persisted` must
    /// return [`DaemonError::WorkspaceIncompatibleGraph`] (which the
    /// dispatcher exposes as JSON-RPC -32005), **not** the generic
    /// transient-build [`DaemonError::WorkspaceBuildFailed`] (-32001).
    /// We hand-craft a snapshot file with the current V10 magic but a
    /// `GraphHeader.version` of `99` to force
    /// `PersistenceError::IncompatibleVersion`.
    #[test]
    fn real_workspace_builder_load_persisted_incompatible_snapshot_returns_incompatible_graph_error()
     {
        use sha2::{Digest, Sha256};
        use sqry_core::graph::unified::persistence::{
            BuildProvenance, GraphHeader, GraphStorage, MAGIC_BYTES_V10, MANIFEST_SCHEMA_VERSION,
            Manifest, PluginSelectionManifest, SNAPSHOT_FORMAT_VERSION,
        };
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().to_path_buf();
        let storage = GraphStorage::new(&workspace);
        fs::create_dir_all(storage.graph_dir()).expect("graph dir");

        // Build V10-magic + bogus-version-99 header bytes.
        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = 99;
        let header_bytes = postcard::to_allocvec(&header).expect("encode header");
        let mut bytes: Vec<u8> = Vec::with_capacity(14 + 4 + header_bytes.len() + 8);
        bytes.extend_from_slice(MAGIC_BYTES_V10);
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        bytes.extend_from_slice(&0u64.to_le_bytes());
        fs::write(storage.snapshot_path(), &bytes).expect("write snapshot");

        // Stub manifest pointing at the bogus snapshot SHA so the
        // GraphStorage `exists()` / `snapshot_exists()` precondition
        // checks pass and `load_persisted` actually reaches
        // `load_from_path`.
        let snapshot_sha256 = hex_lower(&Sha256::digest(&bytes));
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            built_at: "1970-01-01T00:00:00Z".to_string(),
            root_path: workspace.to_string_lossy().into_owned(),
            node_count: 0,
            edge_count: 0,
            raw_edge_count: None,
            snapshot_sha256,
            build_provenance: BuildProvenance {
                sqry_version: env!("CARGO_PKG_VERSION").to_string(),
                build_timestamp: "1970-01-01T00:00:00Z".to_string(),
                build_command: "test:incompatible-version".to_string(),
                plugin_hashes: std::collections::HashMap::new(),
            },
            file_count: std::collections::HashMap::new(),
            languages: Vec::new(),
            config: std::collections::HashMap::new(),
            confidence: Default::default(),
            last_indexed_commit: None,
            plugin_selection: Some(PluginSelectionManifest {
                active_plugin_ids: Vec::new(),
                high_cost_mode: None,
            }),
        };
        manifest
            .save(storage.manifest_path())
            .expect("save manifest");

        let plugins = Arc::new(sqry_core::plugin::PluginManager::new());
        let builder = RealWorkspaceBuilder::new(plugins);
        let err = builder
            .load_persisted(&workspace)
            .expect_err("incompatible-version snapshot must fail load_persisted");

        match err {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, workspace);
                assert!(
                    reason.contains("snapshot version mismatch") && reason.contains("found 99"),
                    "expected snapshot version mismatch diagnostic, got {reason:?}"
                );
            }
            other => panic!("expected DaemonError::WorkspaceIncompatibleGraph, got {other:?}"),
        }
    }
}
