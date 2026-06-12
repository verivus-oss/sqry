//! Thin wrapper around the `sd_notify` crate.
//!
//! # Why a wrapper module?
//!
//! `sd_notify` is a **Linux-only** dependency (systemd is Linux-specific).
//! Rather than scattering `#[cfg(target_os = "linux")]` guards across every
//! call site in the startup path, this module provides a single cross-platform
//! API:
//!
//! - On **Linux**: delegates to the real [`sd_notify`](https://docs.rs/sd-notify)
//!   crate.  No-op when `NOTIFY_SOCKET` is absent (the crate already handles
//!   that silently).
//! - On **macOS** and **Windows**: every function is a no-op returning `Ok(())`.
//!   launchd and the Windows SCM have no `sd_notify` equivalent; the daemon
//!   signals readiness through other mechanisms (socket-connect for auto-spawn,
//!   launchd `KeepAlive` heartbeat for launchd supervision).
//!
//! # Authoritative ready-signal matrix (§C.3.1 step 15)
//!
//! | Context | Authoritative signal | This module's role |
//! |---|---|---|
//! | systemd `Type=notify` | `READY=1` via `sd_notify` | sends it |
//! | `--spawned-by-client` (auto-spawn) | self-pipe close | no-op |
//! | Direct foreground | none required | no-op |
//!
//! `sqryd.ready` sentinel file is always touched as a diagnostic aid, but it
//! is **not authoritative** for any code path.
//!
//! # Thread safety
//!
//! All functions are `Send + Sync`-safe.  The underlying `sd_notify::notify`
//! call is a one-shot `sendmsg` to the `NOTIFY_SOCKET` datagram socket and
//! carries no mutable global state.

/// Send `READY=1` to systemd (Linux only; no-op elsewhere).
///
/// Call this after `IpcServer::bind` succeeds and the daemon is fully
/// prepared to accept connections.  Under `Type=notify` this is the
/// authoritative signal that unblocks `systemctl start` (and `is-active`
/// checks).  When not running under systemd the function returns `Ok(())`
/// without any observable side-effect.
///
/// # Errors
///
/// On Linux, returns `Err` if `NOTIFY_SOCKET` is set but the `sendmsg`
/// syscall fails (e.g. the socket path is stale or the socket type is wrong).
/// The caller should log this as a warning but **not** abort startup — a
/// failed `READY=1` is better surfaced as a `systemctl status` warning than
/// a daemon that never starts.
///
/// On non-Linux platforms this function always returns `Ok(())`.
pub fn notify_ready() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // `sd_notify::notify(&[sd_notify::NotifyState::Ready])` sends
        // "READY=1\n" to $NOTIFY_SOCKET while leaving the environment visible
        // to any child processes that might also need to notify.
        sd_notify::notify(&[sd_notify::NotifyState::Ready])?;
    }
    // macOS / Windows: launchd and SCM manage readiness through their own
    // protocols; nothing to do here.
    Ok(())
}

/// Send `STATUS=<message>` to systemd (Linux only; no-op elsewhere).
///
/// Updates the daemon's human-readable status string visible in
/// `systemctl status sqryd`.  Useful for progress messages during slow
/// workspace graph loads.
///
/// # Errors
///
/// Same contract as [`notify_ready`]: Linux returns `Err` on `sendmsg`
/// failure; other platforms always return `Ok(())`.
pub fn notify_status(message: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        sd_notify::notify(&[sd_notify::NotifyState::Status(message)])?;
    }
    // Suppress "unused variable" on non-Linux.
    #[cfg(not(target_os = "linux"))]
    let _ = message;
    Ok(())
}

/// Send `STOPPING=1` to systemd (Linux only; no-op elsewhere).
///
/// Informs the service manager that the daemon has begun its shutdown
/// sequence.  This gives systemd an early signal before the process exits
/// so that `systemctl stop` does not wait for `TimeoutStopSec` to elapse.
///
/// # Errors
///
/// Same contract as [`notify_ready`].
pub fn notify_stopping() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        sd_notify::notify(&[sd_notify::NotifyState::Stopping])?;
    }
    Ok(())
}

/// Returns `true` if the daemon is being supervised by systemd on this
/// platform (i.e. `NOTIFY_SOCKET` is set in the environment).
///
/// On non-Linux platforms this always returns `false`.
///
/// # Note
///
/// This is a **read-only** check; it does not consume or clear
/// `NOTIFY_SOCKET`.  Use this to gate the `RollingSizeAppender` skip-path
/// described in the design (`m4` fix — §G.1 `install_tracing`).
#[must_use]
pub fn is_under_systemd() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("NOTIFY_SOCKET").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::test_support::NotifySocketGuard;

    /// On non-Linux platforms every notify function must be a no-op that
    /// returns `Ok(())`.  On Linux without `NOTIFY_SOCKET` set the real
    /// `sd_notify` crate also silently returns `Ok(())`; we verify the
    /// same contract holds in the wrapper.
    #[test]
    fn notify_ready_is_ok_without_notify_socket() {
        // Temporarily unset NOTIFY_SOCKET so the Linux path also no-ops.
        let _guard = NotifySocketGuard::unset();
        assert!(
            notify_ready().is_ok(),
            "notify_ready must not error when NOTIFY_SOCKET is absent"
        );
    }

    #[test]
    fn notify_status_is_ok_without_notify_socket() {
        let _guard = NotifySocketGuard::unset();
        assert!(
            notify_status("test status message").is_ok(),
            "notify_status must not error when NOTIFY_SOCKET is absent"
        );
    }

    #[test]
    fn notify_stopping_is_ok_without_notify_socket() {
        let _guard = NotifySocketGuard::unset();
        assert!(
            notify_stopping().is_ok(),
            "notify_stopping must not error when NOTIFY_SOCKET is absent"
        );
    }

    #[test]
    fn is_under_systemd_returns_false_without_notify_socket() {
        let _guard = NotifySocketGuard::unset();
        assert!(
            !is_under_systemd(),
            "is_under_systemd must return false when NOTIFY_SOCKET is absent"
        );
    }

    /// On non-Linux platforms `is_under_systemd` must always be `false`
    /// regardless of what environment variables are set.
    #[test]
    #[cfg(not(target_os = "linux"))]
    fn is_under_systemd_is_always_false_on_non_linux() {
        // We explicitly set NOTIFY_SOCKET to prove the guard is stronger
        // than env-var presence: the platform dispatch (`#[cfg(not(target_os
        // = "linux"))]`) must win, not the env-var check.
        let _guard = NotifySocketGuard::set("/run/systemd/notify");
        assert!(
            !is_under_systemd(),
            "is_under_systemd must always be false on non-Linux platforms \
             even when NOTIFY_SOCKET is set"
        );
    }

    /// On Linux, when `NOTIFY_SOCKET` is set to a valid-looking path,
    /// `is_under_systemd` must return `true`.  We use a dummy path string;
    /// the test only exercises the env-var detection, not the actual socket.
    #[test]
    #[cfg(target_os = "linux")]
    fn is_under_systemd_returns_true_when_notify_socket_set() {
        // SAFETY: test-only env mutation; wrapped in a guard that restores
        // the previous value.  Test suite must NOT run with `cargo test
        // --test-threads=1` suppressed; see guard implementation below.
        let _guard = NotifySocketGuard::set("/run/systemd/notify");
        assert!(
            is_under_systemd(),
            "is_under_systemd must return true when NOTIFY_SOCKET is set"
        );
    }

    /// On Linux, `notify_ready()` with a syntactically valid but
    /// non-existent `NOTIFY_SOCKET` path must return `Err` (the `sendmsg`
    /// will fail with ENOENT / ECONNREFUSED).  This verifies that the Linux
    /// code path is actually invoked rather than silently no-oping.
    #[test]
    #[cfg(target_os = "linux")]
    fn notify_ready_errors_on_stale_notify_socket() {
        // Use a tempdir-derived path so the test is hermetic — no other
        // process can accidentally create this path and turn the test into
        // a false negative.  We create the tempdir (guaranteeing a unique
        // directory exists) but deliberately do NOT create `notify.sock`
        // inside it, so the path is guaranteed not to exist.
        let dir = tempfile::tempdir().expect("tempdir allocation failed");
        let nonexistent = dir.path().join("notify.sock");
        debug_assert!(!nonexistent.exists(), "notify.sock must not pre-exist");
        let socket_path = nonexistent
            .to_str()
            .expect("tempdir path must be valid UTF-8");
        let _guard = NotifySocketGuard::set(socket_path);
        let result = notify_ready();
        assert!(
            result.is_err(),
            "notify_ready must return Err when NOTIFY_SOCKET points to a non-existent socket"
        );
    }
}
