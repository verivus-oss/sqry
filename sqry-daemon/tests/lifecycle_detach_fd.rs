//! Task 9 U14b — Unix FD-inheritance integration test.
//!
//! # M1 proof: `parent_to_grandchild_fd_inheritance_preserves_flock`
//!
//! This test validates the U3+U9 pidfile FD-inheritance path end-to-end:
//!
//! 1. Spawn the real `sqryd start --detach` binary with an isolated
//!    `XDG_RUNTIME_DIR` temp directory.
//! 2. Poll the pidfile until the **grandchild** PID appears.
//!    The grandchild writes its own PID to the pidfile via `write_pid_file_grandchild`
//!    **before** calling `PidfileLock::adopt`, so the pidfile is stable by the
//!    time the parent receives the ready signal and exits.
//! 3. On Linux, walk `/proc/<grandchild-pid>/fd/` to find the open FD whose
//!    resolved symlink target matches the canonical inode of `sqryd.lock`.
//! 4. Attempt `try_lock_exclusive` from **this** test process on a fresh FD
//!    to the same lockfile inode — must return `WouldBlock`, proving the
//!    grandchild still holds the inherited OFD-level exclusive flock.
//! 5. Send `SIGTERM` to the grandchild.
//! 6. Assert that after exit `try_lock_exclusive` on a fresh FD succeeds —
//!    proving the grandchild released the lock on shutdown.  This is verified
//!    via lock-acquisition polling rather than `/proc/<pid>` disappearance,
//!    which avoids false negatives on short-lived zombie entries.
//!
//! # Platform gating
//!
//! The entire test file is `#[cfg(all(unix, target_os = "linux"))]` because:
//!
//! - `/proc/<pid>/fd/` is a Linux procfs interface.
//! - The FD-inheritance mechanism (OFD-level flock surviving `fork`+`exec`)
//!   is the same on all Unix platforms, but the M1 proof strategy (reading
//!   the fd symlink to find the lock FD number) requires Linux procfs.
//! - On macOS/BSDs the equivalent is `lsof -p <pid>` which requires elevated
//!   privileges in many CI environments; the procfs approach is hermetic.
//!
//! # Binary location
//!
//! The `sqryd` binary is located via `CARGO_BIN_EXE_sqryd` (set by Cargo
//! during `cargo test --workspace`), with a fallback that walks up from
//! `std::env::current_exe()` to the `target/<profile>/` directory.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §D (pidfile locking), §C.3.2 (detach path FD inheritance).

#![cfg(all(unix, target_os = "linux"))]

use std::{
    fs, io,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Binary location helper
// ---------------------------------------------------------------------------

/// Locate the `sqryd` binary built by the workspace.
///
/// Checks `CARGO_BIN_EXE_sqryd` first (set by Cargo during `cargo test
/// --workspace`), then walks up from `std::env::current_exe()` to the
/// `target/<profile>/` directory.
///
/// Returns `None` when the binary cannot be located — the tests that call
/// this function skip themselves via `return` when `None` is returned.
fn find_sqryd_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    let binary_name = format!("sqryd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;
    // Integration test binaries live in target/debug/deps/; sqryd is at
    // target/debug/sqryd.
    let deps_dir = exe.parent()?; // target/debug/deps
    let candidate = deps_dir.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let profile_dir = deps_dir.parent()?; // target/debug
    let candidate = profile_dir.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

// ---------------------------------------------------------------------------
// RAII kill guard for the detached grandchild
// ---------------------------------------------------------------------------

/// Kills the detached `sqryd` grandchild process via `SIGKILL` on drop.
///
/// This guard ensures the daemon does not escape as an orphan if any test
/// assertion panics between the point where the parent exits and the
/// explicit `SIGTERM` + wait at the end of the test.
///
/// # Drop behaviour
///
/// Sends `SIGKILL` to `pid` and ignores all errors (the process may have
/// already exited).  Does NOT wait for the zombie — that is acceptable for
/// test teardown; the detached grandchild has been reparented to the system
/// subreaper (init/systemd) before the intermediate parent exits, so it is
/// the subreaper's responsibility to reap it, not the test process's.
struct DaemonGuard {
    pid: u32,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // SAFETY: libc::kill is async-signal-safe. The PID came from the
        // daemon's own pidfile. Errors are ignored: the process may have
        // already exited normally.
        let _ = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
    }
}

// ---------------------------------------------------------------------------
// Pidfile polling
// ---------------------------------------------------------------------------

/// Read the PID from `<dir>/sqry/sqryd.pid` (best-effort; returns `None` on
/// any error or if the file does not yet exist).
fn read_pid_file(dir: &Path) -> Option<u32> {
    let pid_path = dir.join("sqry").join("sqryd.pid");
    let text = fs::read_to_string(&pid_path).ok()?;
    text.trim().parse::<u32>().ok()
}

/// Poll `read_pid_file(dir)` until a non-zero PID appears or `timeout`
/// elapses.  Returns the PID on success, or `None` on timeout.
fn wait_for_pid_file(dir: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = read_pid_file(dir).filter(|&p| p > 0) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// M1 proof: inode-match via /proc/<pid>/fd
// ---------------------------------------------------------------------------

/// Find the FD number in `/proc/<pid>/fd/` whose symlink target resolves to
/// the same inode as `lockfile`.
///
/// Returns `Some(fd_number)` when a matching FD is found, `None` if no FD in
/// the process's FD table refers to the lockfile inode.
///
/// This operates by reading the symlinks in `/proc/<pid>/fd/` via
/// `fs::read_link`, then stating the resolved path and comparing inodes.
/// We also stat the lockfile directly (via the path provided by the parent)
/// so both sides use the **same inode** even if the file was moved (though the
/// lockfile is never unlinked per §D.4 of the design, so this is always
/// consistent).
fn find_lock_fd_in_proc(pid: u32, lockfile: &Path) -> io::Result<Option<u32>> {
    let lock_meta = fs::metadata(lockfile)?;
    let lock_ino = lock_meta.ino();
    let lock_dev = lock_meta.dev();

    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));

    let entries = match fs::read_dir(&fd_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // Cannot read another process's fd directory (e.g. different UID).
            // In CI we run as the same user that spawned sqryd, so this
            // should not happen. Propagate so the test can fail clearly.
            return Err(e);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Process may have exited. Return None: no lock FD found.
            return Ok(None);
        }
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = entry?;
        let fd_name = entry.file_name();
        let fd_str = fd_name.to_string_lossy();
        let Ok(fd_num) = fd_str.parse::<u32>() else {
            continue;
        };

        // Read the symlink target. The symlink for a regular file FD is
        // an absolute path like `/tmp/sqry-1000/sqry/sqryd.lock`.
        let target = match fs::read_link(entry.path()) {
            Ok(t) => t,
            Err(_) => continue, // FD may have closed between listing and read_link.
        };

        // Stat the target and compare device+inode. Using stat rather than
        // comparing paths handles hard-links and bind-mounts correctly.
        match fs::metadata(&target) {
            Ok(m) if m.ino() == lock_ino && m.dev() == lock_dev => {
                return Ok(Some(fd_num));
            }
            _ => continue,
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// flock WouldBlock helper
// ---------------------------------------------------------------------------

/// Returns `true` if `e` indicates a "would block" / "resource busy" locking
/// failure — i.e. the lock is held by another process.
fn is_would_block(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || e.raw_os_error()
            .is_some_and(|c| c == libc::EWOULDBLOCK || c == libc::EAGAIN)
}

// ---------------------------------------------------------------------------
// Signal helper
// ---------------------------------------------------------------------------

/// Send `SIGTERM` to `pid`.
fn send_sigterm(pid: u32) -> io::Result<()> {
    // SAFETY: libc::kill is async-signal-safe and pid is a valid process ID
    // obtained from the daemon's own pidfile. A negative return value is the
    // only error path.
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Poll until `try_lock_exclusive` on a fresh FD to `lockfile` succeeds or
/// `timeout` elapses.  Returns `true` if the lock was acquired within the
/// window (implying the prior holder has released it).
///
/// This is a more reliable "process exited and released lock" signal than
/// polling `/proc/<pid>` disappearance, which can linger briefly on zombies
/// even after the flock has been released by the kernel.
fn wait_for_lock_released(lockfile: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let fd = match fs::OpenOptions::new().read(true).write(true).open(lockfile) {
            Ok(f) => f,
            Err(_) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };

        match fs2::FileExt::try_lock_exclusive(&fd) {
            Ok(()) => {
                let _ = fs2::FileExt::unlock(&fd);
                return true;
            }
            Err(ref e) if is_would_block(e) => {
                // Lock still held.
            }
            Err(_) => {
                // Unexpected error — treat as "not released" and keep polling.
            }
        }
        drop(fd);

        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll until the process `pid` no longer exists (i.e. `kill(pid, 0)` returns
/// `ESRCH`) or `timeout` elapses.  Returns `true` if the process is gone
/// within the timeout window.
///
/// This is a secondary postcondition check used after `wait_for_lock_released`
/// to confirm the process has actually exited, not merely dropped its flock
/// early due to a bug.  Without this check, a regression that causes the
/// daemon to release the flock before exiting would disarm the [`DaemonGuard`]
/// while the process is still running, creating both a false positive and an
/// orphan risk.
fn wait_for_process_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: kill(pid, 0) is a standard existence check that does not
        // send any signal. Sending signal 0 returns 0 if the process exists
        // and we have permission to signal it, or -1 with ESRCH if it does
        // not exist. EPERM would mean the process exists but we cannot signal
        // it (different UID) — treat as still-running.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return true; // Process is gone.
            }
            // EPERM or other error — process still exists (or ambiguous).
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// M1 proof test
// ---------------------------------------------------------------------------

/// M1 proof: after `sqryd start --detach` completes, the grandchild holds an
/// exclusive OFD-level flock on `sqryd.lock` and releases it on shutdown.
///
/// **What this test proves (and what it does not):**
///
/// 1. The pidfile contains the grandchild PID after the grandchild calls
///    `write_pid_file_grandchild` (which runs before `PidfileLock::adopt`
///    in `run_start_spawned_by_client_unix`).
/// 2. The grandchild's FD table (via `/proc/<pid>/fd/`) contains an open FD
///    that resolves to the same inode as `sqryd.lock`.
/// 3. A fresh `try_lock_exclusive` attempt from the test process on a new FD
///    to the same lockfile returns `WouldBlock` — proving the grandchild
///    holds an exclusive OFD-level flock in its steady-state post-detach.
/// 4. After the grandchild receives `SIGTERM`, `try_lock_exclusive` on a fresh
///    FD eventually succeeds and `kill(pid, 0)` returns `ESRCH` — proving the
///    lock was released **and** the process actually exited.
///
/// Note: this test validates the steady-state post-detach invariant.  It does
/// not guarantee the flock was held *continuously without interruption* across
/// the parent→grandchild handoff — distinguishing an uninterrupted OFD
/// inheritance from an unlock+relock sequence would require injecting a second
/// observer at the moment of handoff, which is not possible from userspace
/// without modifying the production code.  The design correctness of the
/// handoff path (FD_CLOEXEC cleared before exec, OFD semantics across
/// fork+exec) is verified by code review; this test provides a runtime
/// regression guard for the post-detach observable state.
///
/// **Test isolation:** `XDG_RUNTIME_DIR` is overridden to a private `TempDir`
/// for the spawned process only (passed as an environment variable to the
/// child, not to this test process's own environment).  The test process
/// never manipulates the global `XDG_RUNTIME_DIR` env var, avoiding
/// interference with other concurrently-running tests.
///
/// **Skip if binary absent:** when `sqryd` cannot be located (e.g., the
/// workspace has not been built yet), the test returns early without failing.
/// This mirrors the convention in `sqry-lsp/tests/common/daemon_fixture.rs`.
///
/// **Kill-on-drop guard:** a [`DaemonGuard`] is installed as soon as the
/// grandchild PID is known.  If any assertion panics between PID acquisition
/// and the explicit SIGTERM, the guard sends SIGKILL to prevent the daemon
/// from escaping as an orphan.  The guard is only disarmed after both the
/// flock is released **and** `kill(pid, 0)` confirms the process has exited,
/// to prevent false positives if a regression causes early flock release.
#[test]
fn parent_to_grandchild_fd_inheritance_preserves_flock() {
    // ------------------------------------------------------------------
    // Step 1: Locate the sqryd binary.
    // ------------------------------------------------------------------
    let Some(sqryd_bin) = find_sqryd_binary() else {
        eprintln!(
            "parent_to_grandchild_fd_inheritance_preserves_flock: \
             skipping — sqryd binary not found (run `cargo build -p sqry-daemon` first)"
        );
        return;
    };

    // ------------------------------------------------------------------
    // Step 2: Create an isolated runtime directory.
    //
    // We point XDG_RUNTIME_DIR to a new TempDir for the spawned process.
    // The directory structure sqryd expects:
    //   <tmp>/sqry/sqryd.pid
    //   <tmp>/sqry/sqryd.lock
    //   <tmp>/sqry/sqryd.sock
    // ------------------------------------------------------------------
    let tmp = TempDir::new().expect("TempDir::new must succeed");
    let xdg_runtime = tmp.path().to_path_buf();

    let lockfile = xdg_runtime.join("sqry").join("sqryd.lock");
    let pidfile = xdg_runtime.join("sqry").join("sqryd.pid");

    // ------------------------------------------------------------------
    // Step 3: Spawn `sqryd start --detach`.
    //
    // The parent process (sqryd) will:
    //   a. Acquire the pidfile lock.
    //   b. Write the parent PID to sqryd.pid.
    //   c. Spawn the grandchild with the lock FD inherited.
    //   d. Exit 0 after the grandchild signals ready.
    //
    // The grandchild will:
    //   a. Write its own PID to sqryd.pid (before adopt).
    //   b. Call PidfileLock::adopt on the inherited FD.
    //   c. Run foreground startup and close the ready-pipe write end.
    // ------------------------------------------------------------------
    let mut parent = Command::new(&sqryd_bin)
        .args(["start", "--detach"])
        .env("XDG_RUNTIME_DIR", &xdg_runtime)
        // Inhibit log output to avoid noise in test output.
        .env("SQRY_DAEMON_LOG_LEVEL", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn sqryd start --detach");

    // ------------------------------------------------------------------
    // Step 4: Wait for the parent to exit (it exits 0 after handing off
    //         to the grandchild).
    // ------------------------------------------------------------------
    let parent_status = parent
        .wait()
        .expect("waiting for sqryd parent process must not fail");

    assert!(
        parent_status.success(),
        "sqryd start --detach parent must exit 0; got: {parent_status:?}"
    );

    // ------------------------------------------------------------------
    // Step 5: Poll the pidfile until the grandchild PID appears.
    //
    // The grandchild writes its own PID atomically via `write_pid_file_grandchild`
    // BEFORE calling `PidfileLock::adopt` (see `run_start_spawned_by_client_unix`).
    // The parent first wrote the parent PID; the grandchild's write overwrites it.
    // We poll until a non-zero PID is present; since the parent has already
    // exited at this point, any non-zero PID in the file is the grandchild PID.
    // ------------------------------------------------------------------
    let grandchild_pid =
        wait_for_pid_file(&xdg_runtime, Duration::from_secs(10)).unwrap_or_else(|| {
            panic!(
                "timed out waiting for grandchild PID to appear in pidfile \
                 (looked at {:?})",
                pidfile
            )
        });

    assert!(
        grandchild_pid > 1,
        "grandchild PID must be a real process ID, not 0 or 1; got {grandchild_pid}"
    );

    // Confirm the grandchild is actually running.
    let gc_proc = PathBuf::from(format!("/proc/{grandchild_pid}"));
    assert!(
        gc_proc.exists(),
        "grandchild process /proc/{grandchild_pid} must exist after detach"
    );

    // ------------------------------------------------------------------
    // Install a kill-on-drop guard so the daemon is not left as an orphan
    // if any subsequent assertion panics.
    //
    // The guard is disarmed (dropped without killing) at the end of the
    // test after the explicit SIGTERM.
    // ------------------------------------------------------------------
    let guard = DaemonGuard {
        pid: grandchild_pid,
    };

    // ------------------------------------------------------------------
    // Step 6: Find the lock FD in the grandchild's FD table via
    //         /proc/<grandchild-pid>/fd/.
    //
    // The grandchild inherits the lock FD across fork+exec with
    // FD_CLOEXEC cleared (set by the pre_exec hook in entrypoint.rs
    // §C.3.2 step C).  After exec, the FD remains open in the grandchild's
    // FD table and the symlink in /proc/<pid>/fd/<N> resolves to the
    // lockfile path (which maps to the same inode as sqryd.lock).
    // ------------------------------------------------------------------
    let found_fd = find_lock_fd_in_proc(grandchild_pid, &lockfile).unwrap_or_else(|e| {
        panic!("failed to scan /proc/{grandchild_pid}/fd/ for the lock FD: {e}")
    });

    let lock_fd_num = found_fd.unwrap_or_else(|| {
        // Collect the fd table for diagnostics.
        let fd_dir = format!("/proc/{grandchild_pid}/fd");
        let fds: Vec<String> = fs::read_dir(&fd_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let target = fs::read_link(e.path())
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| "<unreadable>".to_string());
                        format!("{name} -> {target}")
                    })
                    .collect()
            })
            .unwrap_or_default();

        panic!(
            "no FD in /proc/{grandchild_pid}/fd/ resolves to the same inode as \
             {:?}.\n\nFD table:\n{}",
            lockfile,
            fds.join("\n")
        )
    });

    // Log for test diagnostics (visible with `cargo test -- --nocapture`).
    eprintln!(
        "M1 proof: grandchild PID={grandchild_pid} holds \
         lock FD={lock_fd_num} → {:?}",
        lockfile
    );

    // ------------------------------------------------------------------
    // Step 7: Assert WouldBlock from a fresh FD.
    //
    // Open a fresh FD to the lockfile (the same inode the grandchild
    // holds).  Attempt try_lock_exclusive — must return WouldBlock
    // because the grandchild's inherited OFD-level exclusive flock is
    // still held.
    // ------------------------------------------------------------------
    let fresh_fd = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lockfile)
        .unwrap_or_else(|e| panic!("opening lockfile {:?} failed: {e}", lockfile));

    let lock_result = fs2::FileExt::try_lock_exclusive(&fresh_fd);
    assert!(
        lock_result.as_ref().err().is_some_and(is_would_block),
        "try_lock_exclusive on a fresh FD must return WouldBlock while the \
         grandchild (pid={grandchild_pid}) holds the inherited flock; \
         got: {lock_result:?}"
    );
    drop(fresh_fd);

    // ------------------------------------------------------------------
    // Step 8: Send SIGTERM to the grandchild.
    // ------------------------------------------------------------------
    send_sigterm(grandchild_pid)
        .unwrap_or_else(|e| panic!("SIGTERM to grandchild PID={grandchild_pid} failed: {e}"));

    // ------------------------------------------------------------------
    // Step 9: Poll for lock release.
    //
    // We poll `try_lock_exclusive` rather than `/proc/<pid>` disappearance.
    // The kernel releases OFD-level flocks as part of process teardown
    // (`do_exit` → FD table close → OFD refcount 0 → lock release) before
    // the zombie entry is collected.  Polling the lock directly avoids
    // spurious failures on systems where `/proc/<pid>` lingers briefly
    // after the flock is already released.
    // ------------------------------------------------------------------
    let lock_released = wait_for_lock_released(&lockfile, Duration::from_secs(10));
    assert!(
        lock_released,
        "sqryd.lock must be releasable within 10 s after SIGTERM to \
         grandchild (pid={grandchild_pid})"
    );

    // ------------------------------------------------------------------
    // Step 10: Confirm the process has actually exited (not just dropped
    //          the flock early).
    //
    // `wait_for_lock_released` alone is not sufficient to disarm the guard:
    // a regression that causes the daemon to release the flock before exiting
    // would disarm cleanup protection while leaving an orphan process behind.
    // `kill(pid, 0)` returning ESRCH is the authoritative "process is gone"
    // signal.
    // ------------------------------------------------------------------
    let process_gone = wait_for_process_gone(grandchild_pid, Duration::from_secs(5));
    assert!(
        process_gone,
        "grandchild (pid={grandchild_pid}) must exit within 5 s after releasing the flock"
    );

    // ------------------------------------------------------------------
    // Disarm the kill guard — both the flock has been released AND the
    // process has exited (confirmed by kill(pid,0) → ESRCH).
    // ------------------------------------------------------------------
    std::mem::forget(guard);

    // TempDir cleanup happens automatically on drop. The grandchild's
    // Drop should have removed the pidfile already; the lockfile is
    // intentionally never unlinked (§D.4), so TempDir::drop handles it.
}

// ---------------------------------------------------------------------------
// Additional sanity: lockfile inode is stable across the test window
// ---------------------------------------------------------------------------

/// Verify that the lockfile inode is **never** unlinked during the observable
/// portion of the detach lifecycle (§D.4 invariant): from the moment the
/// parent exits (lockfile already created and locked by startup), through the
/// grandchild's steady-state run, to after the grandchild exits — the
/// `sqryd.lock` file must persist with the same inode.
///
/// **Coverage window:** `pre_ino` is captured immediately after the parent
/// exits (at which point the lockfile exists and is held by the grandchild).
/// Any startup-time unlink/recreate would be visible as an inode change if it
/// occurred within the observable window (after parent exit).  Early-startup
/// unlink before parent exit is not observable from userspace without
/// modifying the production code.
///
/// The lockfile is intentionally never unlinked by the daemon itself —
/// inode stability is required for stale-recovery correctness.  Only external
/// tooling (e.g. `sqryd stop` or manual cleanup) may remove it.
///
/// Lock-release is verified via `try_lock_exclusive` polling; process exit is
/// confirmed via `kill(pid, 0)` → ESRCH before disarming the kill guard.
#[test]
fn lockfile_inode_survives_grandchild_exit() {
    let Some(sqryd_bin) = find_sqryd_binary() else {
        eprintln!(
            "lockfile_inode_survives_grandchild_exit: \
             skipping — sqryd binary not found"
        );
        return;
    };

    let tmp = TempDir::new().expect("TempDir::new");
    let xdg_runtime = tmp.path().to_path_buf();
    let lockfile = xdg_runtime.join("sqry").join("sqryd.lock");

    let mut parent = Command::new(&sqryd_bin)
        .args(["start", "--detach"])
        .env("XDG_RUNTIME_DIR", &xdg_runtime)
        .env("SQRY_DAEMON_LOG_LEVEL", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sqryd");

    let status = parent.wait().expect("wait for parent");
    assert!(
        status.success(),
        "sqryd --detach parent must exit 0; got {status:?}"
    );

    // Capture the lockfile inode immediately after the parent exits.
    // At this point the lockfile has already been created and locked by the
    // startup path; the grandchild has inherited the FD and is running.
    // Any unlink/recreate from here onward would appear as an inode change.
    let pre_ino = fs::metadata(&lockfile)
        .unwrap_or_else(|e| panic!("lockfile {:?} must exist after parent exits: {e}", lockfile))
        .ino();

    let grandchild_pid = wait_for_pid_file(&xdg_runtime, Duration::from_secs(10))
        .expect("grandchild PID must appear in pidfile within 10 s");

    // Install kill guard now that we have the grandchild PID.
    let guard = DaemonGuard {
        pid: grandchild_pid,
    };

    // Terminate the grandchild.
    send_sigterm(grandchild_pid).unwrap_or_else(|e| panic!("SIGTERM to PID={grandchild_pid}: {e}"));

    // Wait for the flock to be released (postcondition polling).
    let released = wait_for_lock_released(&lockfile, Duration::from_secs(10));
    assert!(released, "lock must be released within 10 s after SIGTERM");

    // Confirm the process has actually exited (not just dropped the flock
    // early) before disarming the kill guard.
    let process_gone = wait_for_process_gone(grandchild_pid, Duration::from_secs(5));
    assert!(
        process_gone,
        "grandchild (pid={grandchild_pid}) must exit within 5 s after releasing the flock"
    );

    // Disarm the guard — flock released AND process confirmed gone.
    std::mem::forget(guard);

    // The lockfile must still exist with the same inode (§D.4).
    let post_meta = fs::metadata(&lockfile).unwrap_or_else(|e| {
        panic!(
            "lockfile {:?} must still exist after grandchild exits (§D.4): {e}",
            lockfile
        )
    });
    assert_eq!(
        post_meta.ino(),
        pre_ino,
        "lockfile inode must be stable from parent exit through grandchild shutdown (§D.4)"
    );
}
