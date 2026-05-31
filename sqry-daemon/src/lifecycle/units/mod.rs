//! Service unit generators — Task 9 §F.
//!
//! Each submodule emits a platform-native service descriptor as a `String`.
//! The generators are pure functions: `&DaemonConfig` + `&InstallOptions` →
//! `String`. No OS API calls are made; tests run on any host regardless of
//! target OS.
//!
//! # Platform gating
//!
//! | Module      | `cfg` gate                   | Subcommand               |
//! |-------------|------------------------------|--------------------------|
//! | `systemd`   | `target_os = "linux"`        | `install-systemd-user/system` |
//! | `launchd`   | `target_os = "macos"`        | `install-launchd`        |
//! | `windows`   | `target_os = "windows"`      | `install-windows`        |
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §F.1–§F.4 + §F.5 (testing).

/// systemd user + system unit generators (Task 9 U6, Linux only).
///
/// Exports [`systemd::generate_user_unit`] and [`systemd::generate_system_unit`].
/// [`systemd::resolve_system_unit_user`] validates the `%i` account name before
/// `generate_system_unit` is called.
#[cfg(target_os = "linux")]
pub mod systemd;

/// launchd user-agent plist generator (Task 9 U7, macOS only).
///
/// Exports [`launchd::generate_plist`], [`launchd::default_install_path`],
/// and the constants [`launchd::PLIST_LABEL`] / [`launchd::INSTALL_PATH`].
#[cfg(target_os = "macos")]
pub mod launchd;

#[cfg(target_os = "windows")]
pub mod windows;

// ---------------------------------------------------------------------------
// Shared types.
// ---------------------------------------------------------------------------

/// Options forwarded to every service-unit generator.
///
/// Passed alongside `&DaemonConfig` to each `generate_*` function so that
/// per-install overrides (e.g. a specific service account on the system
/// systemd unit, or a custom label on the launchd plist) can be supplied
/// without touching the config file.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Optional user account name.
    ///
    /// - **systemd system unit** (U6): the `%i` template value, validated via
    ///   the POSIX account database (`getpwnam_r`) on Linux. Falls back to
    ///   `$USER` env if `None`. The generator exits `EX_CONFIG` (78) if the
    ///   account is unresolvable.
    /// - **Windows** (U8): the account passed to `sc.exe obj=` and the Task
    ///   Scheduler `UserId`. Defaults to `"LocalSystem"` for the service and
    ///   the current user for the Task Scheduler job if `None`.
    /// - Ignored by the systemd user unit and launchd generators (they always
    ///   run as the invoking user).
    pub user: Option<String>,

    /// Override the path to the `sqryd` binary embedded in the generated unit.
    ///
    /// When `None`, generators call [`std::env::current_exe()`] to resolve the
    /// running binary.  Set this to a fixed path in tests for portable,
    /// deterministic snapshot assertions.
    pub exe_path: Option<std::path::PathBuf>,

    /// Override the user home directory used by the launchd plist generator.
    ///
    /// When `None`, the launchd generator calls [`dirs::home_dir()`] (falling
    /// back to the `$HOME` environment variable) to resolve the invoking user's
    /// home directory.  Set this to a fixed path in tests to inject arbitrary
    /// home-directory values — including paths that contain XML-significant
    /// characters — for portable, deterministic assertions independent of the
    /// real home directory.
    ///
    /// Ignored by all non-launchd generators.
    pub home_dir: Option<std::path::PathBuf>,
}
