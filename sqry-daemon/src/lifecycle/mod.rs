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

#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;

    /// Sets or unsets `NOTIFY_SOCKET` for one test and restores it on drop.
    ///
    /// Holds the crate-wide `TEST_ENV_LOCK` (surface parity W4 round 2,
    /// W4-D14) rather than a mutex of its own: a module-local lock serialises
    /// nothing against the other tests that write the environment under the
    /// crate lock. The guard field drops after `Drop::drop` restores the
    /// variable, so the restore also happens under the lock. The lock gate
    /// (`tests/env_lock_discipline.rs`, decision D-i7-envlock-1) reads this as a
    /// holder, whose guard covers its `drop` only: a test whose own reads the
    /// gate follows takes the crate lock itself instead of keeping one.
    pub(crate) struct NotifySocketGuard {
        previous: Option<OsString>,
        _lock: crate::TestEnvGuard,
    }

    impl NotifySocketGuard {
        pub(crate) fn unset() -> Self {
            let _lock = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var_os("NOTIFY_SOCKET");
            unsafe {
                std::env::remove_var("NOTIFY_SOCKET");
            }
            Self { previous, _lock }
        }

        pub(crate) fn set(value: &str) -> Self {
            let _lock = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var_os("NOTIFY_SOCKET");
            unsafe {
                std::env::set_var("NOTIFY_SOCKET", value);
            }
            Self { previous, _lock }
        }
    }

    impl Drop for NotifySocketGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var("NOTIFY_SOCKET", value) },
                None => unsafe { std::env::remove_var("NOTIFY_SOCKET") },
            }
        }
    }
}
