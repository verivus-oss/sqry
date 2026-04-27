//! Workspace management — state machine + admission accounting.
//!
//! Phase 6a of the sqryd plan (Task 6 Steps 1 / 2 / 3 / 4 / 4a / 4b /
//! 4c / 4d) lands the admission-accounting half of the manager:
//!
//! - [`state`] — `WorkspaceState`, `WorkspaceKey`, `OldGraphToken`.
//! - [`admission`] — `AdmissionState`, `RetainedEntry`, working-set
//!   estimate helpers (Amendment 2 §G.5 / §G.6).
//! - [`loaded`] — `LoadedWorkspace` struct (Amendment 2 §D / §G.7
//!   high-water mark, staleness anchor, pinned flag, `rebuild_lane`
//!   slot for Task 7).
//! - [`manager`] — `WorkspaceManager` with `reserve_rebuild`,
//!   `publish_and_retain`, `RollbackGuard`, retention reaper
//!   (Amendment 2 §G.1 / §G.2 / §G.3).
//!
//! Phase 6b (next increment) lands workspace lifecycle:
//! `get_or_load`, `evict_lru`, `unload`, `status`, Failed-state
//! handling + staleness cap + `-32002` expiry, and the SQRYD_HOOK
//! `save_derived` wire-in.

pub mod admission;
pub mod builder;
pub mod hook;
pub mod loaded;
pub mod manager;
pub mod persisted_state;
pub mod staleness;
pub mod state;
pub mod status;

pub use admission::{AdmissionState, RetainedEntry, WorkingSetInputs, working_set_estimate};
pub use builder::{EmptyGraphBuilder, FailingGraphBuilder, RealWorkspaceBuilder, WorkspaceBuilder};
pub use hook::{NoOpHook, RecordingHook, SharedHook, SqrydHook, noop_hook, spawn_hook};
pub use loaded::{LoadedWorkspace, PendingRebuild};
pub(crate) use manager::clone_err;
pub use manager::{RebuildReservation, ServeVerdict, WorkspaceManager};
pub use persisted_state::{
    PersistedState, PersistedStateError, load_persisted_state, parse_persisted_state,
};
pub use staleness::{BACKOFF_SCHEDULE, StalenessVerdict, backoff_delay_for, classify_staleness};
pub use state::{OldGraphToken, WorkspaceKey, WorkspaceState, wire_workspace_id_from_core};
pub use status::{DaemonStatus, MemoryStatus, WorkspaceStatus};
