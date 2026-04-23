//! `daemon/status` payload types.
//!
//! Phase 6b surfaces these from [`super::WorkspaceManager::status`].
//! Task 8 serialises them over the IPC envelope; Task 10 (`sqry daemon
//! status`) renders them into the human-readable and `--json`
//! formats from Amendment 2 §D.
//!
//! Every byte count is a `u64` so the JSON-RPC payload is
//! platform-independent on 32-bit hosts.

use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use super::state::WorkspaceState;

/// Top-level snapshot returned by `daemon/status`.
///
/// Construction is a single pass over the workspaces map plus a
/// handful of atomic reads; the snapshot is *not* transactional w.r.t.
/// publishes landing concurrently, but every field is individually
/// consistent. Callers treat it as a best-effort point-in-time view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Seconds since the `WorkspaceManager` was constructed.
    pub uptime_seconds: u64,

    /// `env!("CARGO_PKG_VERSION")` at build time — whatever the
    /// running daemon binary was compiled with.
    pub daemon_version: String,

    /// Aggregate memory accounting.
    pub memory: MemoryStatus,

    /// One entry per loaded or otherwise-tracked workspace, sorted
    /// by `index_root` for deterministic CLI output.
    pub workspaces: Vec<WorkspaceStatus>,
}

/// Aggregate memory accounting readout. Mirrors Amendment 2 §D.
///
/// Task 7 Phase 7c: marked `#[non_exhaustive]` so future field
/// additions (e.g., `retained_bytes` for visibility into the
/// retention reaper pipeline) do not force downstream struct-literal
/// callers through a source-breaking change. External callers must
/// construct instances through [`DaemonStatus`] observations (via
/// `WorkspaceManager::status`) rather than literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryStatus {
    /// Hard cap (`memory_limit_mb * 1 MiB`).
    pub limit_bytes: u64,

    /// Currently attributed memory:
    /// `loaded_bytes + reserved_bytes + sum(retained_old bytes)`.
    /// This is the left-hand side of the §G.5 invariant.
    pub current_bytes: u64,

    /// Reservation-only subset of `current_bytes` — the sum of every
    /// in-flight [`crate::workspace::RebuildReservation`]. Task 7
    /// Phase 7c: exposed so integration tests can assert refund on
    /// the cancellation path. At rest (no rebuild in flight) this is
    /// always `0`.
    pub reserved_bytes: u64,

    /// Monotonic peak of `current_bytes` over the daemon's uptime.
    /// Updated on every publish via `fetch_max`.
    pub high_water_bytes: u64,
}

/// Per-workspace status row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    /// Canonical absolute path (from [`super::WorkspaceKey::index_root`]).
    pub index_root: PathBuf,

    /// Current lifecycle state.
    pub state: WorkspaceState,

    /// Whether this workspace is LRU-exempt.
    pub pinned: bool,

    /// Live graph size (from [`super::LoadedWorkspace::memory_bytes`]).
    pub current_bytes: u64,

    /// Monotonic peak over this workspace's loaded lifetime. Resets
    /// only on unload/eviction (fresh `LoadedWorkspace`), never on
    /// rebuilds — per Amendment 2 §D.
    pub high_water_bytes: u64,

    /// Wall-clock of the most recent successful build. `None` if the
    /// workspace has never built successfully.
    pub last_good_at: Option<SystemTime>,

    /// Display string of the most recent error, if any. `None` in
    /// the steady-state Loaded case.
    pub last_error: Option<String>,

    /// Count of consecutive failed rebuilds since the last successful
    /// publish. Resets to 0 on `record_success`.
    pub retry_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_status_round_trips_through_json() {
        let status = DaemonStatus {
            uptime_seconds: 120,
            daemon_version: "8.0.6".into(),
            memory: MemoryStatus {
                limit_bytes: 2 * 1024 * 1024 * 1024,
                current_bytes: 450 * 1024 * 1024,
                reserved_bytes: 0,
                high_water_bytes: 1_200 * 1024 * 1024,
            },
            workspaces: vec![WorkspaceStatus {
                index_root: PathBuf::from("/repos/example"),
                state: WorkspaceState::Loaded,
                pinned: true,
                current_bytes: 320 * 1024 * 1024,
                high_water_bytes: 890 * 1024 * 1024,
                last_good_at: None,
                last_error: None,
                retry_count: 0,
            }],
        };
        let json = serde_json::to_string(&status).expect("serialise");
        let back: DaemonStatus = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back, status);
    }
}
