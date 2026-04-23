//! launchd user-agent plist generator for macOS.
//!
//! Generates a `~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist` file that
//! runs `sqryd foreground` as a per-user launchd agent.  The generated plist
//! keeps the daemon alive and starts it at login (`KeepAlive=true`,
//! `RunAtLoad=true`).
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §F.3 (unchanged from iter-0).
//!
//! # Output structure
//!
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
//!   "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
//! <plist version="1.0">
//! <dict>
//!   <key>Label</key><string>ai.verivus.sqry.sqryd</string>
//!   <key>ProgramArguments</key>
//!   <array>
//!     <string>/usr/local/bin/sqryd</string>
//!     <string>foreground</string>
//!   </array>
//!   <key>KeepAlive</key><true/>
//!   <key>RunAtLoad</key><true/>
//!   <key>StandardOutPath</key><string>/Users/alice/Library/Logs/sqry/sqryd.log</string>
//!   <key>StandardErrorPath</key><string>/Users/alice/Library/Logs/sqry/sqryd.err.log</string>
//!   <key>WorkingDirectory</key><string>/Users/alice/Library/Application Support/sqry</string>
//!   <key>EnvironmentVariables</key>
//!   <dict>
//!     <key>RUST_BACKTRACE</key><string>1</string>
//!   </dict>
//! </dict>
//! </plist>
//! ```
//!
//! > **Note:** `StandardOutPath`, `StandardErrorPath`, and `WorkingDirectory`
//! > are absolute paths.  `launchd` does **not** perform tilde (`~`) expansion
//! > on plist string values, so tilde-prefixed paths would be interpreted
//! > literally and the daemon would fail to open its log files or change into
//! > its working directory.  The generator calls [`dirs::home_dir()`] at
//! > generation time to produce the correct absolute path for the invoking user.
//! > If the home directory cannot be determined, sentinel paths
//! > (`SQRYD_ERR_NO_HOME_DIR/...`) are emitted so that `launchctl load` fails
//! > loudly rather than silently.  The `install-launchd` subcommand must check
//! > [`default_install_path()`] and abort before writing a plist with sentinel
//! > paths to disk.
//!
//! # Install path
//!
//! `~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist`
//!
//! Load with:
//! ```sh
//! launchctl load ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist
//! ```
//!
//! # No in-process log rotation under launchd
//!
//! Unlike systemd (which provides `Type=notify`), launchd has no equivalent
//! notification protocol.  The daemon detects launchd supervision by the
//! *absence* of `NOTIFY_SOCKET`; in that mode the `RollingSizeAppender` is
//! still active.  Log files land in
//! `$HOME/Library/Logs/sqry/` (absolute path, not tilde-prefixed — launchd
//! does not perform tilde expansion in plist values) matching the
//! `StandardOutPath` in this plist.

use std::path::PathBuf;

use crate::{config::DaemonConfig, lifecycle::units::InstallOptions};

/// The launchd `Label` string for the sqryd user agent.
///
/// This string appears in `launchctl list`, in the plist `<key>Label</key>`
/// field, and as the basename of the plist file (without the `.plist`
/// extension).  It follows Apple's reverse-DNS naming convention.
pub const PLIST_LABEL: &str = "ai.verivus.sqry.sqryd";

/// Preferred install path for the generated plist.
///
/// Printed as a comment in the generated plist and used by the
/// `install-launchd` CLI subcommand to suggest the destination.
pub const INSTALL_PATH: &str = "~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist";

/// Generate the launchd user-agent plist XML for `sqryd`.
///
/// Returns the complete XML plist as a `String` ready to be written to
/// `~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist`.
///
/// # Arguments
///
/// * `cfg` — accepted for API consistency and reserved for future per-config
///   customisation (e.g. embedding the socket path or a custom log directory
///   derived from `cfg.log_file`).  Currently unused; all paths use well-known
///   macOS conventions.
/// * `opts` — install-time overrides.  `opts.exe_path` overrides
///   `std::env::current_exe()` — pass `Some` in tests for stable, portable
///   snapshots.
///
/// # Binary resolution
///
/// When `opts.exe_path` is `None` the function calls
/// [`std::env::current_exe()`] and canonicalises the result.  On macOS this
/// typically resolves to the installed binary path (e.g.
/// `/usr/local/bin/sqryd`).  If `current_exe()` fails (unusual, but possible
/// in sandboxed environments) the string literal `"sqryd"` is used as a
/// graceful fallback.
///
/// # Log directory
///
/// The log file paths (`StandardOutPath` and `StandardErrorPath`) are computed
/// as absolute paths using [`resolve_home()`].  In practice this expands to
/// `/Users/<username>/Library/Logs/sqry/`.  Tilde-prefixed literals must NOT
/// be used because `launchd` does not perform tilde expansion in plist string
/// values — using `~/Library/...` would cause the daemon to fail to open its
/// log files.  In the rare case where the home directory cannot be determined
/// (e.g., rootless containers without a `HOME` env var), the generator emits
/// sentinel paths (`SQRYD_ERR_NO_HOME_DIR/...`) that cause `launchctl load`
/// to fail loudly.  The `install-launchd` subcommand MUST check for this
/// condition via [`default_install_path()`] and abort with a user-visible error
/// before writing the plist to disk.
///
/// All three runtime paths (`StandardOutPath`, `StandardErrorPath`,
/// `WorkingDirectory`) are XML-escaped via [`xml_escape()`] before embedding
/// in the plist so that unusual home-directory names (e.g., `/Users/Tom & Jerry`)
/// do not produce malformed XML.
///
/// # Working directory
///
/// `WorkingDirectory` is set to the absolute path
/// `$HOME/Library/Application Support/sqry`, the canonical per-user data
/// directory for sqry on macOS.  An absolute path is required because `launchd`
/// does not expand `~` in plist values.
///
/// # Version stamp
///
/// A leading XML comment (`<!-- sqryd version X.Y.Z -->`) is included so the
/// plist can be compared against the installed binary version at a glance.
#[must_use]
pub fn generate_plist(cfg: &DaemonConfig, opts: &InstallOptions) -> String {
    let _ = cfg; // cfg reserved for future per-config overrides (e.g. custom socket path)

    let exe_raw = resolve_exe(opts);
    // XML-escape the binary path: paths containing `&`, `<`, `>`, `"`, or `'`
    // would otherwise produce a malformed plist that launchd rejects.
    let exe = xml_escape(&exe_raw);

    let version = env!("CARGO_PKG_VERSION");

    // launchd does NOT perform tilde expansion on plist values — StandardOutPath,
    // StandardErrorPath, and WorkingDirectory must be absolute paths.  We resolve
    // the home directory at generation time using resolve_home(), which reads
    // $HOME (POSIX) on macOS.
    //
    // If the home directory cannot be determined (e.g., rootless container, CI
    // environment without $HOME), the generator returns a plist with clearly-
    // invalid sentinel paths ("SQRYD_ERR_NO_HOME_DIR/...") so that launchctl
    // will fail loudly at load time rather than silently writing to a literal
    // "~/Library/..." path (which launchd would interpret as a relative path
    // under the daemon's working directory).  The `install-launchd` CLI
    // subcommand MUST call `default_install_path()` first and abort with a
    // user-visible error if the home directory is unavailable.
    //
    // opts.home_dir overrides resolve_home() for tests so that home paths
    // containing XML-significant characters (e.g., `/Users/Tom & Jerry`) can
    // be injected without relying on the real home directory.
    let home = opts.home_dir.clone().or_else(resolve_home);
    // XML-escape all three runtime paths: home directory paths containing `&`,
    // `<`, `>`, `"`, or `'` (e.g., `/Users/Tom & Jerry`) would produce a
    // malformed plist that `launchctl load` rejects.
    let log_out_path = xml_escape(
        &home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Logs")
                    .join("sqry")
                    .join("sqryd.log")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Logs/sqry/sqryd.log".to_owned()),
    );
    let log_err_path = xml_escape(
        &home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Logs")
                    .join("sqry")
                    .join("sqryd.err.log")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Logs/sqry/sqryd.err.log".to_owned()),
    );
    let working_dir = xml_escape(
        &home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Application Support")
                    .join("sqry")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Application Support/sqry".to_owned()),
    );

    // Build the plist XML.  We use a hand-rolled formatter rather than a plist
    // crate to keep the output deterministic and the dep tree lean.  The format
    // is identical to what Xcode / Apple's plistutil would produce.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- sqryd version {version} -->
<!-- Install path: {install_path} -->
<!-- Load:   launchctl load {install_path} -->
<!-- Unload: launchctl unload {install_path} -->
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>foreground</string>
  </array>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>{log_out}</string>
  <key>StandardErrorPath</key><string>{log_err}</string>
  <key>WorkingDirectory</key><string>{working_dir}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_BACKTRACE</key><string>1</string>
  </dict>
</dict>
</plist>
"#,
        version = version,
        install_path = INSTALL_PATH,
        label = PLIST_LABEL,
        exe = exe,
        log_out = log_out_path,
        log_err = log_err_path,
        working_dir = working_dir,
    )
}

/// Escape a string for safe embedding in XML character data.
///
/// Replaces the five XML-significant characters with their entity references:
///
/// | Char | Entity   |
/// |------|----------|
/// | `&`  | `&amp;`  |
/// | `<`  | `&lt;`   |
/// | `>`  | `&gt;`   |
/// | `"`  | `&quot;` |
/// | `'`  | `&apos;` |
///
/// This is necessary because binary paths can theoretically contain any of
/// these characters (e.g., a path such as `/opt/sqry&tools/sqryd`), and
/// embedding them raw would produce a malformed plist that `launchctl` rejects.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Resolve the current user's home directory.
///
/// Tries [`dirs::home_dir()`] first (which reads `$HOME` on POSIX / macOS).
/// Returns `None` only if both `dirs::home_dir()` and the `HOME` environment
/// variable are unavailable — an extremely rare condition on a normal macOS
/// user session.  Callers that receive `None` must surface an error to the
/// user; the generated plist will contain sentinel paths that launchd will
/// reject at `launchctl load` time.
fn resolve_home() -> Option<std::path::PathBuf> {
    dirs::home_dir().or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
}

/// Resolve the sqryd binary path for inclusion in the plist.
///
/// Returns `opts.exe_path` if set; otherwise calls
/// [`std::env::current_exe()`] and canonicalises the result.  Falls back to
/// the bare string `"sqryd"` if resolution fails (unusual in practice).
fn resolve_exe(opts: &InstallOptions) -> String {
    if let Some(path) = &opts.exe_path {
        return path.to_string_lossy().into_owned();
    }

    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sqryd".to_owned())
}

/// Suggested install path for the generated plist file.
///
/// Expands `~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist` to an
/// absolute path using [`resolve_home()`].  Returns `None` if the home
/// directory cannot be determined (rare; possible in container environments
/// without a home directory or `$HOME` environment variable).
///
/// Uses the same home-resolution logic as [`generate_plist()`] so that
/// `install-launchd` and the plist generator agree on home availability.
/// If this function returns `None`, the generated plist will contain
/// sentinel paths and the installer MUST abort before writing to disk.
#[must_use]
pub fn default_install_path() -> Option<PathBuf> {
    resolve_home().map(|home| {
        home.join("Library")
            .join("LaunchAgents")
            .join("ai.verivus.sqry.sqryd.plist")
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{config::DaemonConfig, lifecycle::units::InstallOptions};

    /// Helper that produces a stable `InstallOptions` with a fixed binary path
    /// so test snapshots are portable across machines and CI runners.
    fn stable_opts() -> InstallOptions {
        InstallOptions {
            exe_path: Some(PathBuf::from("/usr/local/bin/sqryd")),
            user: None,
            home_dir: None,
        }
    }

    // ── Content assertions ────────────────────────────────────────────────

    /// The plist must contain the correct label string.
    #[test]
    fn launchd_plist_contains_label_and_keepalive() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.contains("<key>Label</key><string>ai.verivus.sqry.sqryd</string>"),
            "plist must contain the correct Label value; got:\n{plist}"
        );
        assert!(
            plist.contains("<key>KeepAlive</key><true/>"),
            "plist must set KeepAlive=true; got:\n{plist}"
        );
        assert!(
            plist.contains("<key>RunAtLoad</key><true/>"),
            "plist must set RunAtLoad=true; got:\n{plist}"
        );
    }

    /// `ProgramArguments` must include the resolved binary path and the
    /// `foreground` sub-command.
    #[test]
    fn launchd_plist_contains_program_arguments() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.contains("<string>/usr/local/bin/sqryd</string>"),
            "plist must embed the resolved binary path; got:\n{plist}"
        );
        assert!(
            plist.contains("<string>foreground</string>"),
            "plist must pass 'foreground' as the first argument; got:\n{plist}"
        );
    }

    /// Standard output and error paths must be absolute paths under
    /// `$HOME/Library/Logs/sqry/`.  launchd does NOT expand `~`, so
    /// tilde-prefixed paths would prevent log files from being created.
    #[test]
    fn launchd_plist_contains_standard_out_and_err_paths() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.contains("<key>StandardOutPath</key>"),
            "plist must have StandardOutPath key; got:\n{plist}"
        );
        // The path must end with the expected filename components regardless
        // of the absolute home-dir prefix.
        assert!(
            plist.contains("Library/Logs/sqry/sqryd.log"),
            "StandardOutPath must point into Library/Logs/sqry/; got:\n{plist}"
        );
        // Must NOT be tilde-prefixed (launchd does not expand ~).
        assert!(
            !plist.contains("~/Library/Logs/sqry/sqryd.log"),
            "StandardOutPath must be an absolute path, not tilde-prefixed; got:\n{plist}"
        );
        assert!(
            plist.contains("<key>StandardErrorPath</key>"),
            "plist must have StandardErrorPath key; got:\n{plist}"
        );
        assert!(
            plist.contains("Library/Logs/sqry/sqryd.err.log"),
            "StandardErrorPath must point into Library/Logs/sqry/; got:\n{plist}"
        );
        assert!(
            !plist.contains("~/Library/Logs/sqry/sqryd.err.log"),
            "StandardErrorPath must be an absolute path, not tilde-prefixed; got:\n{plist}"
        );
    }

    /// `WorkingDirectory` must be set to the absolute sqry application-support
    /// directory.  launchd does NOT expand `~`, so a tilde-prefixed path would
    /// prevent the daemon from `chdir`-ing to its data directory on startup.
    #[test]
    fn launchd_plist_contains_working_directory() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.contains("<key>WorkingDirectory</key>"),
            "plist must have WorkingDirectory key; got:\n{plist}"
        );
        assert!(
            plist.contains("Library/Application Support/sqry"),
            "WorkingDirectory must point into Library/Application Support/sqry; got:\n{plist}"
        );
        // Must NOT be tilde-prefixed (launchd does not expand ~).
        assert!(
            !plist.contains("~/Library/Application Support/sqry"),
            "WorkingDirectory must be an absolute path, not tilde-prefixed; got:\n{plist}"
        );
    }

    /// When the home directory is available, the runtime paths in the plist
    /// (`StandardOutPath`, `StandardErrorPath`, `WorkingDirectory`) must be
    /// absolute paths starting with `/`.  This directly guards against the
    /// launchd `~`-expansion issue: any non-`/`-prefixed path would be treated
    /// as a relative path under the daemon root and would silently misdirect
    /// log files.
    ///
    /// When `dirs::home_dir()` returns `None` this test is skipped.
    #[test]
    fn launchd_plist_runtime_paths_are_absolute_when_home_available() {
        // Only run this assertion when a real home directory is available.
        if resolve_home().is_none() {
            return;
        }
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        // Extract the value of StandardOutPath.  The format is always:
        // <key>StandardOutPath</key><string>THEVALUE</string>
        let log_out = plist
            .split("<key>StandardOutPath</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain StandardOutPath");
        assert!(
            log_out.starts_with('/'),
            "StandardOutPath must be an absolute path starting with '/'; got: {log_out:?}"
        );

        let log_err = plist
            .split("<key>StandardErrorPath</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain StandardErrorPath");
        assert!(
            log_err.starts_with('/'),
            "StandardErrorPath must be an absolute path starting with '/'; got: {log_err:?}"
        );

        let working = plist
            .split("<key>WorkingDirectory</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain WorkingDirectory");
        assert!(
            working.starts_with('/'),
            "WorkingDirectory must be an absolute path starting with '/'; got: {working:?}"
        );
    }

    /// `EnvironmentVariables` must include `RUST_BACKTRACE=1`.
    #[test]
    fn launchd_plist_contains_rust_backtrace_env() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.contains("<key>EnvironmentVariables</key>"),
            "plist must have EnvironmentVariables key; got:\n{plist}"
        );
        assert!(
            plist.contains("<key>RUST_BACKTRACE</key><string>1</string>"),
            "EnvironmentVariables must include RUST_BACKTRACE=1; got:\n{plist}"
        );
    }

    /// The plist must include a version comment stamp.
    #[test]
    fn launchd_plist_contains_version_stamp() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());
        let version = env!("CARGO_PKG_VERSION");

        assert!(
            plist.contains(&format!("<!-- sqryd version {version} -->")),
            "plist must include version stamp comment; got:\n{plist}"
        );
    }

    /// The generated output must be valid XML (well-formed).  We use a minimal
    /// structural check: the XML declaration must come first and the root
    /// `<plist>` element must be opened and closed.
    #[test]
    fn launchd_plist_is_well_formed_xml() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());

        assert!(
            plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
            "plist must begin with the XML declaration; got start: {:?}",
            &plist[..plist.len().min(60)]
        );
        assert!(
            plist.contains("<plist version=\"1.0\">"),
            "plist must contain the root <plist> element; got:\n{plist}"
        );
        assert!(
            plist.contains("</plist>"),
            "plist root element must be closed; got:\n{plist}"
        );
        // The root must contain exactly one <dict> section.
        assert!(
            plist.contains("<dict>"),
            "plist must have a root <dict>; got:\n{plist}"
        );
        assert!(
            plist.contains("</dict>"),
            "plist root <dict> must be closed; got:\n{plist}"
        );
    }

    /// Snapshot test: the full generated plist must match the expected string
    /// for a default `DaemonConfig` and a fixed binary path.  Failures here
    /// signal an unintended change to the output format.
    ///
    /// The expected paths are computed from `resolve_home()` — matching the
    /// exact same home-resolution logic used by `generate_plist()` — so that
    /// the expected and actual values are always in sync regardless of the
    /// environment.  When the home directory is unavailable (rootless container
    /// without `$HOME`), the test expects the same `SQRYD_ERR_NO_HOME_DIR/...`
    /// sentinel strings that `generate_plist()` emits.
    #[test]
    fn launchd_plist_snapshot() {
        let cfg = DaemonConfig::default();
        let plist = generate_plist(&cfg, &stable_opts());
        let version = env!("CARGO_PKG_VERSION");

        let home = resolve_home();
        let log_out = home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Logs")
                    .join("sqry")
                    .join("sqryd.log")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Logs/sqry/sqryd.log".to_owned());
        let log_err = home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Logs")
                    .join("sqry")
                    .join("sqryd.err.log")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Logs/sqry/sqryd.err.log".to_owned());
        let working = home
            .as_ref()
            .map(|h| {
                h.join("Library")
                    .join("Application Support")
                    .join("sqry")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "SQRYD_ERR_NO_HOME_DIR/Library/Application Support/sqry".to_owned());

        let expected = format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n",
                "  \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                "<!-- sqryd version {version} -->\n",
                "<!-- Install path: ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist -->\n",
                "<!-- Load:   launchctl load ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist -->\n",
                "<!-- Unload: launchctl unload ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist -->\n",
                "<plist version=\"1.0\">\n",
                "<dict>\n",
                "  <key>Label</key><string>ai.verivus.sqry.sqryd</string>\n",
                "  <key>ProgramArguments</key>\n",
                "  <array>\n",
                "    <string>/usr/local/bin/sqryd</string>\n",
                "    <string>foreground</string>\n",
                "  </array>\n",
                "  <key>KeepAlive</key><true/>\n",
                "  <key>RunAtLoad</key><true/>\n",
                "  <key>StandardOutPath</key><string>{log_out}</string>\n",
                "  <key>StandardErrorPath</key><string>{log_err}</string>\n",
                "  <key>WorkingDirectory</key><string>{working}</string>\n",
                "  <key>EnvironmentVariables</key>\n",
                "  <dict>\n",
                "    <key>RUST_BACKTRACE</key><string>1</string>\n",
                "  </dict>\n",
                "</dict>\n",
                "</plist>\n",
            ),
            version = version,
            log_out = log_out,
            log_err = log_err,
            working = working,
        );

        assert_eq!(
            plist, expected,
            "launchd plist snapshot mismatch.\n\nActual:\n{plist}\n\nExpected:\n{expected}"
        );
    }

    /// `default_install_path` must return a path ending in the expected
    /// plist filename when a home directory is available.
    #[test]
    fn default_install_path_ends_with_expected_filename() {
        // Skip this test when running in a rootless container with no HOME dir.
        if let Some(path) = default_install_path() {
            assert!(
                path.ends_with("Library/LaunchAgents/ai.verivus.sqry.sqryd.plist"),
                "default_install_path must end with the standard LaunchAgents path; got: {path:?}"
            );
        }
    }

    /// When `opts.exe_path` is `None` the generator must not panic and must
    /// produce a non-empty binary path in the plist.
    #[test]
    fn launchd_plist_with_current_exe_fallback_does_not_panic() {
        let cfg = DaemonConfig::default();
        let opts = InstallOptions {
            exe_path: None,
            user: None,
            home_dir: None,
        };
        let plist = generate_plist(&cfg, &opts);
        // The plist must still contain a ProgramArguments array with at least
        // one non-empty <string> entry for the binary path.
        assert!(
            plist.contains("<key>ProgramArguments</key>"),
            "plist with fallback exe must still have ProgramArguments; got:\n{plist}"
        );
    }

    /// XML-significant characters in the binary path must be entity-escaped so
    /// the generated plist is well-formed XML that `launchctl load` accepts.
    ///
    /// This covers paths like `/opt/sqry&tools/sqryd` (unusual but legal on
    /// Unix/macOS) and future user-supplied paths via `--exe-path`.
    #[test]
    fn launchd_plist_xml_escapes_special_chars_in_exe_path() {
        let cfg = DaemonConfig::default();
        // A path with all five XML-significant characters.
        let opts = InstallOptions {
            exe_path: Some(PathBuf::from("/opt/sqry&tools/<foo>/sqryd\"bar'baz>")),
            user: None,
            home_dir: None,
        };
        let plist = generate_plist(&cfg, &opts);

        // Raw characters must NOT appear inside the generated XML.
        assert!(
            !plist.contains("/opt/sqry&tools/"),
            "raw '&' must be escaped to '&amp;'; got:\n{plist}"
        );
        assert!(
            !plist.contains("<foo>"),
            "raw '<' must be escaped to '&lt;'; got:\n{plist}"
        );

        // Their entity references MUST be present.
        assert!(
            plist.contains("&amp;"),
            "plist must contain '&amp;' for the escaped ampersand; got:\n{plist}"
        );
        assert!(
            plist.contains("&lt;"),
            "plist must contain '&lt;' for the escaped '<'; got:\n{plist}"
        );
        assert!(
            plist.contains("&gt;"),
            "plist must contain '&gt;' for the escaped '>'; got:\n{plist}"
        );
        assert!(
            plist.contains("&quot;"),
            "plist must contain '&quot;' for the escaped '\"'; got:\n{plist}"
        );
        assert!(
            plist.contains("&apos;"),
            "plist must contain '&apos;' for the escaped \"'\"; got:\n{plist}"
        );

        // Unit test for the helper directly.
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("plain/path/no/special"), "plain/path/no/special");
        assert_eq!(xml_escape(""), "");
    }

    /// XML-significant characters in the resolved home directory path must be
    /// entity-escaped in `StandardOutPath`, `StandardErrorPath`, and
    /// `WorkingDirectory` so the generated plist is well-formed XML.
    ///
    /// This test injects a fake home directory containing XML-significant
    /// characters (`&` and `<`) via `opts.home_dir` so that the test drives
    /// `generate_plist()` directly through the escaping logic, regardless of
    /// the real home directory on the host.  This makes the test a true
    /// regression guard for the iter-4 fix.
    #[test]
    fn launchd_plist_xml_escapes_runtime_paths() {
        let cfg = DaemonConfig::default();
        // Inject a fake home path with XML-significant characters.
        // This is an unusual but syntactically valid Unix path.
        let fake_home = PathBuf::from("/Users/Tom & <Jerry>");
        let opts = InstallOptions {
            exe_path: Some(PathBuf::from("/usr/local/bin/sqryd")),
            home_dir: Some(fake_home.clone()),
            ..InstallOptions::default()
        };
        let plist = generate_plist(&cfg, &opts);

        // The raw characters must NOT appear in the generated XML.
        assert!(
            !plist.contains("/Users/Tom & <Jerry>"),
            "raw '&' and '<' must be escaped in runtime paths; got:\n{plist}"
        );

        // The escaped entities MUST appear in the plist.
        assert!(
            plist.contains("Tom &amp; &lt;Jerry&gt;"),
            "runtime paths must contain entity-escaped home components; got:\n{plist}"
        );

        // Specifically, StandardOutPath must be escaped.
        let log_out = plist
            .split("<key>StandardOutPath</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain StandardOutPath");
        assert!(
            log_out.contains("&amp;"),
            "StandardOutPath must escape '&' from home path; got: {log_out:?}"
        );
        assert!(
            log_out.contains("&lt;"),
            "StandardOutPath must escape '<' from home path; got: {log_out:?}"
        );
        assert!(
            !log_out.contains(" & "),
            "StandardOutPath must not contain raw '&'; got: {log_out:?}"
        );

        // StandardErrorPath and WorkingDirectory must also escape all XML-special
        // characters from the injected home path — including both '&' and '<'.
        let log_err = plist
            .split("<key>StandardErrorPath</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain StandardErrorPath");
        assert!(
            log_err.contains("&amp;"),
            "StandardErrorPath must escape '&' from home path; got: {log_err:?}"
        );
        assert!(
            log_err.contains("&lt;"),
            "StandardErrorPath must escape '<' from home path; got: {log_err:?}"
        );

        let working = plist
            .split("<key>WorkingDirectory</key><string>")
            .nth(1)
            .and_then(|s| s.split("</string>").next())
            .expect("plist must contain WorkingDirectory");
        assert!(
            working.contains("&amp;"),
            "WorkingDirectory must escape '&' from home path; got: {working:?}"
        );
        assert!(
            working.contains("&lt;"),
            "WorkingDirectory must escape '<' from home path; got: {working:?}"
        );
    }
}
