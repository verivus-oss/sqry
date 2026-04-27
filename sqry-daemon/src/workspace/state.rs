//! Workspace state machine and key types.
//!
//! Corresponds to Task 6 Step 1 of the sqryd plan, augmented by
//! STEP_6 of the workspace-aware-cross-repo DAG (2026-04-26):
//!
//! - [`WorkspaceState`] — the six-state workspace lifecycle enum (A2 §G.5,
//!   §G.7). **Moved to `sqry-daemon-protocol` in Phase 8c U1** so the
//!   wire-type [`crate::ipc::protocol::ResponseMeta`] can carry a canonical
//!   `workspace_state` field without the leaf protocol crate taking a dep
//!   on `sqry-daemon`. Re-exported here so existing call sites continue to
//!   compile. Stored on [`crate::workspace::LoadedWorkspace`] as an
//!   [`AtomicU8`]; the `#[repr(u8)]` discriminant makes the store / load
//!   round-trip lossless and gives cheap exhaustive match arms.
//! - [`WorkspaceKey`] — the identity used to dedup workspaces in
//!   [`crate::workspace::WorkspaceManager`].
//!
//!   STEP_6 of the workspace-aware-cross-repo plan **augments** (does not
//!   replace) the original three-dimensional key. The composite key is
//!   now four-dimensional: an optional `workspace_id` that groups
//!   logically-related source roots, plus the canonical absolute
//!   `source_root` (renamed from `index_root` with a serde alias for
//!   wire-compat), the [`ProjectRootMode`] (so the same repo opened with
//!   different modes gets distinct cache entries), and a config
//!   fingerprint (so a meaningful config change forces a fresh load).
//!
//!   Backward compatibility:
//!   - `workspace_id == None` reproduces today's per-source-root /
//!     anonymous semantics — exactly what the daemon did before STEP_6.
//!   - The serde wire form preserves the legacy `index_root` field name
//!     via `#[serde(alias = "index_root")]`, so persisted v1 JSON
//!     fixtures and over-the-wire payloads from older clients keep
//!     parsing without an explicit migration step. The new wire form
//!     emits `source_root`.
//! - [`OldGraphToken`] — opaque handle used by the admission map to key
//!   retained old graphs. Never serialised; values are process-local.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sqry_core::project::ProjectRootMode;
use sqry_daemon_protocol::WorkspaceId;

// ---------------------------------------------------------------------------
// WorkspaceState — re-exported from the leaf protocol crate.
// ---------------------------------------------------------------------------

pub use sqry_daemon_protocol::protocol::WorkspaceState;

// ---------------------------------------------------------------------------
// WorkspaceKey
// ---------------------------------------------------------------------------

/// Composite identity for a loaded workspace.
///
/// Two workspaces with the same [`Self::source_root`] but different
/// [`Self::root_mode`] or [`Self::config_fingerprint`] are distinct cache
/// entries — this prevents cache collisions when the same repo is opened
/// with different client configurations.
///
/// `source_root` is always the canonical absolute path (caller
/// responsibility; [`Self::new`] does not canonicalise because a path may
/// not exist on disk yet at the moment a key is synthesised).
///
/// # STEP_6 augmentation
///
/// `workspace_id` lifts the key into a four-dimensional space so a
/// single logical workspace can group multiple source roots under one
/// stable identity. With `workspace_id = None`, the key collapses to
/// today's three-dimensional behaviour: each source root is its own
/// anonymous / per-repo entry. With `workspace_id = Some(id)`, every
/// `WorkspaceKey` sharing the same `id` belongs to the same logical
/// workspace; the manager LRU still operates per-source-root so partial
/// eviction is observable, but `daemon/workspaceStatus` aggregates
/// across source roots that share the id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceKey {
    /// Optional identity of the logical workspace this key belongs to.
    ///
    /// `None` reproduces today's per-source-root / anonymous behaviour.
    /// `Some(id)` groups this entry with every other `WorkspaceKey`
    /// carrying the same `id` — `daemon/workspaceStatus { workspace_id }`
    /// returns the aggregate, but the LRU still operates per-source-root.
    ///
    /// `#[serde(default)]` so v1 JSON fixtures (which never carry the
    /// field) round-trip into `None`. Serialisation skips `None` so the
    /// new wire form is identical to v1 for the anonymous case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,

    /// Canonical absolute path to the source-root directory (the
    /// per-source-root index unit).
    ///
    /// `#[serde(alias = "index_root")]` keeps every v1 payload that
    /// emitted the legacy `index_root` field name parsing without
    /// migration; the new wire form emits `source_root`.
    #[serde(alias = "index_root")]
    pub source_root: PathBuf,

    /// How the project root was determined for this workspace.
    pub root_mode: ProjectRootMode,

    /// 64-bit fingerprint of the config values that materially affect
    /// the graph (plugin selection, cost tiering, macro expansion
    /// toggles, etc). Callers compute the fingerprint deterministically
    /// from the client-declared options; an unchanged fingerprint
    /// means the daemon is free to return the cached graph.
    pub config_fingerprint: u64,
}

impl WorkspaceKey {
    /// Construct a new anonymous (per-source-root) key. Caller is
    /// responsible for passing a canonical absolute path.
    ///
    /// Equivalent to today's three-dimensional `WorkspaceKey`. For
    /// logical-workspace-grouped keys, use [`Self::with_workspace_id`].
    #[must_use]
    pub fn new(source_root: PathBuf, root_mode: ProjectRootMode, config_fingerprint: u64) -> Self {
        Self {
            workspace_id: None,
            source_root,
            root_mode,
            config_fingerprint,
        }
    }

    /// Construct a logical-workspace-grouped key. Two keys sharing the
    /// same `workspace_id` are aggregated by `daemon/workspaceStatus`;
    /// they remain distinct cache entries (admission, LRU, eviction
    /// all operate per source root).
    #[must_use]
    pub fn with_workspace_id(
        workspace_id: WorkspaceId,
        source_root: PathBuf,
        root_mode: ProjectRootMode,
        config_fingerprint: u64,
    ) -> Self {
        Self {
            workspace_id: Some(workspace_id),
            source_root,
            root_mode,
            config_fingerprint,
        }
    }

    /// Backwards-compat accessor for the source-root path. Pre-STEP_6
    /// the field was named `index_root`; the rename to `source_root`
    /// preserves the new wire form while this accessor lets internal
    /// call sites that read the path continue to compile without a
    /// per-site rename.
    #[must_use]
    #[inline]
    pub fn index_root(&self) -> &Path {
        &self.source_root
    }
}

impl std::fmt::Display for WorkspaceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The pre-STEP_6 format was `<path>[<mode>@<fingerprint>]`;
        // we keep that suffix verbatim and prefix with the workspace-id
        // short hex when one is bound. Anonymous keys round-trip to the
        // exact pre-STEP_6 string.
        if let Some(id) = &self.workspace_id {
            write!(
                f,
                "{}@{}[{}@{:016x}]",
                self.source_root.display(),
                id,
                self.root_mode,
                self.config_fingerprint,
            )
        } else {
            write!(
                f,
                "{}[{}@{:016x}]",
                self.source_root.display(),
                self.root_mode,
                self.config_fingerprint,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// WorkspaceId bridge — sqry_core ↔ sqry_daemon_protocol.
// ---------------------------------------------------------------------------

/// Bridge from the canonical [`sqry_core::workspace::WorkspaceId`] to
/// the wire-form [`WorkspaceId`] (defined in `sqry-daemon-protocol` so
/// the leaf protocol crate stays free of a `sqry-core` dependency).
///
/// The two types are byte-identical (32-byte BLAKE3-256 digest) so the
/// bridge is a zero-cost copy of the underlying array.
#[must_use]
pub fn wire_workspace_id_from_core(core: &sqry_core::workspace::WorkspaceId) -> WorkspaceId {
    WorkspaceId::from_bytes(*core.as_bytes())
}

// ---------------------------------------------------------------------------
// OldGraphToken
// ---------------------------------------------------------------------------

/// Opaque token used by [`crate::workspace::admission::AdmissionState::retained_old`]
/// to key entries for retained old graphs.
///
/// The token is a monotonic per-process counter sourced from a single
/// [`AtomicU64`]. Uniqueness is guaranteed for the lifetime of the daemon
/// process (2^64 is effectively inexhaustible at daemon-scale rates).
/// Tokens are never persisted or serialised across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OldGraphToken(u64);

impl OldGraphToken {
    /// Mint a fresh token. Thread-safe — concurrent callers always
    /// receive distinct values.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Inspect the raw token value (useful for tracing).
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl Default for OldGraphToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for OldGraphToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OldGraphToken({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_via_discriminant() {
        for &s in &[
            WorkspaceState::Unloaded,
            WorkspaceState::Loading,
            WorkspaceState::Loaded,
            WorkspaceState::Rebuilding,
            WorkspaceState::Evicted,
            WorkspaceState::Failed,
        ] {
            assert_eq!(WorkspaceState::from_u8(s.as_u8()), Some(s), "{s}");
        }
    }

    #[test]
    fn state_from_out_of_range_is_none() {
        assert_eq!(WorkspaceState::from_u8(6), None);
        assert_eq!(WorkspaceState::from_u8(255), None);
    }

    #[test]
    fn state_is_serving_matches_a2_table() {
        assert!(!WorkspaceState::Unloaded.is_serving());
        assert!(!WorkspaceState::Loading.is_serving());
        assert!(WorkspaceState::Loaded.is_serving());
        assert!(WorkspaceState::Rebuilding.is_serving());
        assert!(!WorkspaceState::Evicted.is_serving());
        assert!(WorkspaceState::Failed.is_serving());
    }

    #[test]
    fn key_distinguishes_root_mode_and_fingerprint() {
        let a = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1234_5678_9abc_def0,
        );
        let b = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::WorkspaceFolder,
            0x1234_5678_9abc_def0,
        );
        let c = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0xdead_beef_dead_beef,
        );
        assert_ne!(a, b, "different root_mode must be different keys");
        assert_ne!(a, c, "different fingerprint must be different keys");
        assert_eq!(a, a.clone(), "same components compare equal");

        // STEP_6 augmentation: `workspace_id = None` reproduces today's
        // anonymous / per-source-root semantics. Two anonymous keys
        // with the same three classical dimensions collapse to the
        // same cache entry — exactly the pre-STEP_6 invariant.
        let d = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1234_5678_9abc_def0,
        );
        assert_eq!(a, d, "anonymous keys reproduce per-source-root semantics");
        assert!(
            a.workspace_id.is_none(),
            "WorkspaceKey::new must set workspace_id = None",
        );

        // STEP_6: same source-root + mode + fingerprint, different
        // workspace_id ⇒ distinct cache entries. The classical-key
        // semantics (acceptance criterion #7 in the STEP_6 brief) are
        // preserved in the `None` case AND extended cleanly to `Some`.
        let id_x = WorkspaceId::from_bytes([0x11; 32]);
        let id_y = WorkspaceId::from_bytes([0x22; 32]);
        let e = WorkspaceKey::with_workspace_id(
            id_x,
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1234_5678_9abc_def0,
        );
        let f = WorkspaceKey::with_workspace_id(
            id_y,
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1234_5678_9abc_def0,
        );
        let g = WorkspaceKey::with_workspace_id(
            id_x,
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0xdead_beef_dead_beef,
        );
        assert_ne!(a, e, "anonymous vs. logical key must differ");
        assert_ne!(e, f, "different workspace_id must be different keys");
        assert_ne!(
            e, g,
            "two LogicalWorkspaces sharing source-root path but differing \
             config_fingerprint produce distinct cache entries"
        );

        // STEP_11_4 acceptance: the `LogicalWorkspace` case — two
        // workspaces sharing the same workspace_id and source-root
        // path but produced by distinct PluginSelectionConfig /
        // HighCostMode / global indexing inputs (i.e. different
        // `compute_workspace_config_fingerprint(...)` outputs)
        // MUST land in distinct cache entries. This is the
        // contract `WorkspaceKey` enforces on top of
        // `LogicalWorkspace.config_fingerprint` /
        // `SourceRoot.config_fingerprint`.
        let fp_a = sqry_core::config::compute_workspace_config_fingerprint(
            b"plugins:rust,go",
            b"indexing:default",
        );
        let fp_b = sqry_core::config::compute_workspace_config_fingerprint(
            b"plugins:rust,go,python",
            b"indexing:default",
        );
        assert_ne!(
            fp_a, fp_b,
            "STEP_11_4: differing PluginSelectionConfig must produce \
             distinct fingerprints"
        );
        let h = WorkspaceKey::with_workspace_id(
            id_x,
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            fp_a,
        );
        let i = WorkspaceKey::with_workspace_id(
            id_x,
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            fp_b,
        );
        assert_ne!(
            h, i,
            "STEP_11_4: two LogicalWorkspace-grouped WorkspaceKeys sharing \
             workspace_id + source-root path but differing \
             compute_workspace_config_fingerprint(...) MUST be distinct \
             cache entries"
        );

        // And the per-source-root override / inheritance composes:
        // a SourceRoot that does not carry an explicit override
        // inherits the workspace-level fingerprint via
        // `effective_config_fingerprint` — the daemon-side key reads
        // the effective value, not the raw field, so the inheritance
        // surface lives in `sqry-core` and is wire-compatible with
        // today's `WorkspaceKey`.
        let mut root = sqry_core::workspace::SourceRoot::from_path(PathBuf::from("/repos/example"));
        assert_eq!(
            root.config_fingerprint, 0,
            "fresh SourceRoot has fingerprint 0"
        );
        assert_eq!(
            root.effective_config_fingerprint(fp_a),
            fp_a,
            "SourceRoot with fingerprint 0 inherits the workspace default",
        );
        root.config_fingerprint = 0x1234_5678_9abc_def0;
        assert_eq!(
            root.effective_config_fingerprint(fp_a),
            0x1234_5678_9abc_def0,
            "SourceRoot with explicit override returns the override, not the default",
        );
    }

    #[test]
    fn token_is_monotonic_and_unique() {
        let a = OldGraphToken::new();
        let b = OldGraphToken::new();
        let c = OldGraphToken::new();
        assert!(a.raw() < b.raw());
        assert!(b.raw() < c.raw());
        assert_ne!(a, b);
        assert_ne!(b, c);
    }
}
