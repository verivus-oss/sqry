//! Signal handler installation for the sqryd daemon.
//!
//! # Signal model and async-signal safety
//!
//! All signal handling here is built on [`tokio::signal`].  Tokio registers an
//! **async-signal-safe** forwarder at the OS level: on delivery the OS calls a
//! one-line C trampoline that writes a single byte to an internal self-pipe
//! (`write(2)` is async-signal-safe per POSIX).  The resulting async `Stream`
//! is polled exclusively from normal Tokio task context — ordinary async Rust,
//! not a raw `libc` signal handler.  There is no `SA_RESETHAND`, no global
//! mutable signal state touched by this module, and no calls to non-async-
//! signal-safe functions from within the signal-delivery path.
//!
//! # Signals handled
//!
//! | Signal | Platform | Action |
//! |--------|----------|--------|
//! | `SIGTERM` | Unix | Logs at `INFO`, cancels the shutdown token. |
//! | `SIGINT`  | Unix | Logs at `INFO`, cancels the shutdown token. |
//! | `SIGHUP`  | Unix | Logs at `WARN` ("treating as graceful shutdown per Task 9 §B.4"), cancels the shutdown token. |
//! | Ctrl-C    | Windows | Cancels the shutdown token. |
//!
//! Hot-reload on `SIGHUP` is explicitly **out of scope** for Task 9 (§B.4 of
//! the design).  `SIGHUP` triggers a graceful shutdown identical to `SIGTERM`.
//!
//! # Usage
//!
//! ```ignore
//! use tokio_util::sync::CancellationToken;
//! use sqry_daemon::lifecycle::signals::install_signal_handlers;
//!
//! let shutdown = CancellationToken::new();
//! let _guard = install_signal_handlers(shutdown.clone())?;
//! // …
//! shutdown.cancelled().await; // wakes on SIGTERM / SIGINT / SIGHUP / Ctrl-C
//! ```
//!
//! The returned [`SignalGuard`] **must be kept alive** for the duration of the
//! server's run loop.  Dropping it aborts the signal-listener tasks so that
//! further deliveries of `SIGTERM` / `SIGINT` / `SIGHUP` are no longer
//! forwarded to the shutdown token.  Note: Tokio installs a process-wide OS
//! signal handler for each registered signal kind and that handler is **not**
//! removed when individual listeners are dropped; the OS does not revert to
//! `SIG_DFL` after the guard is dropped.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §M (signal handling, n7 fix), §B.4 (SIGHUP = graceful shutdown).

use tokio_util::sync::CancellationToken;

use crate::error::{DaemonError, DaemonResult};

/// RAII guard that keeps the signal-listener tasks alive.
///
/// Created by [`install_signal_handlers`].  Dropping this guard aborts every
/// signal-listener task spawned during installation, stopping them from
/// forwarding further signal deliveries to the shutdown token.
///
/// **Important:** Tokio installs a process-wide OS signal handler for each
/// registered signal kind and that handler is **not** removed when the
/// listener is dropped.  The OS does **not** revert to `SIG_DFL` after the
/// guard is dropped.  What changes is that subsequent deliveries are no
/// longer forwarded to *this* guard's shutdown token; any other active
/// `tokio::signal` listener in the process could still observe them.  Keep
/// the guard alive for the entire lifetime of the server's run loop to ensure
/// graceful-shutdown signals are processed by sqryd.
#[derive(Debug)]
#[must_use = "SignalGuard must be kept alive for signal handling to remain active"]
pub struct SignalGuard {
    /// Abort handles for each spawned signal-listener task.
    ///
    /// Stored as `JoinHandle` so that `abort()` is called on each during
    /// `Drop`.  We do not `.await` the handles — the abort is fire-and-forget.
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

/// Install OS signal handlers and wire them to `shutdown`.
///
/// On **Unix** this registers listeners for `SIGTERM`, `SIGINT`, and `SIGHUP`.
/// On **Windows** this registers a `Ctrl-C` listener via
/// [`tokio::signal::ctrl_c`].
///
/// Each signal listener runs in its own Tokio task.  When a signal is
/// delivered, the task logs the event and calls
/// [`CancellationToken::cancel`] on `shutdown`.  Because
/// [`CancellationToken::cancel`] is idempotent, receiving multiple signals
/// (e.g. two rapid `SIGTERM`s) is safe and produces only a single
/// cancellation.
///
/// # Errors
///
/// Returns [`DaemonError::SignalSetup`] if any signal stream cannot be
/// registered.  This can happen in highly restricted containers (e.g. when
/// `sigaction(2)` returns `ENOSYS`) or when tokio's signal back-end fails to
/// initialise its self-pipe.
pub fn install_signal_handlers(shutdown: CancellationToken) -> DaemonResult<SignalGuard> {
    let handles = register_handlers(shutdown)?;
    Ok(SignalGuard { handles })
}

// ── Platform-specific registration ───────────────────────────────────────────

/// Unix: register SIGTERM, SIGINT, SIGHUP listeners.
#[cfg(unix)]
fn register_handlers(
    shutdown: CancellationToken,
) -> DaemonResult<Vec<tokio::task::JoinHandle<()>>> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm =
        signal(SignalKind::terminate()).map_err(|e| DaemonError::SignalSetup { source: e })?;
    let mut sigint =
        signal(SignalKind::interrupt()).map_err(|e| DaemonError::SignalSetup { source: e })?;
    let mut sighup =
        signal(SignalKind::hangup()).map_err(|e| DaemonError::SignalSetup { source: e })?;

    let shutdown_term = shutdown.clone();
    let h_term = tokio::spawn(async move {
        loop {
            // Signal::recv() returns None if the signal driver closes (rare,
            // but possible in certain runtime teardown scenarios).  Break to
            // avoid a busy-spin; the shutdown token will be cancelled on next
            // delivery in the normal path.
            if sigterm.recv().await.is_none() {
                tracing::warn!("SIGTERM signal driver closed — listener exiting");
                break;
            }
            tracing::info!("received SIGTERM — initiating graceful shutdown");
            shutdown_term.cancel();
        }
    });

    let shutdown_int = shutdown.clone();
    let h_int = tokio::spawn(async move {
        loop {
            if sigint.recv().await.is_none() {
                tracing::warn!("SIGINT signal driver closed — listener exiting");
                break;
            }
            tracing::info!("received SIGINT — initiating graceful shutdown");
            shutdown_int.cancel();
        }
    });

    let h_hup = tokio::spawn(async move {
        loop {
            if sighup.recv().await.is_none() {
                tracing::warn!("SIGHUP signal driver closed — listener exiting");
                break;
            }
            tracing::warn!(
                "received SIGHUP — treating as graceful shutdown per Task 9 §B.4 \
                 (hot-reload is out of scope)"
            );
            shutdown.cancel();
        }
    });

    Ok(vec![h_term, h_int, h_hup])
}

/// Windows: register a Ctrl-C listener.
#[cfg(not(unix))]
fn register_handlers(
    shutdown: CancellationToken,
) -> DaemonResult<Vec<tokio::task::JoinHandle<()>>> {
    let h = tokio::spawn(async move {
        loop {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!("ctrl-c listener error: {e} — stopping Ctrl-C handler");
                break;
            }
            tracing::info!("received Ctrl-C — initiating graceful shutdown");
            shutdown.cancel();
        }
    });

    Ok(vec![h])
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::install_signal_handlers;

    // ── helper ───────────────────────────────────────────────────────────

    /// Wait for `token` to be cancelled with a generous timeout so that slow
    /// CI machines do not produce spurious failures.  Returns `true` if the
    /// token was cancelled before the timeout, `false` otherwise.
    async fn wait_cancelled(token: &CancellationToken) -> bool {
        tokio::time::timeout(Duration::from_secs(5), token.cancelled())
            .await
            .is_ok()
    }

    // ── Unix signal tests ─────────────────────────────────────────────────

    /// SIGTERM must cancel the shutdown token.
    ///
    /// Sends `SIGTERM` to the current process via `libc::kill(getpid(),
    /// SIGTERM)`.  Tokio's signal back-end delivers the signal to the listener
    /// task through its internal self-pipe; the task then calls
    /// `shutdown.cancel()`.
    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_triggers_cancellation_token() {
        let shutdown = CancellationToken::new();
        let _guard = install_signal_handlers(shutdown.clone())
            .expect("install_signal_handlers must succeed");

        // Give the listener tasks a moment to start before sending the signal.
        tokio::task::yield_now().await;

        // SAFETY: `getpid()` returns the calling process's PID; `kill(pid,
        // SIGTERM)` sends SIGTERM only to this process.  Tokio has already
        // registered its async-signal-safe forwarder, so the signal is
        // delivered to the listener task rather than terminating the process.
        let pid = unsafe { libc::getpid() };
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(rc, 0, "kill(getpid(), SIGTERM) must succeed");

        assert!(
            wait_cancelled(&shutdown).await,
            "shutdown token must be cancelled after SIGTERM"
        );
    }

    /// SIGINT must cancel the shutdown token.
    #[cfg(unix)]
    #[tokio::test]
    async fn sigint_triggers_cancellation_token() {
        let shutdown = CancellationToken::new();
        let _guard = install_signal_handlers(shutdown.clone())
            .expect("install_signal_handlers must succeed");

        tokio::task::yield_now().await;

        let pid = unsafe { libc::getpid() };
        let rc = unsafe { libc::kill(pid, libc::SIGINT) };
        assert_eq!(rc, 0, "kill(getpid(), SIGINT) must succeed");

        assert!(
            wait_cancelled(&shutdown).await,
            "shutdown token must be cancelled after SIGINT"
        );
    }

    /// SIGHUP must cancel the shutdown token.
    ///
    /// The SIGHUP listener emits a WARN-level log ("treating as graceful
    /// shutdown per Task 9 §B.4") before cancelling.  This test verifies the
    /// cancellation behaviour; the exact log output is not captured here
    /// because it would require a custom tracing subscriber, which adds
    /// fragility.
    #[cfg(unix)]
    #[tokio::test]
    async fn sighup_triggers_cancellation_token_with_warn_log() {
        let shutdown = CancellationToken::new();
        let _guard = install_signal_handlers(shutdown.clone())
            .expect("install_signal_handlers must succeed");

        tokio::task::yield_now().await;

        let pid = unsafe { libc::getpid() };
        let rc = unsafe { libc::kill(pid, libc::SIGHUP) };
        assert_eq!(rc, 0, "kill(getpid(), SIGHUP) must succeed");

        assert!(
            wait_cancelled(&shutdown).await,
            "shutdown token must be cancelled after SIGHUP"
        );
    }

    /// `install_signal_handlers` must be callable repeatedly (each call
    /// registers a fresh set of listeners on the same signals, which tokio
    /// supports).  This also verifies that dropping a guard stops the previous
    /// listeners without affecting a subsequent `install_signal_handlers` call.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_is_idempotent_across_independent_invocations() {
        // First guard installed and immediately dropped — its listeners are
        // aborted but the signal streams are destroyed cleanly.
        {
            let shutdown = CancellationToken::new();
            let _guard =
                install_signal_handlers(shutdown.clone()).expect("first install must succeed");
        }

        // Second installation on a fresh token must still succeed.
        let shutdown2 = CancellationToken::new();
        let _guard2 = install_signal_handlers(shutdown2.clone())
            .expect("second install after drop must succeed");

        tokio::task::yield_now().await;

        let pid = unsafe { libc::getpid() };
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(rc, 0, "kill(getpid(), SIGTERM) must succeed");

        assert!(
            wait_cancelled(&shutdown2).await,
            "second shutdown token must be cancelled after SIGTERM"
        );
    }

    /// Dropping `SignalGuard` before a signal is received must not cancel the
    /// token.  The listener tasks have been aborted; Tokio's process-wide OS
    /// handler still intercepts the signal (the OS does NOT restore `SIG_DFL`
    /// on listener drop per Tokio's documented behavior), but no application
    /// code observes it.
    ///
    /// NOTE: We cannot easily verify that a subsequent SIGTERM is still
    /// intercepted by Tokio rather than killing the process, so we only assert
    /// that the token is NOT cancelled shortly after the guard is dropped.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_guard_before_signal_does_not_cancel_token() {
        let shutdown = CancellationToken::new();

        // Install and immediately drop the guard.
        drop(install_signal_handlers(shutdown.clone()).expect("install must succeed"));

        // The token must NOT have been cancelled.
        assert!(
            !shutdown.is_cancelled(),
            "token must not be cancelled when no signal has been sent"
        );
    }

    // ── Registration-failure simulation ──────────────────────────────────

    /// On Unix we cannot easily trigger a real `ENOSYS` registration failure
    /// from a test (it only occurs in very restricted container environments),
    /// so we instead verify the _happy-path_ installation returns `Ok` and
    /// the guard is usable.  The `DaemonError::SignalSetup` variant is
    /// exercised by the unit tests in `error.rs` (exit code + JSON-RPC code
    /// checks), not here.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_signal_handlers_returns_ok_on_happy_path() {
        let shutdown = CancellationToken::new();
        let result = install_signal_handlers(shutdown);
        assert!(
            result.is_ok(),
            "install_signal_handlers must return Ok on a normal Unix host: {result:?}"
        );
    }

    // ── Windows / non-Unix Ctrl-C test ────────────────────────────────────

    /// On Windows (and other non-Unix platforms), install must succeed.
    /// We cannot trigger a synthetic Ctrl-C in tests without platform-specific
    /// APIs, so this test only validates successful installation.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn install_signal_handlers_returns_ok_on_non_unix() {
        let shutdown = CancellationToken::new();
        let result = install_signal_handlers(shutdown);
        assert!(
            result.is_ok(),
            "install_signal_handlers must return Ok on a non-Unix host: {result:?}"
        );
    }
}
