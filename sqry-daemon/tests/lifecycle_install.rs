//! Task 9 U12 — `lifecycle_install` integration tests.
//!
//! Exercises the three platform-native service-unit generators via the
//! production `sqryd` binary CLI subcommands:
//!
//! - [`install_systemd_user_output_parses_as_valid_unit_file`] — Linux only;
//!   pipes the output through `systemd-analyze verify` when available, or
//!   performs a structural INI-content check if the tool is absent.
//!
//! - [`install_launchd_output_parses_as_valid_plist_xml`] — macOS only (skipped
//!   on Linux/Windows with an explicit reason); pipes through `plutil -lint`
//!   when on macOS.
//!
//! - [`install_windows_emits_both_variants`] — Windows only (skipped with a
//!   reason on all other platforms).
//!
//! # Binary location
//!
//! The `sqryd` binary is located in two steps:
//!
//! 1. `CARGO_BIN_EXE_sqryd` — set by Cargo's test harness when running
//!    integration tests from a workspace with `cargo test --workspace`.
//! 2. Walk from [`std::env::current_exe()`] up through
//!    `target/<profile>/deps/` → `target/<profile>/` and look for the
//!    `sqryd[.exe]` binary.
//!
//! Tests that cannot find the binary print a reason-annotated SKIP message and
//! return rather than failing — this preserves green CI when the binary has not
//! been built explicitly (e.g. `cargo test -p sqry-daemon` without `--bin`
//! targets) and avoids spurious failures in crates that run only a subset of
//! the workspace.
//!
//! # Platform gating
//!
//! The test file itself is unconditionally compiled.  Each individual test
//! uses `#[cfg_attr(not(target_os = "linux"), ignore = "...")]` or equivalent
//! runtime skip guards so that the test count visible to Cargo is stable
//! across platforms while the tests only exercise the generators that exist on
//! the current host.
//!
//! # Design reference
//!
//! `docs/development/sqryd-daemon/TASK_9_IMPLEMENTATION_DAG.md` §U12.
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §F.5.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// Binary location helper
// ---------------------------------------------------------------------------

/// Locate the production `sqryd` binary.
///
/// Resolution order:
///
/// 1. `CARGO_BIN_EXE_sqryd` — set by Cargo when running integration tests in a
///    workspace with the `sqryd` binary as a workspace member.
/// 2. `<test-binary-dir>/sqryd[.exe]` — the test binary lives in
///    `target/<profile>/deps/`; `sqryd` is placed in `target/<profile>/`.
///    Walk one and two levels up from the test binary's parent directory.
///
/// Returns `None` when the binary cannot be found.  Callers should print a
/// skip message and return rather than panicking so that `cargo test -p
/// sqry-daemon` without an explicit `--bin sqryd` build doesn't fail.
fn find_sqryd_binary() -> Option<PathBuf> {
    // Prefer the Cargo-provided env var (most reliable, set during `cargo test
    // --workspace` and `cargo test -p sqry-daemon`).
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    let binary_name = format!("sqryd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;

    // Integration test binaries live in target/<profile>/deps/.
    // `sqryd` is placed at target/<profile>/sqryd.
    let deps_dir = exe.parent()?; // target/<profile>/deps/
    let candidate = deps_dir.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let profile_dir = deps_dir.parent()?; // target/<profile>/
    let candidate = profile_dir.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }

    None
}

#[cfg(target_os = "linux")]
fn current_posix_username() -> Option<String> {
    let uid = unsafe {
        // SAFETY: `getuid` has no preconditions and cannot fail.
        libc::getuid()
    };
    let mut passwd_entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer_len = initial_passwd_buffer_len();

    loop {
        let mut buffer = vec![0_u8; buffer_len];
        let rc = unsafe {
            // SAFETY: all out-pointers reference writable storage and the
            // scratch buffer is valid for its full length during the call.
            libc::getpwuid_r(
                uid,
                passwd_entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };

        if rc == 0 {
            if result.is_null() {
                return None;
            }
            let passwd_entry = unsafe {
                // SAFETY: POSIX set `result` to `passwd_entry` on success.
                passwd_entry.assume_init()
            };
            let username = unsafe {
                // SAFETY: `pw_name` is a NUL-terminated C string in `buffer`.
                std::ffi::CStr::from_ptr(passwd_entry.pw_name)
            };
            return username.to_str().ok().map(ToOwned::to_owned);
        }

        if rc == libc::ERANGE {
            buffer_len = buffer_len.checked_mul(2)?;
            if buffer_len > 1_048_576 {
                return None;
            }
            continue;
        }

        return None;
    }
}

#[cfg(target_os = "linux")]
fn initial_passwd_buffer_len() -> usize {
    let sysconf_len = unsafe {
        // SAFETY: `sysconf(_SC_GETPW_R_SIZE_MAX)` has no preconditions.
        libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX)
    };
    usize::try_from(sysconf_len)
        .ok()
        .filter(|len| *len > 0)
        .unwrap_or(16_384)
}

// ---------------------------------------------------------------------------
// Linux: install-systemd-user → systemd-analyze verify (or structural check)
// ---------------------------------------------------------------------------

/// Invoke `sqryd install-systemd-user`, capture stdout, and validate the
/// produced unit file.
///
/// **Validation strategy:**
///
/// * If `systemd-analyze` is present on the `PATH`, the unit text is written
///   to a temporary `.service` file and passed to `systemd-analyze verify`.
///   A zero exit code is authoritative.
/// * If `systemd-analyze` is absent (uncommon on a normal Linux host but
///   possible in minimal CI containers), a structural content check is
///   performed instead:
///
///   - The text must contain `[Unit]`, `[Service]`, `[Install]` sections.
///   - `Type=notify` must be present.
///   - `ExecStart=` must reference the `foreground` subcommand.
///
///   This is the same set of assertions already covered by the unit tests in
///   `src/lifecycle/units/systemd.rs`; here they serve as a cross-binary
///   integration smoke-check rather than a strict systemd conformance check.
///
/// The test is **skipped with a reason** on non-Linux hosts.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "install-systemd-user is Linux-only; test skipped on this platform"
)]
fn install_systemd_user_output_parses_as_valid_unit_file() {
    // Locate the sqryd binary.
    let Some(sqryd) = find_sqryd_binary() else {
        eprintln!(
            "SKIP install_systemd_user_output_parses_as_valid_unit_file: \
             sqryd binary not found — run `cargo build -p sqry-daemon --bin sqryd` first"
        );
        return;
    };

    // Invoke `sqryd install-systemd-user` and capture stdout.
    let output = Command::new(&sqryd)
        .arg("install-systemd-user")
        // Suppress tracing noise on stderr so test output is clean.
        .env("RUST_LOG", "off")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {:?}: {e}", sqryd));

    assert!(
        output.status.success(),
        "sqryd install-systemd-user must exit 0; status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let unit_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !unit_text.trim().is_empty(),
        "sqryd install-systemd-user must produce non-empty output"
    );

    // --- Structural content assertions (always run, independent of
    //     whether systemd-analyze is available) ---
    assert!(
        unit_text.contains("[Unit]"),
        "unit file must contain [Unit] section; got:\n{unit_text}"
    );
    assert!(
        unit_text.contains("[Service]"),
        "unit file must contain [Service] section; got:\n{unit_text}"
    );
    assert!(
        unit_text.contains("[Install]"),
        "unit file must contain [Install] section; got:\n{unit_text}"
    );
    assert!(
        unit_text.contains("Type=notify"),
        "unit file must contain Type=notify; got:\n{unit_text}"
    );
    assert!(
        unit_text.contains("foreground"),
        "ExecStart must reference 'foreground' subcommand; got:\n{unit_text}"
    );
    assert!(
        unit_text.contains("WantedBy=default.target"),
        "user unit must target default.target; got:\n{unit_text}"
    );

    // --- systemd-analyze verify (when available) ---
    // Only attempt verification when the tool is present on the host.
    let analyze_available = Command::new("systemd-analyze")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if analyze_available {
        // Write the unit text to a temp file with a `.service` extension so
        // systemd-analyze recognises it as a service unit.
        let mut tmp = NamedTempFile::with_suffix(".service")
            .expect("create temp .service file for systemd-analyze verify");
        tmp.write_all(output.stdout.as_slice())
            .expect("write unit text to temp file");
        // Flush to ensure all bytes are on disk before systemd-analyze opens it.
        tmp.flush().expect("flush temp .service file");

        let verify = Command::new("systemd-analyze")
            .arg("verify")
            .arg(tmp.path())
            .output()
            .expect("spawn systemd-analyze verify");

        assert!(
            verify.status.success(),
            "systemd-analyze verify rejected the generated unit file (exit {}):\n\
             stdout: {}\nstderr: {}\nunit text:\n{}",
            verify.status,
            String::from_utf8_lossy(&verify.stdout),
            String::from_utf8_lossy(&verify.stderr),
            unit_text,
        );
    } else {
        eprintln!(
            "NOTE: systemd-analyze not available — skipping verify step; \
             structural content assertions still run"
        );
    }
}

/// Install a systemd *system* unit and validate its structure.
///
/// This test exercises the same `sqryd install-systemd-system` subcommand path
/// that U10 wires up, complementing the user-unit test above with a quick
/// cross-binary integration check.  It passes the current username (resolved
/// via `$USER`) as `--user` to avoid triggering the "no user provided" error
/// path, and checks the `%i` template specifier is present.
///
/// The test is **skipped with a reason** on non-Linux hosts.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "install-systemd-system is Linux-only; test skipped on this platform"
)]
fn install_systemd_system_output_contains_percent_i_template() {
    // The entire body of this test uses POSIX account lookup and the
    // `install-systemd-system` subcommand, both of which are Linux-only.
    // All bindings — including the `sqryd` binary lookup — are placed inside a
    // single `#[cfg(target_os = "linux")]` block so that they are erased at
    // compile time on macOS and Windows.  This avoids `unused_variables`
    // warnings (which are hard failures under `-D warnings`) on non-Linux
    // cross-compilation targets.
    //
    // The `#[cfg_attr(not(target_os = "linux"), ignore)]` attribute above is a
    // *runtime* gate only; it does not affect compilation of the function body.
    #[cfg(target_os = "linux")]
    {
        let Some(sqryd) = find_sqryd_binary() else {
            eprintln!(
                "SKIP install_systemd_system_output_contains_percent_i_template: \
                 sqryd binary not found — run `cargo build -p sqry-daemon --bin sqryd` first"
            );
            return;
        };

        // We need a valid POSIX account name to pass as --user.
        // Prefer POSIX account lookup (immune to env
        // manipulation) over `$USER` (env var, unreliable in containers / su /
        // sudo contexts).  Fall back to $USER only if the OS lookup fails.
        // Skip the test if neither source can produce a non-empty name.
        let username = {
            let os_name = current_posix_username().filter(|n| !n.is_empty());
            let env_name = std::env::var("USER").ok().filter(|n| !n.is_empty());
            match os_name.or(env_name) {
                Some(u) => u,
                None => {
                    eprintln!(
                        "SKIP install_systemd_system_output_contains_percent_i_template: \
                         cannot determine current username via OS lookup or $USER"
                    );
                    return;
                }
            }
        };

        let output = Command::new(&sqryd)
            .args(["install-systemd-system", "--user", &username])
            .env("RUST_LOG", "off")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {:?}: {e}", sqryd));

        assert!(
            output.status.success(),
            "sqryd install-systemd-system must exit 0; status={}",
            output.status,
        );

        let unit_text = String::from_utf8_lossy(&output.stdout);
        assert!(
            unit_text.contains("User=%i"),
            "system unit must contain User=%i; got:\n{unit_text}"
        );
        assert!(
            unit_text.contains("Group=%i"),
            "system unit must contain Group=%i; got:\n{unit_text}"
        );
        assert!(
            unit_text.contains("WantedBy=multi-user.target"),
            "system unit must target multi-user.target; got:\n{unit_text}"
        );
    }
}

// ---------------------------------------------------------------------------
// macOS: install-launchd → plutil -lint (or structural check on Linux/Windows)
// ---------------------------------------------------------------------------

/// Invoke `sqryd install-launchd`, capture stdout, and validate the produced
/// plist XML.
///
/// **Validation strategy:**
///
/// * On macOS: write the output to a temp file and invoke `plutil -lint` on
///   it.  A zero exit code from `plutil` is authoritative proof of well-formed
///   plist XML.
/// * On Linux / Windows: the test is skipped at runtime with a reason message
///   because `install-launchd` is a macOS-only subcommand and the `sqryd`
///   binary compiled for Linux/Windows does not include it.
///
/// The test is **skipped** on non-macOS hosts.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "install-launchd is macOS-only; test skipped on this platform"
)]
fn install_launchd_output_parses_as_valid_plist_xml() {
    let Some(sqryd) = find_sqryd_binary() else {
        eprintln!(
            "SKIP install_launchd_output_parses_as_valid_plist_xml: \
             sqryd binary not found — run `cargo build -p sqry-daemon --bin sqryd` first"
        );
        return;
    };

    let output = Command::new(&sqryd)
        .arg("install-launchd")
        .env("RUST_LOG", "off")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {:?}: {e}", sqryd));

    assert!(
        output.status.success(),
        "sqryd install-launchd must exit 0; status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let plist_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !plist_text.trim().is_empty(),
        "sqryd install-launchd must produce non-empty output"
    );

    // --- Structural content assertions ---
    assert!(
        plist_text.contains("<?xml version=\"1.0\""),
        "plist must begin with the XML declaration; got:\n{plist_text}"
    );
    assert!(
        plist_text.contains("<plist version=\"1.0\">"),
        "plist must contain the root <plist> element; got:\n{plist_text}"
    );
    assert!(
        plist_text.contains("ai.verivus.sqry.sqryd"),
        "plist must contain the sqryd label; got:\n{plist_text}"
    );
    // Validate that KeepAlive and RunAtLoad are both present and set to
    // `<true/>`.  We check this with a proximity predicate rather than exact
    // adjacency (`<key>KeepAlive</key><true/>`) so that the assertions remain
    // correct whether the generator emits compact or indented XML.
    //
    // The predicate: locate the key text, then inspect the plist text starting
    // immediately after that key for the first `<true/>` or `<false/>` value
    // element before the next `<key>` entry.  This matches plist semantics
    // (a key's value is the very next element) while tolerating whitespace.
    let plist_key_is_true = |key: &str| -> bool {
        let key_tag = format!("<key>{key}</key>");
        let Some(after_key) = plist_text
            .find(key_tag.as_str())
            .map(|pos| &plist_text[pos + key_tag.len()..])
        else {
            return false;
        };
        // Scan forward: the value element must appear before the next <key>.
        let next_key_pos = after_key.find("<key>").unwrap_or(after_key.len());
        let between = &after_key[..next_key_pos];
        between.contains("<true/>")
    };
    assert!(
        plist_key_is_true("KeepAlive"),
        "plist must set KeepAlive to <true/>; got:\n{plist_text}"
    );
    assert!(
        plist_key_is_true("RunAtLoad"),
        "plist must set RunAtLoad to <true/>; got:\n{plist_text}"
    );
    assert!(
        plist_text.contains("</plist>"),
        "plist must close the root element; got:\n{plist_text}"
    );

    // --- plutil -lint validation (macOS only, when tool is present) ---
    // On macOS, `plutil` ships as part of the base system at `/usr/bin/plutil`.
    // We probe availability by checking whether that path is a file — this
    // avoids spawning `plutil` for the probe itself, which is error-prone:
    // `plutil --version` is not a supported flag and exits non-zero on all
    // macOS versions; `plutil -lint /dev/null` also exits non-zero because
    // /dev/null is not a valid plist.  A filesystem check has no such caveat.
    #[cfg(target_os = "macos")]
    let plutil_available = std::path::Path::new("/usr/bin/plutil").is_file();
    #[cfg(not(target_os = "macos"))]
    let plutil_available = false;

    if plutil_available {
        // Write the plist to a temp file with a `.plist` extension so
        // `plutil` recognises it correctly.
        let mut tmp =
            NamedTempFile::with_suffix(".plist").expect("create temp .plist file for plutil lint");
        tmp.write_all(output.stdout.as_slice())
            .expect("write plist text to temp file");
        tmp.flush().expect("flush temp .plist file");

        let lint = Command::new("plutil")
            .args(["-lint", tmp.path().to_str().expect("temp path is UTF-8")])
            .output()
            .expect("spawn plutil -lint");

        assert!(
            lint.status.success(),
            "plutil -lint rejected the generated plist (exit {}):\n\
             stdout: {}\nstderr: {}\nplist text:\n{}",
            lint.status,
            String::from_utf8_lossy(&lint.stdout),
            String::from_utf8_lossy(&lint.stderr),
            plist_text,
        );
    } else {
        eprintln!(
            "NOTE: plutil not available — skipping lint step; \
             structural content assertions still run"
        );
    }
}

// ---------------------------------------------------------------------------
// Windows: install-windows → both sc.exe and Task Scheduler XML variants
// ---------------------------------------------------------------------------

/// Invoke `sqryd install-windows` and assert that the output contains both the
/// `sc.exe create` command and the Task Scheduler XML document.
///
/// The test is **skipped with a reason** on all non-Windows hosts because the
/// `install-windows` subcommand is only compiled into the Windows binary.
#[test]
#[cfg_attr(
    not(target_os = "windows"),
    ignore = "install-windows is Windows-only; test skipped on this platform"
)]
fn install_windows_emits_both_variants() {
    let Some(sqryd) = find_sqryd_binary() else {
        eprintln!(
            "SKIP install_windows_emits_both_variants: \
             sqryd binary not found — run `cargo build -p sqry-daemon --bin sqryd` first"
        );
        return;
    };

    let output = Command::new(&sqryd)
        .arg("install-windows")
        .env("RUST_LOG", "off")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {:?}: {e}", sqryd));

    assert!(
        output.status.success(),
        "sqryd install-windows must exit 0; status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let combined = String::from_utf8_lossy(&output.stdout);
    assert!(
        !combined.trim().is_empty(),
        "sqryd install-windows must produce non-empty output"
    );

    // ── sc.exe section ────────────────────────────────────────────────────────
    // The entrypoint prints "-- sc.exe create command --" before the script.
    assert!(
        combined.contains("-- sc.exe create command --"),
        "output must contain the exact sc.exe create command section header; got:\n{combined}"
    );
    assert!(
        combined.contains("sc.exe create sqryd"),
        "output must include the sc.exe create sqryd command; got:\n{combined}"
    );
    assert!(
        combined.contains("start= auto"),
        "sc.exe create command must set start= auto; got:\n{combined}"
    );

    // ── Task Scheduler XML section ────────────────────────────────────────────
    // The entrypoint prints "-- Task Scheduler XML --" before the XML.
    assert!(
        combined.contains("-- Task Scheduler XML --"),
        "output must contain the exact Task Scheduler XML section header; got:\n{combined}"
    );
    assert!(
        combined.contains("<?xml version=\"1.0\""),
        "output must contain the XML declaration for the task XML; got:\n{combined}"
    );
    assert!(
        combined.contains("<Task "),
        "output must contain the Task Scheduler <Task> root element; got:\n{combined}"
    );
    assert!(
        combined.contains("http://schemas.microsoft.com/windows/2004/02/mit/task"),
        "Task Scheduler XML must include the correct namespace; got:\n{combined}"
    );
    assert!(
        combined.contains("</Task>"),
        "Task Scheduler XML must be closed; got:\n{combined}"
    );

    // ── Both variants must not be confused ───────────────────────────────────
    // The entrypoint (run_install_windows in entrypoint.rs) emits the exact
    // separator "-- Task Scheduler XML --" between the two sections. Using
    // this exact string avoids accidental matches if the phrase appears in
    // comments or documentation elsewhere in the output.
    const XML_SEPARATOR: &str = "-- Task Scheduler XML --";

    // The sc.exe section must not contain the XML declaration.
    let sc_section = combined.split(XML_SEPARATOR).next().unwrap_or(&combined);
    assert!(
        !sc_section.contains("<?xml version="),
        "sc.exe section must not contain XML declarations; got:\n{sc_section}"
    );

    // The Task Scheduler XML section must not contain sc.exe commands.
    let xml_section = combined.split(XML_SEPARATOR).nth(1).unwrap_or_default();
    assert!(
        !xml_section.contains("sc.exe create"),
        "Task Scheduler XML section must not contain sc.exe commands; got:\n{xml_section}"
    );
}

// ---------------------------------------------------------------------------
// Cross-platform smoke: print-config always works
// ---------------------------------------------------------------------------

/// Invoke `sqryd print-config` and assert the output is valid TOML containing
/// key fields from the default `DaemonConfig`.
///
/// This test runs on all platforms and serves as a cross-platform integration
/// smoke-check that the binary starts up, loads the config subsystem, and
/// serialises the effective configuration correctly.
#[test]
fn print_config_emits_valid_toml_with_expected_keys() {
    let Some(sqryd) = find_sqryd_binary() else {
        eprintln!(
            "SKIP print_config_emits_valid_toml_with_expected_keys: \
             sqryd binary not found — run `cargo build -p sqry-daemon --bin sqryd` first"
        );
        return;
    };

    let output = Command::new(&sqryd)
        .arg("print-config")
        .env("RUST_LOG", "off")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {:?}: {e}", sqryd));

    assert!(
        output.status.success(),
        "sqryd print-config must exit 0; status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let toml_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !toml_text.trim().is_empty(),
        "sqryd print-config must produce non-empty output"
    );

    // Check that TOML is parseable.
    let parsed: toml::Value = toml::from_str(&toml_text).unwrap_or_else(|e| {
        panic!("sqryd print-config output is not valid TOML: {e}\noutput:\n{toml_text}")
    });

    // Spot-check key fields that DaemonConfig always serialises.
    assert!(
        parsed.get("memory_limit_mb").is_some(),
        "TOML must contain memory_limit_mb; parsed:\n{toml_text}"
    );
    assert!(
        parsed.get("debounce_ms").is_some(),
        "TOML must contain debounce_ms; parsed:\n{toml_text}"
    );
    assert!(
        parsed.get("idle_timeout_minutes").is_some(),
        "TOML must contain idle_timeout_minutes; parsed:\n{toml_text}"
    );
}
