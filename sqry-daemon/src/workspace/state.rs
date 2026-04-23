//! Workspace state machine and key types.
//!
//! Corresponds to Task 6 Step 1 of the sqryd plan:
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
//!   [`crate::workspace::WorkspaceManager`]. Composed of the absolute
//!   `index_root`, the [`ProjectRootMode`] (so the same repo opened with
//!   different modes gets distinct cache entries), and a config
//!   fingerprint (so a meaningful config change forces a fresh load).
//! - [`OldGraphToken`] — opaque handle used by the admission map to key
//!   retained old graphs. Never serialised; values are process-local.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sqry_core::project::ProjectRootMode;

// ---------------------------------------------------------------------------
// WorkspaceState — re-exported from the leaf protocol crate.
// ---------------------------------------------------------------------------

pub use sqry_daemon_protocol::protocol::WorkspaceState;

// ---------------------------------------------------------------------------
// WorkspaceKey
// ---------------------------------------------------------------------------

/// Composite identity for a loaded workspace.
///
/// Two workspaces with the same [`Self::index_root`] but different
/// [`Self::root_mode`] or [`Self::config_fingerprint`] are distinct cache
/// entries — this prevents cache collisions when the same repo is opened
/// with different client configurations.
///
/// `index_root` is always the canonical absolute path (caller
/// responsibility; [`Self::new`] does not canonicalise because a path may
/// not exist on disk yet at the moment a key is synthesised).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceKey {
    /// Canonical absolute path to the workspace root directory.
    pub index_root: PathBuf,

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
    /// Construct a new key. Caller is responsible for passing a canonical
    /// absolute path.
    #[must_use]
    pub fn new(index_root: PathBuf, root_mode: ProjectRootMode, config_fingerprint: u64) -> Self {
        Self {
            index_root,
            root_mode,
            config_fingerprint,
        }
    }
}

impl std::fmt::Display for WorkspaceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}[{}@{:016x}]",
            self.index_root.display(),
            self.root_mode,
            self.config_fingerprint,
        )
    }
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
