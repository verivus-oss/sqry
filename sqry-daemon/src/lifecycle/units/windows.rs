//! Windows service-unit generator — Task 9 §F.4.
//!
//! Emits two complementary installation primitives for `sqryd` on Windows:
//!
//! 1. **`sc.exe create` command string** ([`generate_sc_create`]) — registers
//!    `sqryd` as a native Windows Service via the Service Control Manager
//!    (SCM). Uses startup type `auto`, account `LocalSystem` (overridable via
//!    [`InstallOptions::user`]), display name, description, and recovery
//!    options (restart on first/second failure, reset error count after one
//!    day).
//!
//! 2. **Task Scheduler XML** ([`generate_task_xml`]) — a per-user scheduled
//!    task that triggers on logon and runs `sqryd foreground`. Preferred for
//!    per-developer installations where the user does not have administrator
//!    rights to register an SCM service. When no `opts.user` is provided the
//!    logon trigger covers any user; supply `opts.user` for a specific account.
//!
//! Both generators are pure `String` functions: they resolve
//! `std::env::current_exe()` for the binary path and stamp the current crate
//! version in a comment header. No OS API calls are made beyond `current_exe()`.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §F.4.
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter0_request.md` §F.4
//! (original verbatim spec).
//!
//! # Non-goals
//!
//! This module does **not** call `sc.exe` or `schtasks.exe` directly. It only
//! generates the textual installer artifact. The `install-windows` subcommand
//! (U10) prints both outputs and instructs the operator to run them. Native
//! SCM integration via the `windows-service` crate is explicitly deferred
//! (design §B.2 non-goal #6).

#![cfg(target_os = "windows")]

use std::path::PathBuf;

use crate::config::DaemonConfig;
use crate::lifecycle::units::InstallOptions;

// ---------------------------------------------------------------------------
// Version stamp.
// ---------------------------------------------------------------------------

/// Crate version embedded in the generated output as a comment header.
const SQRYD_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Binary path helpers.
// ---------------------------------------------------------------------------

/// Resolve the absolute path to the running `sqryd` binary.
///
/// When `opts.exe_path` is set it is returned directly (useful in tests for
/// deterministic snapshot assertions without calling `current_exe()`).
///
/// Otherwise calls [`std::env::current_exe`] and canonicalises the result to
/// remove any `\\?\` UNC prefix that Windows sometimes returns. Falls back to
/// `"sqryd.exe"` (bare name) if `current_exe()` fails — the caller should
/// warn the operator to substitute the real path.
///
/// Returns `(path, resolved)` where `resolved = false` triggers a warning
/// comment in the generated output.
fn resolve_exe_for_opts(opts: &InstallOptions) -> (PathBuf, bool) {
    if let Some(ref p) = opts.exe_path {
        return (p.clone(), true);
    }
    match std::env::current_exe() {
        Ok(p) => {
            // Canonicalise to drop the `\\?\` extended-path prefix that
            // `current_exe` may return on some Windows configurations.
            let canonical = p.canonicalize().unwrap_or(p);
            (canonical, true)
        }
        Err(_) => (PathBuf::from("sqryd.exe"), false),
    }
}

// ---------------------------------------------------------------------------
// Public generators.
// ---------------------------------------------------------------------------

/// Generate an `sc.exe` command string that registers `sqryd` as a native
/// Windows Service.
///
/// # Output format
///
/// Returns a multi-line shell script (PowerShell / cmd-compatible) that:
///
/// 1. Creates the service with `sc.exe create`.
/// 2. Sets the display name and description with `sc.exe config` /
///    `sc.exe description`.
/// 3. Configures failure-recovery actions (restart on first/second failure,
///    no action on subsequent failures, error count reset after 86 400 s).
///
/// The operator runs this script once in an elevated session. Subsequent
/// daemon lifecycle is handled by the SCM.
///
/// # Parameters
///
/// - `cfg` — `DaemonConfig` for `memory_limit_mb` (embedded in description).
/// - `opts.user` — account name for `obj=`. Defaults to `"LocalSystem"`.
///
/// # Notes
///
/// - Native SCM integration (the `windows-service` crate) is deferred per
///   design §B.2 non-goal #6. The generated command spawns `sqryd foreground`
///   directly; the SCM treats it as a non-interactive console application.
/// - `type= own` ensures `sqryd` runs in its own process, not a shared host.
/// - The `sc.exe config obj=` line uses the `LocalSystem` account by default
///   because it has write access to `%LOCALAPPDATA%` (the runtime dir) without
///   requiring per-deployment credential management.
#[must_use]
pub fn generate_sc_create(cfg: &DaemonConfig, opts: &InstallOptions) -> String {
    let (exe, resolved) = resolve_exe_for_opts(opts);
    let exe_str = exe.to_string_lossy();
    let account = opts.user.as_deref().unwrap_or("LocalSystem");
    let memory_mb = cfg.memory_limit_mb;

    let mut out = String::with_capacity(2048);

    // Version comment header.
    out.push_str(&format!(
        ":: sqryd version {SQRYD_VERSION}\n\
         :: Generated by: sqryd install-windows\n\
         :: Run this script in an elevated (Administrator) PowerShell or cmd session.\n"
    ));
    if !resolved {
        out.push_str(
            ":: WARNING: could not resolve sqryd.exe path — \
             substitute the correct absolute path below.\n",
        );
    }
    out.push('\n');

    // 1. sc.exe create — register the service.
    //
    // `binPath` must include all arguments because SCM passes the full
    // string to CreateProcess. The quoted `exe_str foreground` form is
    // mandatory so that paths with spaces survive SCM's argument splitting.
    //
    // `obj=` must be quoted in case the account name contains spaces (e.g.
    // `"NT AUTHORITY\NetworkService"`). Built-in virtual accounts like
    // `NT AUTHORITY\LocalService` and `NT AUTHORITY\NetworkService` are
    // supported without a `password=` argument; domain or local user accounts
    // require `password= <plaintext>` which must be supplied manually after
    // generating this script.
    out.push_str(&format!(
        "sc.exe create sqryd \\\n\
         \tbinPath= \"\\\"{exe_str}\\\" foreground\" \\\n\
         \tstart= auto \\\n\
         \ttype= own \\\n\
         \tobj= \"{account}\"\n\n"
    ));

    // Emit an operator-visible NOTE when the account is not one of the three
    // Windows built-in virtual accounts (LocalSystem, NT AUTHORITY\LocalService,
    // NT AUTHORITY\NetworkService).  Those three are password-free; every other
    // account — domain users, local users, managed-service accounts — requires
    // `password=` on the `sc.exe create` line, which the generator cannot supply
    // because the plaintext credential must be provided at deployment time.
    let builtin = matches!(
        account,
        "LocalSystem" | "NT AUTHORITY\\LocalService" | "NT AUTHORITY\\NetworkService"
    );
    if !builtin {
        out.push_str(&format!(
            ":: NOTE: \"{account}\" is not a built-in virtual account.\n\
             ::   Domain and local user accounts require a password argument on sc.exe create.\n\
             ::   Re-run the create command and append:  password= <your-plaintext-password>\n\n"
        ));
    }

    // 2. sc.exe config — display name.
    out.push_str(
        "sc.exe config sqryd \\\n\
         \tdisplayname= \"sqry Daemon (sqryd)\"\n\n",
    );

    // 3. sc.exe description — human-readable description embeds memory budget.
    out.push_str(&format!(
        "sc.exe description sqryd \\\n\
         \t\"sqry semantic code-search daemon — {memory_mb} MiB memory budget. \
         See https://github.com/verivus-oss/sqry for documentation.\"\n\n"
    ));

    // 4. sc.exe failure — recovery options.
    //
    // Actions: restart on first failure (60 000 ms delay), restart on second
    // failure (120 000 ms delay), do nothing on subsequent failures.
    // `reset=` 86 400 sets the error-count reset period to 1 day.
    out.push_str(
        "sc.exe failure sqryd \\\n\
         \treset= 86400 \\\n\
         \tactions= restart/60000/restart/120000/\"\"/0\n\n",
    );

    // 5. Verification hint.
    out.push_str(
        ":: Verify registration:\n\
         ::   sc.exe qc sqryd\n\
         :: Start the service:\n\
         ::   sc.exe start sqryd\n\
         :: Enable auto-start at boot (if not already):\n\
         ::   sc.exe config sqryd start= auto\n",
    );

    out
}

/// Generate a Task Scheduler XML descriptor for a per-user `sqryd` logon
/// trigger.
///
/// # Rationale
///
/// Task Scheduler is the preferred per-user installation mechanism on Windows
/// because it does not require administrator privileges (user can register
/// tasks in the personal task folder), runs at each logon, and can be managed
/// via `schtasks.exe /Import` or the Task Scheduler GUI.
///
/// # Output format
///
/// Returns a well-formed UTF-8 XML string conforming to the Task Scheduler 2.0
/// schema (`http://schemas.microsoft.com/windows/2004/02/mit/task`). The task:
///
/// - Triggers on logon by `opts.user` (when provided) or any user (when
///   `None`; `<UserId>` is omitted from `<LogonTrigger>`, which is the
///   documented Task Scheduler schema behaviour for "any user").
/// - Runs `sqryd.exe foreground`.
/// - Uses `RunLevel::HighestAvailable` so the daemon can bind its named pipe
///   without requiring full elevation.
/// - Sets `MultipleInstancesPolicy=IgnoreNew` — if a `sqryd` is already
///   running, the duplicate trigger is silently dropped.
/// - Enables the task and configures `ExecutionTimeLimit` of `PT0S`
///   (unlimited — daemon is long-lived by design).
/// - Sets `DisallowStartIfOnBatteries=false` and `StopIfGoingOnBatteries=false`
///   so the daemon keeps running on laptop battery transitions.
///
/// # Encoding
///
/// The returned `String` is UTF-8 encoded. The XML declaration specifies
/// `encoding="UTF-8"`. If you write the output to disk as a `.xml` file you
/// do **not** need to re-encode it; `schtasks.exe /Import` accepts UTF-8.
///
/// # XML escaping
///
/// `exe_str` is passed through [`xml_escape`] so that paths containing
/// `&`, `<`, `>`, `"`, or `'` produce valid XML. `user_id` (when provided
/// by the caller) is similarly escaped.
///
/// # Import
///
/// ```text
/// schtasks.exe /Create /TN "\sqry\sqryd" /XML sqryd-task.xml /F
/// ```
///
/// # Parameters
///
/// - `opts.user` — `<UserId>` in both `<LogonTrigger>` and `<Principal>`.
///   When `None`, the trigger fires for any user logon and the principal
///   element omits `<UserId>`, relying on the `InteractiveToken` logon type to
///   bind to the importing user at registration time.
#[must_use]
pub fn generate_task_xml(cfg: &DaemonConfig, opts: &InstallOptions) -> String {
    let (exe, resolved) = resolve_exe_for_opts(opts);
    // Escape the exe path for embedding in XML attribute/text context.
    let exe_str_raw = exe.to_string_lossy();
    let exe_str = xml_escape(&exe_str_raw);
    let memory_mb = cfg.memory_limit_mb;

    // When no explicit user is supplied, omit <UserId> from both the trigger
    // and the principal.  The Task Scheduler schema documents <UserId> as
    // optional in <LogonTrigger>; omitting it means "any user".  Populating
    // it with an environment-variable placeholder (%USERNAME%) is NOT
    // documented as valid and may be rejected by schtasks.exe on import.
    let trigger_user_id_elem = opts
        .user
        .as_deref()
        .map(|u| format!("      <UserId>{}</UserId>\n", xml_escape(u)))
        .unwrap_or_default();
    let principal_user_id_elem = opts
        .user
        .as_deref()
        .map(|u| format!("      <UserId>{}</UserId>\n", xml_escape(u)))
        .unwrap_or_default();

    let warning_comment = if resolved {
        String::new()
    } else {
        "    <!-- WARNING: could not resolve sqryd.exe path — substitute the correct absolute path in <Command> below. -->\n".to_string()
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- sqryd version {SQRYD_VERSION} -->
<!-- Generated by: sqryd install-windows -->
<!-- Import with: schtasks.exe /Create /TN "\sqry\sqryd" /XML sqryd-task.xml /F -->
<!-- Memory budget: {memory_mb} MiB (from DaemonConfig.memory_limit_mb) -->
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>sqry semantic code-search daemon — persistent graph service ({memory_mb} MiB budget). See https://github.com/verivus-oss/sqry</Description>
    <Author>sqry / verivus-oss</Author>
    <Version>{SQRYD_VERSION}</Version>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
{trigger_user_id_elem}    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
{principal_user_id_elem}      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
{warning_comment}    <Exec>
      <Command>{exe_str}</Command>
      <Arguments>foreground</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

// ---------------------------------------------------------------------------
// XML helpers.
// ---------------------------------------------------------------------------

/// Escape the five predefined XML entities in `s`.
///
/// Only characters that are illegal in XML text content and attribute values
/// are escaped: `&`, `<`, `>`, `"`, and `'`. This is sufficient for embedding
/// arbitrary Windows paths and usernames into an XML element or attribute.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
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

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;
    use crate::lifecycle::units::InstallOptions;

    /// Helper: build a deterministic `DaemonConfig` fixture for snapshot-style
    /// assertions. Uses the public `Default` impl so that any new field with a
    /// sensible default does not silently break these tests.
    fn fixture_cfg() -> DaemonConfig {
        DaemonConfig::default()
    }

    // -----------------------------------------------------------------------
    // generate_sc_create tests.
    // -----------------------------------------------------------------------

    #[test]
    fn windows_install_sc_create_contains_binpath_and_start_auto() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_sc_create(&cfg, &opts);

        // Must reference the service name.
        assert!(
            output.contains("sc.exe create sqryd"),
            "expected 'sc.exe create sqryd' in output:\n{output}"
        );
        // Must configure auto-start.
        assert!(
            output.contains("start= auto"),
            "expected 'start= auto' in output:\n{output}"
        );
        // Must set type= own (isolated process, not svchost).
        assert!(
            output.contains("type= own"),
            "expected 'type= own' in output:\n{output}"
        );
        // Default account — must be quoted to handle accounts with spaces.
        assert!(
            output.contains("obj= \"LocalSystem\""),
            "expected 'obj= \"LocalSystem\"' in output:\n{output}"
        );
    }

    #[test]
    fn windows_install_sc_create_contains_display_name_and_description() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_sc_create(&cfg, &opts);

        assert!(
            output.contains("sc.exe config sqryd"),
            "expected 'sc.exe config sqryd' in output:\n{output}"
        );
        assert!(
            output.contains("displayname=") || output.contains("displayname= "),
            "expected displayname in output:\n{output}"
        );
        assert!(
            output.contains("sc.exe description sqryd"),
            "expected 'sc.exe description sqryd' in output:\n{output}"
        );
        // Description should mention memory budget.
        let memory_mb = cfg.memory_limit_mb.to_string();
        assert!(
            output.contains(&memory_mb),
            "expected memory budget '{memory_mb}' in description:\n{output}"
        );
    }

    #[test]
    fn windows_install_sc_create_contains_recovery_options() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_sc_create(&cfg, &opts);

        assert!(
            output.contains("sc.exe failure sqryd"),
            "expected 'sc.exe failure sqryd' in output:\n{output}"
        );
        // reset= 86400 — 1 day error-count reset.
        assert!(
            output.contains("reset= 86400"),
            "expected 'reset= 86400' in output:\n{output}"
        );
        // First restart delay: 60 000 ms.
        assert!(
            output.contains("restart/60000"),
            "expected 'restart/60000' in output:\n{output}"
        );
        // Second restart delay: 120 000 ms.
        assert!(
            output.contains("restart/120000"),
            "expected 'restart/120000' in output:\n{output}"
        );
    }

    #[test]
    fn windows_install_sc_create_uses_custom_account_when_provided() {
        let cfg = fixture_cfg();
        let opts = InstallOptions {
            user: Some("NT AUTHORITY\\NetworkService".to_string()),
            exe_path: None,
        };
        let output = generate_sc_create(&cfg, &opts);

        // Account must appear in the obj= clause, quoted so that embedded
        // spaces survive shell tokenisation.
        assert!(
            output.contains("\"NT AUTHORITY\\NetworkService\""),
            "expected quoted custom account in output:\n{output}"
        );
        // Must NOT contain the default LocalSystem when custom is set.
        assert!(
            !output.contains("\"LocalSystem\""),
            "default LocalSystem must not appear when custom account is set:\n{output}"
        );
    }

    #[test]
    fn windows_install_sc_create_contains_version_stamp() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_sc_create(&cfg, &opts);

        assert!(
            output.contains(SQRYD_VERSION),
            "expected version stamp '{SQRYD_VERSION}' in output:\n{output}"
        );
    }

    // -----------------------------------------------------------------------
    // generate_task_xml tests.
    // -----------------------------------------------------------------------

    #[test]
    fn windows_install_task_xml_is_valid_xml() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        // Must be valid XML — check for the declaration and root element.
        assert!(
            output.starts_with(r#"<?xml version="1.0""#),
            "expected XML declaration at start:\n{output}"
        );
        assert!(
            output.contains("<Task "),
            "expected <Task ...> root element:\n{output}"
        );
        assert!(
            output.contains("</Task>"),
            "expected </Task> closing tag:\n{output}"
        );
        // Namespace.
        assert!(
            output.contains("http://schemas.microsoft.com/windows/2004/02/mit/task"),
            "expected Task Scheduler 2.0 namespace:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_contains_logon_trigger() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains("<LogonTrigger>"),
            "expected <LogonTrigger> in output:\n{output}"
        );
        assert!(
            output.contains("</LogonTrigger>"),
            "expected </LogonTrigger> in output:\n{output}"
        );
        assert!(
            output.contains("<Enabled>true</Enabled>"),
            "expected enabled trigger:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_omits_user_id_when_none() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        // When no user is specified, <UserId> must be absent from both the
        // trigger and the principal — the Task Scheduler schema does not
        // document environment-variable expansion in UserId, so emitting
        // %USERNAME% would produce a non-conforming document.
        assert!(
            !output.contains("<UserId>"),
            "expected no <UserId> element when opts.user is None:\n{output}"
        );
        // The trigger must still be present and enabled.
        assert!(
            output.contains("<LogonTrigger>"),
            "expected <LogonTrigger> present:\n{output}"
        );
        assert!(
            output.contains("<Enabled>true</Enabled>"),
            "expected trigger enabled:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_uses_custom_user_when_provided() {
        let cfg = fixture_cfg();
        let opts = InstallOptions {
            user: Some("DOMAIN\\alice".to_string()),
            exe_path: None,
        };
        let output = generate_task_xml(&cfg, &opts);

        // UserId must appear in both the trigger and the principal when
        // an explicit user is provided.
        assert!(
            output.contains("<UserId>DOMAIN\\alice</UserId>"),
            "expected custom UserId in output:\n{output}"
        );
        // No %USERNAME% placeholder — that is not a valid Task Scheduler value.
        assert!(
            !output.contains("%USERNAME%"),
            "%USERNAME% must never appear in generated XML:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_contains_exec_action_with_foreground() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains("<Exec>"),
            "expected <Exec> action:\n{output}"
        );
        assert!(
            output.contains("<Arguments>foreground</Arguments>"),
            "expected <Arguments>foreground</Arguments>:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_contains_unlimited_execution_time_limit() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        // PT0S = unlimited execution time (daemon is long-lived).
        assert!(
            output.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "expected unlimited ExecutionTimeLimit PT0S:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_ignores_battery_state() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"),
            "expected DisallowStartIfOnBatteries=false:\n{output}"
        );
        assert!(
            output.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"),
            "expected StopIfGoingOnBatteries=false:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_sets_ignore_new_multiple_instances_policy() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"),
            "expected IgnoreNew MultipleInstancesPolicy:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_contains_version_stamp() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains(SQRYD_VERSION),
            "expected version stamp '{SQRYD_VERSION}' in output:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_contains_run_level_highest_available() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        assert!(
            output.contains("<RunLevel>HighestAvailable</RunLevel>"),
            "expected RunLevel=HighestAvailable:\n{output}"
        );
    }

    // -----------------------------------------------------------------------
    // Combined: "emits both variants" (U8 acceptance criterion).
    // -----------------------------------------------------------------------

    #[test]
    fn windows_install_emits_sc_create_and_scheduler_xml() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();

        let sc = generate_sc_create(&cfg, &opts);
        let xml = generate_task_xml(&cfg, &opts);

        // sc.exe form must not look like XML.
        assert!(
            !sc.trim_start().starts_with("<?xml"),
            "sc_create output must not be XML:\n{sc}"
        );
        // XML form must not look like a shell command.
        assert!(
            !xml.contains("sc.exe"),
            "task_xml output must not contain sc.exe commands:\n{xml}"
        );
        // Both must carry the version stamp.
        assert!(
            sc.contains(SQRYD_VERSION),
            "sc_create missing version:\n{sc}"
        );
        assert!(
            xml.contains(SQRYD_VERSION),
            "task_xml missing version:\n{xml}"
        );
    }

    // -----------------------------------------------------------------------
    // XML escaping / encoding correctness.
    // -----------------------------------------------------------------------

    #[test]
    fn windows_install_task_xml_is_utf8_not_utf16() {
        let cfg = fixture_cfg();
        let opts = InstallOptions::default();
        let output = generate_task_xml(&cfg, &opts);

        // The XML declaration must claim UTF-8 (matching the Rust String
        // encoding).  Claiming UTF-16 while returning a UTF-8 String would
        // produce a non-conforming document.
        assert!(
            output.contains(r#"encoding="UTF-8""#),
            "expected UTF-8 encoding declaration:\n{output}"
        );
        assert!(
            !output.contains("UTF-16"),
            "must not claim UTF-16 encoding in a UTF-8 String:\n{output}"
        );
    }

    #[test]
    fn xml_escape_handles_special_chars() {
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(xml_escape(r#"a"b'c"#), "a&quot;b&apos;c");
        assert_eq!(xml_escape("normal"), "normal");
        assert_eq!(
            xml_escape("C:\\Program Files\\sqryd.exe"),
            "C:\\Program Files\\sqryd.exe"
        );
    }

    #[test]
    fn windows_install_task_xml_escapes_special_chars_in_exe_path() {
        let cfg = fixture_cfg();
        let opts = InstallOptions {
            user: None,
            exe_path: Some(std::path::PathBuf::from("C:\\Prog&s\\sqry<d>.exe")),
        };
        let output = generate_task_xml(&cfg, &opts);

        // The raw path must NOT appear unescaped in the output.
        assert!(
            !output.contains("Prog&s"),
            "unescaped '&' must not appear in XML output:\n{output}"
        );
        // Escaped form must be present.
        assert!(
            output.contains("Prog&amp;s"),
            "expected '&amp;' escaped form in XML output:\n{output}"
        );
    }

    #[test]
    fn windows_install_task_xml_escapes_special_chars_in_user_id() {
        let cfg = fixture_cfg();
        let opts = InstallOptions {
            user: Some("DOM&AIN\\ali<ce>".to_string()),
            exe_path: None,
        };
        let output = generate_task_xml(&cfg, &opts);

        // The raw user string must not appear verbatim.
        assert!(
            !output.contains("DOM&AIN"),
            "unescaped '&' in user must not appear in XML:\n{output}"
        );
        assert!(
            output.contains("DOM&amp;AIN"),
            "expected &amp; in escaped UserId:\n{output}"
        );
    }
}
