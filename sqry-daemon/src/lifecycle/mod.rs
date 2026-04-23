//! Task 9 lifecycle primitives.
//!
//! This module is the landing zone for all sqryd binary lifecycle concerns:
//!
//! - [`notify`] — thin `sd_notify` wrapper with platform-appropriate fallbacks.
//!   On Linux the real `sd_notify` crate is called; on macOS and Windows the
//!   functions are no-ops so that callers in the shared startup path never need
//!   `#[cfg(target_os = "linux")]` guards at the call site.
//! - [`log_rotate`] — `RollingSizeAppender` + `install_tracing` (Task 9 U5).
//!   Rotates the active log file when it exceeds `log_max_size_mb`, keeping at
//!   most `log_keep_rotations` copies.  When `NOTIFY_SOCKET` is present
//!   (systemd supervision) the rolling appender is skipped and output goes to
//!   stderr instead (§G.1 m4 fix).
//!
//! Modules added by later Task 9 units (U3–U10) will be declared here as they
//! are implemented.  This avoids merge-conflict churn: each unit adds one
//! `pub mod` line.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §C.3.1
//! (step 15 — authoritative ready-signal matrix) + §F.1 (systemd user unit
//! `Type=notify`) + §G (log rotation).

pub mod detach;
pub mod log_rotate;
pub mod notify;
pub mod pidfile;
pub mod signals;
pub mod units;
