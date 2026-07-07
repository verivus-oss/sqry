//! `sqry doctor`: read-only installation diagnostics.
//!
//! Currently exposes a single subcommand, `sqry doctor channels`, which
//! diagnoses stable vs dev sqry channel separation (issue #308). The command
//! never mutates config, index, or daemon state: it resolves both channels'
//! binaries, cross-checks MCP config keys (Claude's global *and*
//! project-scoped entries, since `sqry mcp setup --scope auto` writes to
//! project scope whenever a workspace root is present) and daemon socket
//! paths, compares
//! the two channels' plugin rosters (via subprocess probes, so each channel's
//! roster is judged by its *own* process), and reports any mixed-channel
//! condition. It exits non-zero when a genuine mismatch is detected so it can
//! gate scripts and CI.
//!
//! ## Channel model (issue #308)
//!
//! - Stable owns the bare names `sqry`/`sqry-mcp`/`sqry-lsp`/`sqryd`, the
//!   default `$XDG_RUNTIME_DIR/sqry/` runtime dir, and the MCP server key
//!   `sqry`.
//! - Dev owns the `-d` wrappers (`sqry-d`, `sqry-mcp-d`, ...), a per-repo
//!   runtime dir under `$XDG_RUNTIME_DIR/sqry-dev/<repo_id>/`, and the MCP
//!   server key `sqry_dev`. The dev binaries and the `sqry_dev` key are single
//!   global dev-channel entities (the latest dev build wins); only the runtime
//!   state (socket/pid/lock) is scoped per repo.
//!
//! Full policy: `docs/development/tooling/sqry-channel-separation.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use serde::Serialize;

use crate::args::DoctorCommand;

/// Marker-comment prefixes emitted by `scripts/install-dev.sh` into each dev
/// wrapper so the doctor can read the channel's baked configuration without
/// parsing shell. Keep these in lockstep with `install-dev.sh`.
const MARKER_CHANNEL: &str = "# sqry-dev-channel:";
const MARKER_REPO_ID: &str = "# sqry-dev-repo-id:";
const MARKER_SOCKET: &str = "# sqry-dev-socket:";
const MARKER_SQRYD_PATH: &str = "# sqry-dev-sqryd-path:";
const MARKER_EXEC: &str = "# sqry-dev-exec:";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatch `sqry doctor` subcommands.
///
/// # Errors
///
/// Returns an error if a diagnostic probe fails to run. A detected
/// mixed-channel *mismatch* is not an error return: it is reported and the
/// process exits non-zero via [`std::process::exit`].
pub fn run(command: &DoctorCommand) -> Result<()> {
    match command {
        DoctorCommand::Channels { json } => run_channels(*json),
    }
}

// ---------------------------------------------------------------------------
// Finding model
// ---------------------------------------------------------------------------

/// Severity of a single diagnostic finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    /// Informational, everything as expected.
    Ok,
    /// A condition worth flagging that does not by itself fail the check.
    Warn,
    /// A genuine mixed-channel misconfiguration. Any `Fail` sets the exit
    /// code non-zero.
    Fail,
}

/// A single diagnostic result line.
#[derive(Debug, Clone, Serialize)]
struct Finding {
    severity: Severity,
    /// Stable machine-readable code (for scripting / tests).
    code: &'static str,
    message: String,
}

impl Finding {
    fn ok(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            code,
            message: message.into(),
        }
    }
    fn warn(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            code,
            message: message.into(),
        }
    }
    fn fail(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Structured inputs (pure evaluation seam)
// ---------------------------------------------------------------------------

/// A resolved MCP server `command` value plus a human-readable label for
/// where it was read from (tool and, for Claude, scope), e.g. `"Claude
/// (global)"` or `"Claude (project: /repo)"`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct McpCommandHit {
    /// The `command` string written into the tool's MCP server entry.
    command: String,
    /// Tool (and, for Claude, scope) the entry was read from.
    source: String,
}

/// Everything the evaluator needs, gathered by IO in [`gather_inputs`]. Kept
/// as a plain data struct so [`evaluate`] is a pure function and every
/// mismatch class is unit-testable without real binaries.
#[derive(Debug, Clone, Default)]
struct DoctorInputs {
    /// Resolved path of the stable `sqry-mcp` (None = not found).
    stable_mcp_path: Option<PathBuf>,
    /// Resolved `--version` line of the stable `sqry` CLI, if runnable.
    stable_sqry_version: Option<String>,
    /// Default stable daemon socket path.
    stable_socket: PathBuf,
    /// Whether any dev wrapper (`sqry-mcp-d`) was found.
    dev_installed: bool,
    /// Path of the dev `sqry-mcp-d` wrapper, if found.
    dev_wrapper_path: Option<PathBuf>,
    /// Baked repo id from the dev wrapper marker.
    dev_repo_id: Option<String>,
    /// Baked (install-time resolved) dev socket from the wrapper marker.
    dev_socket: Option<PathBuf>,
    /// Baked `SQRYD_PATH` from the wrapper marker.
    dev_sqryd_path: Option<PathBuf>,
    /// Baked exec target (`...-d.bin`) from the wrapper marker.
    dev_exec_target: Option<PathBuf>,
    /// Whether the dev `SQRYD_PATH` points at an existing file.
    dev_sqryd_exists: bool,
    /// Whether the dev daemon's pid/lock co-locate with its socket (i.e.
    /// whether this daemon build carries #519's socket-scoped lock/pid).
    dev_lock_pid_colocated: bool,
    /// `command` string of the stable `sqry` MCP server key for every
    /// tool/scope combination found: Claude global, Claude project-scoped
    /// (`projects[<workspace_root>].mcpServers`), Gemini global, Codex
    /// global. Every hit is evaluated, not just the first found, so a
    /// mismatch confined to a single scope is never silently skipped.
    mcp_stable_commands: Vec<McpCommandHit>,
    /// `command` string of the dev `sqry_dev` MCP server key for every
    /// tool/scope combination found. Same completeness contract as
    /// [`Self::mcp_stable_commands`].
    mcp_dev_commands: Vec<McpCommandHit>,
    /// Whether the current working directory has a `.sqry/graph`.
    cwd_graph_present: bool,
    /// Plugin-id roster reported by the stable `sqry --list-languages`.
    stable_roster: Option<BTreeSet<String>>,
    /// Plugin-id roster reported by the dev `sqry-d --list-languages`.
    dev_roster: Option<BTreeSet<String>>,
}

/// Does a resolved path live inside a Cargo build tree (`.../target/...`)?
fn path_in_target_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("target"))
}

/// Does a filename look like a dev `-d` binary (`sqry-mcp-d`, `sqryd-d.bin`)?
fn looks_like_dev_binary(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let stem = name.strip_suffix(".bin").unwrap_or(name);
    stem.ends_with("-d")
}

// ---------------------------------------------------------------------------
// Pure evaluation
// ---------------------------------------------------------------------------

/// Evaluate gathered inputs into an ordered list of findings. Pure: no IO.
#[allow(clippy::too_many_lines)]
fn evaluate(inputs: &DoctorInputs) -> Vec<Finding> {
    let mut findings = Vec::new();

    // --- Stable channel binary ---
    match &inputs.stable_mcp_path {
        Some(p) if path_in_target_dir(p) => findings.push(Finding::fail(
            "stable-mcp-in-target",
            format!(
                "stable sqry-mcp resolves inside a build tree: {} (a dev build has \
                 captured the stable name). Reinstall stable to ~/.local/bin and keep \
                 dev builds under the `-d` names via scripts/install-dev.sh.",
                p.display()
            ),
        )),
        Some(p) if looks_like_dev_binary(p) => findings.push(Finding::fail(
            "stable-mcp-is-dev-binary",
            format!(
                "stable sqry-mcp resolves to a dev binary: {} (the stable name points \
                 at a `-d` build).",
                p.display()
            ),
        )),
        Some(p) => findings.push(Finding::ok(
            "stable-mcp",
            format!("stable sqry-mcp: {}", p.display()),
        )),
        None => findings.push(Finding::warn(
            "stable-mcp-missing",
            "stable sqry-mcp not found on PATH or in the usual install dirs",
        )),
    }
    if let Some(v) = &inputs.stable_sqry_version {
        findings.push(Finding::ok("stable-version", format!("stable sqry: {v}")));
    }

    // --- Dev channel binaries ---
    if inputs.dev_installed {
        if let Some(p) = &inputs.dev_wrapper_path {
            findings.push(Finding::ok(
                "dev-wrapper",
                format!(
                    "dev sqry-mcp-d wrapper: {} (repo_id: {})",
                    p.display(),
                    inputs.dev_repo_id.as_deref().unwrap_or("<unknown>")
                ),
            ));
        }

        // Dev daemon selection must point at a distinct `-d` binary, never the
        // stable `sqryd`. This is the core #308 isolation guarantee.
        match &inputs.dev_sqryd_path {
            Some(sp) if !inputs.dev_sqryd_exists => findings.push(Finding::fail(
                "dev-sqryd-missing",
                format!(
                    "dev wrapper's SQRYD_PATH points at {} which does not exist (the \
                     dev daemon would fall back to the stable sqryd). Re-run \
                     scripts/install-dev.sh.",
                    sp.display()
                ),
            )),
            Some(sp) if !looks_like_dev_binary(sp) => findings.push(Finding::fail(
                "dev-pointing-at-stable",
                format!(
                    "dev wrapper's SQRYD_PATH ({}) is not a `-d` daemon binary (the dev \
                     channel would spawn the stable daemon). Re-run scripts/install-dev.sh.",
                    sp.display()
                ),
            )),
            Some(sp) => findings.push(Finding::ok(
                "dev-sqryd",
                format!("dev SQRYD_PATH: {}", sp.display()),
            )),
            None => findings.push(Finding::fail(
                "dev-sqryd-unset",
                "dev wrapper does not export SQRYD_PATH (the dev daemon would resolve \
                 the stable sqryd sibling). Re-run scripts/install-dev.sh.",
            )),
        }

        // Exec target must itself be a `-d.bin`.
        if let Some(exec) = &inputs.dev_exec_target
            && !looks_like_dev_binary(exec)
        {
            findings.push(Finding::fail(
                "dev-exec-not-dev-binary",
                format!(
                    "dev wrapper exec target {} is not a `-d` binary.",
                    exec.display()
                ),
            ));
        }
    } else {
        findings.push(Finding::warn(
            "dev-not-installed",
            "dev channel not installed (no sqry-mcp-d found). Install with \
             scripts/install-dev.sh if you need a side-by-side dev toolchain.",
        ));
    }

    // --- Daemon socket separation ---
    findings.push(Finding::ok(
        "stable-socket",
        format!("stable daemon socket: {}", inputs.stable_socket.display()),
    ));
    if let Some(dev_socket) = &inputs.dev_socket {
        if *dev_socket == inputs.stable_socket {
            findings.push(Finding::fail(
                "socket-collision",
                format!(
                    "dev daemon socket equals the stable socket ({}); the two daemons \
                     would collide.",
                    dev_socket.display()
                ),
            ));
        } else {
            findings.push(Finding::ok(
                "dev-socket",
                format!("dev daemon socket: {}", dev_socket.display()),
            ));
        }

        // Pre-#519 caveat: without socket-scoped lock/pid the dev daemon still
        // contends on the default sqryd.lock/sqryd.pid.
        if !inputs.dev_lock_pid_colocated {
            findings.push(Finding::warn(
                "pre-519-lock-pid",
                "this daemon build does not co-locate the pid/lock with a custom \
                 socket (issue #519 not present): a custom-socket dev daemon still \
                 shares the default sqryd.lock/sqryd.pid, so stable and dev daemons \
                 cannot both hold their locks. Full runtime isolation needs #519.",
            ));
        }
    }

    // --- MCP config cross-checks ---
    // Every hit (Claude global, Claude project-scoped, Gemini, Codex) is
    // evaluated independently: a mismatch confined to one scope must still
    // surface, not be masked by a clean entry found in another scope.
    for hit in &inputs.mcp_stable_commands {
        let p = PathBuf::from(&hit.command);
        if looks_like_dev_binary(&p) || path_in_target_dir(&p) {
            findings.push(Finding::fail(
                "stable-key-aimed-at-dev",
                format!(
                    "{}: stable `sqry` MCP key points at a dev/build binary: {} \
                     (agents using the stable key would run a dev build). Re-run \
                     `sqry mcp setup --channel stable`.",
                    hit.source, hit.command,
                ),
            ));
        } else {
            findings.push(Finding::ok(
                "mcp-stable",
                format!("{}: `sqry` MCP key -> {}", hit.source, hit.command),
            ));
        }
    }
    for hit in &inputs.mcp_dev_commands {
        let p = PathBuf::from(&hit.command);
        if looks_like_dev_binary(&p) || path_in_target_dir(&p) {
            findings.push(Finding::ok(
                "mcp-dev",
                format!("{}: `sqry_dev` MCP key -> {}", hit.source, hit.command),
            ));
        } else {
            findings.push(Finding::warn(
                "dev-key-aimed-at-stable",
                format!(
                    "{}: `sqry_dev` MCP key points at a non-dev binary: {}. Re-run \
                     `sqry mcp setup --channel dev`.",
                    hit.source, hit.command,
                ),
            ));
        }
    }

    // --- Two-roster compatibility (subprocess probes) ---
    match (&inputs.stable_roster, &inputs.dev_roster) {
        (Some(stable), Some(dev)) => {
            if stable == dev {
                findings.push(Finding::ok(
                    "roster-match",
                    format!(
                        "stable and dev plugin rosters match ({} plugins)",
                        stable.len()
                    ),
                ));
            } else {
                let only_stable: Vec<&str> = stable.difference(dev).map(String::as_str).collect();
                let only_dev: Vec<&str> = dev.difference(stable).map(String::as_str).collect();
                let drift = format!(
                    "stable/dev plugin rosters differ (only-stable: [{}], only-dev: [{}])",
                    only_stable.join(", "),
                    only_dev.join(", "),
                );
                // Roster drift only *fails* when a CWD graph exists that one
                // channel could reject; otherwise it is an advisory warning.
                if inputs.cwd_graph_present {
                    findings.push(Finding::fail(
                        "incompatible-graph-risk",
                        format!(
                            "{drift}. A `.sqry/graph` built by one channel may be rejected \
                             by the other with an IncompatibleGraph error. Rebuild with the \
                             matching binary: `sqry index . --force` (or `cargo install \
                             --path sqry-cli --features ...` to add the missing plugin)."
                        ),
                    ));
                } else {
                    findings.push(Finding::warn("roster-drift", drift));
                }
            }
        }
        _ => {
            if inputs.dev_installed {
                findings.push(Finding::warn(
                    "roster-probe-incomplete",
                    "could not probe both channels' plugin rosters via --list-languages",
                ));
            }
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// IO gathering
// ---------------------------------------------------------------------------

fn run_channels(json: bool) -> Result<()> {
    let inputs = gather_inputs();
    let findings = evaluate(&inputs);
    let failed = findings.iter().any(|f| f.severity == Severity::Fail);

    if json {
        render_json(&findings, failed)?;
    } else {
        render_human(&findings, failed);
    }

    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Resolve a binary by name across the current-exe dir, PATH, and the usual
/// install dirs. Mirrors `commands::mcp::find_mcp_binary`'s search order.
fn resolve_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    if let Some(home) = dirs::home_dir() {
        for sub in [".local/bin", ".cargo/bin"] {
            let candidate = home.join(sub).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Run `<bin> --version` and return the trimmed first line.
fn probe_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

/// Run `<bin> --list-languages` and parse the plugin ids it reports.
///
/// Output lines look like `- Rust (id: rust, v1.2.3): [rs]`.
fn probe_roster(bin: &Path) -> Option<BTreeSet<String>> {
    let out = Command::new(bin).arg("--list-languages").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    roster_from_probe_output(&text)
}

/// Extract the set of plugin ids from `--list-languages` output.
fn parse_roster(text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in text.lines() {
        if let Some(idx) = line.find("(id:") {
            let rest = &line[idx + "(id:".len()..];
            // id runs until the next comma or closing paren.
            let end = rest.find([',', ')']).unwrap_or(rest.len());
            let id = rest[..end].trim();
            if !id.is_empty() {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

/// Turn raw `--list-languages` stdout into a roster, treating an empty or
/// unparseable result as a failed probe rather than a genuine empty roster.
///
/// A real sqry build always ships at least one plugin, so a subprocess that
/// exits successfully but yields zero recognizable `(id: ...)` entries has
/// produced garbage (a crashed/truncated write, an incompatible output
/// format, or similar), not evidence of a plugin-less binary. Returning
/// `None` here routes that case into `evaluate`'s `roster-probe-incomplete`
/// warning instead of letting two empty sets compare equal and register as
/// a false `roster-match`.
fn roster_from_probe_output(text: &str) -> Option<BTreeSet<String>> {
    let ids = parse_roster(text);
    if ids.is_empty() { None } else { Some(ids) }
}

/// Marker values read from a dev wrapper (emitted by `install-dev.sh`).
#[derive(Debug, Default)]
struct WrapperMarkers {
    repo_id: Option<String>,
    socket: Option<PathBuf>,
    sqryd_path: Option<PathBuf>,
    exec_target: Option<PathBuf>,
}

fn parse_wrapper_markers(text: &str) -> WrapperMarkers {
    let mut m = WrapperMarkers::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(MARKER_REPO_ID) {
            m.repo_id = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix(MARKER_SOCKET) {
            m.socket = Some(PathBuf::from(v.trim()));
        } else if let Some(v) = line.strip_prefix(MARKER_SQRYD_PATH) {
            m.sqryd_path = Some(PathBuf::from(v.trim()));
        } else if let Some(v) = line.strip_prefix(MARKER_EXEC) {
            m.exec_target = Some(PathBuf::from(v.trim()));
        } else if let Some(v) = line.strip_prefix(MARKER_CHANNEL) {
            // Presence check only; value is always "dev".
            let _ = v;
        }
    }
    m
}

/// Read the stable `sqry` and dev `sqry_dev` MCP `command` from every tool
/// config + scope combination `sqry mcp setup` can actually write to:
///
/// - **Claude Code**: the global `~/.claude.json` top-level `mcpServers`,
///   AND (whenever `workspace_root` is known) the project-scoped
///   `projects[<workspace_root>].mcpServers` entry. Both are read because
///   `sqry mcp setup --scope auto` (the default) resolves to project scope
///   whenever a workspace root exists (`resolve_claude_scope` in
///   `commands::mcp`), which is the common in-repo case; `workspace_root`
///   must be exactly the value [`crate::commands::mcp::detect_workspace_root`]
///   returns so the lookup key matches
///   [`crate::commands::mcp::write_claude_project_entry`]'s write key
///   (`root.to_string_lossy()`) byte for byte.
/// - **Gemini / Codex**: global config only. `sqry mcp setup` rejects
///   `--workspace-root` for these tools (they resolve their workspace from
///   CWD at MCP-launch time instead), so they have no project scope to read.
///
/// Every hit found is returned, not just the first: a mismatch confined to
/// a single tool/scope must never be masked by a clean entry elsewhere.
fn read_mcp_commands(
    home: &Path,
    workspace_root: Option<&Path>,
) -> (Vec<McpCommandHit>, Vec<McpCommandHit>) {
    let mut stable = Vec::new();
    let mut dev = Vec::new();

    // Claude ~/.claude.json: global `mcpServers`, plus the project-scoped
    // `projects[<workspace_root>].mcpServers` entry.
    if let Ok(text) = std::fs::read_to_string(home.join(".claude.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let global_servers = v.get("mcpServers");
        if let Some(cmd) = json_command(global_servers, "sqry") {
            stable.push(McpCommandHit {
                command: cmd,
                source: "Claude (global)".to_string(),
            });
        }
        if let Some(cmd) = json_command(global_servers, "sqry_dev") {
            dev.push(McpCommandHit {
                command: cmd,
                source: "Claude (global)".to_string(),
            });
        }

        if let Some(root) = workspace_root {
            let root_str = root.to_string_lossy();
            let project_servers = v
                .get("projects")
                .and_then(|p| p.get(root_str.as_ref()))
                .and_then(|p| p.get("mcpServers"));
            if let Some(cmd) = json_command(project_servers, "sqry") {
                stable.push(McpCommandHit {
                    command: cmd,
                    source: format!("Claude (project: {root_str})"),
                });
            }
            if let Some(cmd) = json_command(project_servers, "sqry_dev") {
                dev.push(McpCommandHit {
                    command: cmd,
                    source: format!("Claude (project: {root_str})"),
                });
            }
        }
    }

    // Gemini ~/.gemini/settings.json (global only; no project scope).
    if let Ok(text) = std::fs::read_to_string(home.join(".gemini/settings.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let servers = v.get("mcpServers");
        if let Some(cmd) = json_command(servers, "sqry") {
            stable.push(McpCommandHit {
                command: cmd,
                source: "Gemini (global)".to_string(),
            });
        }
        if let Some(cmd) = json_command(servers, "sqry_dev") {
            dev.push(McpCommandHit {
                command: cmd,
                source: "Gemini (global)".to_string(),
            });
        }
    }

    // Codex ~/.codex/config.toml (global only; no project scope).
    if let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml"))
        && let Ok(doc) = text.parse::<toml_edit::DocumentMut>()
    {
        let servers = doc.get("mcp_servers");
        if let Some(cmd) = toml_command(servers, "sqry") {
            stable.push(McpCommandHit {
                command: cmd,
                source: "Codex (global)".to_string(),
            });
        }
        if let Some(cmd) = toml_command(servers, "sqry_dev") {
            dev.push(McpCommandHit {
                command: cmd,
                source: "Codex (global)".to_string(),
            });
        }
    }

    (stable, dev)
}

fn json_command(servers: Option<&serde_json::Value>, key: &str) -> Option<String> {
    servers
        .and_then(|s| s.get(key))
        .and_then(|e| e.get("command"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn toml_command(servers: Option<&toml_edit::Item>, key: &str) -> Option<String> {
    servers
        .and_then(|s| s.get(key))
        .and_then(|t| t.get("command"))
        .and_then(toml_edit::Item::as_str)
        .map(str::to_string)
}

fn gather_inputs() -> DoctorInputs {
    // Stable binaries.
    let stable_mcp_path = resolve_binary("sqry-mcp");
    let stable_sqry = resolve_binary("sqry");
    let stable_sqry_version = stable_sqry.as_deref().and_then(probe_version);

    // Stable daemon socket (default config, ignoring any env override so we
    // report the canonical stable path).
    let stable_socket = sqry_daemon::DaemonConfig::default().socket_path();

    // Dev wrapper + markers.
    let dev_wrapper = resolve_binary("sqry-mcp-d");
    let dev_installed = dev_wrapper.is_some();

    let mut dev_repo_id = None;
    let mut dev_socket = None;
    let mut dev_sqryd_path = None;
    let mut dev_exec_target = None;
    let mut dev_sqryd_exists = false;
    let mut dev_lock_pid_colocated = false;

    if let Some(w) = &dev_wrapper
        && let Ok(text) = std::fs::read_to_string(w)
    {
        let markers = parse_wrapper_markers(&text);
        dev_repo_id = markers.repo_id;
        dev_socket = markers.socket.clone();
        dev_sqryd_path = markers.sqryd_path.clone();
        dev_exec_target = markers.exec_target;
        dev_sqryd_exists = markers.sqryd_path.as_deref().is_some_and(Path::exists);

        // Pre-#519 detection: build a config with the dev socket and check
        // whether pid/lock co-locate under the dev socket's parent.
        if let Some(socket) = &markers.socket {
            let mut dev_cfg = sqry_daemon::DaemonConfig::default();
            dev_cfg.socket.path = Some(socket.clone());
            let socket_parent = dev_cfg.socket_path().parent().map(Path::to_path_buf);
            let pid_parent = dev_cfg.pid_path().parent().map(Path::to_path_buf);
            let lock_parent = dev_cfg.lock_path().parent().map(Path::to_path_buf);
            dev_lock_pid_colocated = socket_parent.is_some()
                && socket_parent == pid_parent
                && socket_parent == lock_parent;
        }
    }

    // Workspace root, resolved exactly like `sqry mcp setup --scope auto`
    // (`crate::commands::mcp::detect_workspace_root`), so the project-scoped
    // Claude MCP-config lookup below targets the same `projects[<root>]` key
    // `sqry mcp setup` would have written from this CWD.
    let workspace_root = crate::commands::mcp::detect_workspace_root();

    // MCP config cross-check (global + Claude project-scoped, plus
    // Gemini/Codex global-only).
    let (mcp_stable_commands, mcp_dev_commands) = dirs::home_dir()
        .map(|home| read_mcp_commands(&home, workspace_root.as_deref()))
        .unwrap_or_default();

    // CWD graph presence.
    let cwd_graph_present = std::env::current_dir()
        .map(|cwd| cwd.join(".sqry").join("graph").is_dir())
        .unwrap_or(false);

    // Two-roster probes (subprocess).
    let stable_roster = stable_sqry.as_deref().and_then(probe_roster);
    let dev_roster = resolve_binary("sqry-d").as_deref().and_then(probe_roster);

    DoctorInputs {
        stable_mcp_path,
        stable_sqry_version,
        stable_socket,
        dev_installed,
        dev_wrapper_path: dev_wrapper,
        dev_repo_id,
        dev_socket,
        dev_sqryd_path,
        dev_exec_target,
        dev_sqryd_exists,
        dev_lock_pid_colocated,
        mcp_stable_commands,
        mcp_dev_commands,
        cwd_graph_present,
        stable_roster,
        dev_roster,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_human(findings: &[Finding], failed: bool) {
    println!("sqry doctor: channel separation (issue #308)\n");
    for f in findings {
        let tag = match f.severity {
            Severity::Ok => "OK  ",
            Severity::Warn => "WARN",
            Severity::Fail => "FAIL",
        };
        println!("[{tag}] {}: {}", f.code, f.message);
    }
    println!();
    if failed {
        println!("Result: FAIL (mixed-channel condition detected; see FAIL lines above).");
    } else {
        println!("Result: OK (no mixed-channel condition detected).");
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    ok: bool,
    fail_count: usize,
    warn_count: usize,
    findings: &'a [Finding],
}

fn render_json(findings: &[Finding], failed: bool) -> Result<()> {
    let report = JsonReport {
        ok: !failed,
        fail_count: findings
            .iter()
            .filter(|f| f.severity == Severity::Fail)
            .count(),
        warn_count: findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
            .count(),
        findings,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn codes_with(findings: &[Finding], sev: Severity) -> Vec<&'static str> {
        findings
            .iter()
            .filter(|f| f.severity == sev)
            .map(|f| f.code)
            .collect()
    }

    fn base_inputs() -> DoctorInputs {
        DoctorInputs {
            stable_mcp_path: Some(PathBuf::from("/home/u/.local/bin/sqry-mcp")),
            stable_socket: PathBuf::from("/run/user/1000/sqry/sqryd.sock"),
            ..Default::default()
        }
    }

    // -- path helpers --

    #[test]
    fn target_dir_detection() {
        assert!(path_in_target_dir(Path::new(
            "/repo/target/release/sqry-mcp"
        )));
        assert!(!path_in_target_dir(Path::new(
            "/home/u/.local/bin/sqry-mcp"
        )));
    }

    #[test]
    fn dev_binary_detection() {
        assert!(looks_like_dev_binary(Path::new("/x/sqry-mcp-d")));
        assert!(looks_like_dev_binary(Path::new("/x/sqryd-d.bin")));
        assert!(!looks_like_dev_binary(Path::new("/x/sqry-mcp")));
        assert!(!looks_like_dev_binary(Path::new("/x/sqryd")));
    }

    #[test]
    fn roster_parsing() {
        let text = "Enabled languages (2):\n\
                    - Rust (id: rust, v1.2.3): [rs]\n\
                    - Python (id: python, v0.9): [py, pyi]\n";
        let ids = parse_roster(text);
        assert_eq!(
            ids,
            ["python".to_string(), "rust".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn wrapper_marker_parsing() {
        let text = "#!/bin/sh\n\
                    # sqry-dev-channel: dev\n\
                    # sqry-dev-repo-id: sqry-3f9a1c\n\
                    # sqry-dev-socket: /run/user/1000/sqry-dev/sqry-3f9a1c/sqryd.sock\n\
                    # sqry-dev-sqryd-path: /home/u/.local/bin/sqryd-d.bin\n\
                    # sqry-dev-exec: /home/u/.local/bin/sqry-mcp-d.bin\n\
                    exec ...\n";
        let m = parse_wrapper_markers(text);
        assert_eq!(m.repo_id.as_deref(), Some("sqry-3f9a1c"));
        assert_eq!(
            m.socket,
            Some(PathBuf::from(
                "/run/user/1000/sqry-dev/sqry-3f9a1c/sqryd.sock"
            ))
        );
        assert_eq!(
            m.sqryd_path,
            Some(PathBuf::from("/home/u/.local/bin/sqryd-d.bin"))
        );
        assert_eq!(
            m.exec_target,
            Some(PathBuf::from("/home/u/.local/bin/sqry-mcp-d.bin"))
        );
    }

    // -- evaluation: clean state --

    #[test]
    fn clean_stable_only_has_no_failures() {
        let inputs = base_inputs();
        let findings = evaluate(&inputs);
        assert!(
            codes_with(&findings, Severity::Fail).is_empty(),
            "unexpected failures: {:?}",
            codes_with(&findings, Severity::Fail)
        );
        // Dev not installed is a warning, not a failure.
        assert!(codes_with(&findings, Severity::Warn).contains(&"dev-not-installed"));
    }

    // -- failure class: stable mcp in target/ --

    #[test]
    fn stable_mcp_in_target_fails() {
        let mut inputs = base_inputs();
        inputs.stable_mcp_path = Some(PathBuf::from("/repo/target/release/sqry-mcp"));
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"stable-mcp-in-target"));
    }

    // -- failure class: dev pointing at stable daemon --

    #[test]
    fn dev_sqryd_pointing_at_stable_fails() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_wrapper_path = Some(PathBuf::from("/home/u/.local/bin/sqry-mcp-d"));
        // SQRYD_PATH points at the stable `sqryd`, not a `-d` binary.
        inputs.dev_sqryd_path = Some(PathBuf::from("/home/u/.local/bin/sqryd"));
        inputs.dev_sqryd_exists = true;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"dev-pointing-at-stable"));
    }

    #[test]
    fn dev_sqryd_missing_fails() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_sqryd_path = Some(PathBuf::from("/home/u/.local/bin/sqryd-d.bin"));
        inputs.dev_sqryd_exists = false;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"dev-sqryd-missing"));
    }

    #[test]
    fn dev_sqryd_unset_fails() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_sqryd_path = None;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"dev-sqryd-unset"));
    }

    // -- failure class: socket collision --

    #[test]
    fn socket_collision_fails() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_sqryd_path = Some(PathBuf::from("/home/u/.local/bin/sqryd-d.bin"));
        inputs.dev_sqryd_exists = true;
        inputs.dev_socket = Some(inputs.stable_socket.clone());
        inputs.dev_lock_pid_colocated = true;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"socket-collision"));
    }

    #[test]
    fn distinct_dev_socket_no_collision() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_sqryd_path = Some(PathBuf::from("/home/u/.local/bin/sqryd-d.bin"));
        inputs.dev_sqryd_exists = true;
        inputs.dev_socket = Some(PathBuf::from("/run/user/1000/sqry-dev/sqry-abc/sqryd.sock"));
        inputs.dev_lock_pid_colocated = true;
        let findings = evaluate(&inputs);
        assert!(!codes_with(&findings, Severity::Fail).contains(&"socket-collision"));
    }

    // -- pre-#519 warning --

    #[test]
    fn pre_519_emits_warning() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_sqryd_path = Some(PathBuf::from("/home/u/.local/bin/sqryd-d.bin"));
        inputs.dev_sqryd_exists = true;
        inputs.dev_socket = Some(PathBuf::from("/run/user/1000/sqry-dev/sqry-abc/sqryd.sock"));
        inputs.dev_lock_pid_colocated = false;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Warn).contains(&"pre-519-lock-pid"));
    }

    // -- failure class: stable MCP key aimed at a dev binary --

    #[test]
    fn stable_mcp_key_aimed_at_dev_fails() {
        let mut inputs = base_inputs();
        inputs.mcp_stable_commands = vec![McpCommandHit {
            command: "/home/u/.local/bin/sqry-mcp-d".to_string(),
            source: "Claude (global)".to_string(),
        }];
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"stable-key-aimed-at-dev"));
    }

    // -- failure class: stable sqry-mcp binary itself is a dev build --

    #[test]
    fn stable_mcp_is_dev_binary_detected() {
        let mut inputs = base_inputs();
        inputs.stable_mcp_path = Some(PathBuf::from("/home/u/.local/bin/sqry-mcp-d"));
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"stable-mcp-is-dev-binary"));

        // Clean case: an ordinary (non `-d`, non-target) stable path must
        // never fire this finding.
        let clean = base_inputs();
        let clean_findings = evaluate(&clean);
        assert!(!codes_with(&clean_findings, Severity::Fail).contains(&"stable-mcp-is-dev-binary"));
    }

    // -- failure class: dev wrapper's exec target is not a `-d` binary --

    #[test]
    fn dev_exec_not_dev_binary_detected() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.dev_exec_target = Some(PathBuf::from("/home/u/.local/bin/sqry-mcp"));
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"dev-exec-not-dev-binary"));

        // Clean case: a genuine `-d` exec target must never fire this
        // finding.
        let mut clean = base_inputs();
        clean.dev_installed = true;
        clean.dev_exec_target = Some(PathBuf::from("/home/u/.local/bin/sqry-mcp-d"));
        let clean_findings = evaluate(&clean);
        assert!(!codes_with(&clean_findings, Severity::Fail).contains(&"dev-exec-not-dev-binary"));
    }

    // -- warning class: dev MCP key aimed at a non-dev binary --

    #[test]
    fn dev_key_aimed_at_stable_detected() {
        let mut inputs = base_inputs();
        inputs.mcp_dev_commands = vec![McpCommandHit {
            command: "/home/u/.local/bin/sqry-mcp".to_string(),
            source: "Claude (global)".to_string(),
        }];
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Warn).contains(&"dev-key-aimed-at-stable"));

        // Clean case: a `sqry_dev` key that genuinely points at a dev binary
        // must never fire this finding.
        let mut clean = base_inputs();
        clean.mcp_dev_commands = vec![McpCommandHit {
            command: "/home/u/.local/bin/sqry-mcp-d".to_string(),
            source: "Claude (global)".to_string(),
        }];
        let clean_findings = evaluate(&clean);
        assert!(!codes_with(&clean_findings, Severity::Warn).contains(&"dev-key-aimed-at-stable"));
    }

    // -- project-scoped Claude MCP config (issue #308 doctor blocker) --

    #[test]
    fn read_mcp_commands_finds_global_and_project_scoped_claude_entries() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let workspace_root = tmp.path().join("repo");
        fs::create_dir_all(&workspace_root).unwrap();
        let workspace_root = workspace_root.canonicalize().unwrap();
        let root_str = workspace_root.to_string_lossy().to_string();

        // Global `sqry` key is clean; the project-scoped `sqry` key (the
        // one `sqry mcp setup --scope auto` actually writes from inside a
        // project) points at a dev binary. This is the exact shape
        // `write_claude_project_entry` produces
        // (`sqry-cli/src/commands/mcp.rs`).
        let claude_json = serde_json::json!({
            "mcpServers": {
                "sqry": {
                    "type": "stdio",
                    "command": "/home/u/.local/bin/sqry-mcp",
                    "args": ["--no-daemon"]
                }
            },
            "projects": {
                root_str.clone(): {
                    "mcpServers": {
                        "sqry": {
                            "type": "stdio",
                            "command": "/home/u/.local/bin/sqry-mcp-d",
                            "args": ["--no-daemon"]
                        }
                    }
                }
            }
        });
        fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&claude_json).unwrap(),
        )
        .unwrap();

        let (stable, _dev) = read_mcp_commands(&home, Some(&workspace_root));
        assert_eq!(
            stable.len(),
            2,
            "expected both global and project hits: {stable:?}"
        );
        assert!(
            stable.iter().any(
                |h| h.command == "/home/u/.local/bin/sqry-mcp" && h.source == "Claude (global)"
            ),
            "missing clean global hit: {stable:?}"
        );
        assert!(
            stable
                .iter()
                .any(|h| h.command == "/home/u/.local/bin/sqry-mcp-d"
                    && h.source == format!("Claude (project: {root_str})")),
            "missing project-scoped hit: {stable:?}"
        );
    }

    #[test]
    fn project_scoped_stable_key_aimed_at_dev_is_detected_end_to_end() {
        // Reproduces the #308 doctor blocker directly: a stable-key mismatch
        // that exists ONLY under `projects[<root>].mcpServers` (no global
        // `mcpServers.sqry` entry at all, the common in-repo `--scope auto`
        // case) must still surface as `stable-key-aimed-at-dev`. Before the
        // fix, `read_mcp_commands` never inspected `projects[...]` at all,
        // so this scenario passed silently.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let workspace_root = tmp.path().join("repo");
        fs::create_dir_all(&workspace_root).unwrap();
        let workspace_root = workspace_root.canonicalize().unwrap();
        let root_str = workspace_root.to_string_lossy().to_string();

        let claude_json = serde_json::json!({
            "projects": {
                root_str: {
                    "mcpServers": {
                        "sqry": {
                            "type": "stdio",
                            "command": "/home/u/.local/bin/sqry-mcp-d",
                            "args": ["--no-daemon"]
                        }
                    }
                }
            }
        });
        fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&claude_json).unwrap(),
        )
        .unwrap();

        let (stable, dev) = read_mcp_commands(&home, Some(&workspace_root));
        assert!(
            stable
                .iter()
                .any(|h| h.command == "/home/u/.local/bin/sqry-mcp-d"),
            "project-scoped stable-key entry was not read at all: {stable:?}"
        );

        let mut inputs = base_inputs();
        inputs.mcp_stable_commands = stable;
        inputs.mcp_dev_commands = dev;
        let findings = evaluate(&inputs);
        assert!(
            codes_with(&findings, Severity::Fail).contains(&"stable-key-aimed-at-dev"),
            "project-scope-only mismatch was not detected: {findings:?}"
        );
    }

    #[test]
    fn read_mcp_commands_without_workspace_root_skips_project_scope() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let workspace_root = tmp.path().join("repo");
        fs::create_dir_all(&workspace_root).unwrap();
        let workspace_root = workspace_root.canonicalize().unwrap();
        let root_str = workspace_root.to_string_lossy().to_string();

        let claude_json = serde_json::json!({
            "projects": {
                root_str: {
                    "mcpServers": {
                        "sqry": {
                            "type": "stdio",
                            "command": "/home/u/.local/bin/sqry-mcp-d",
                            "args": ["--no-daemon"]
                        }
                    }
                }
            }
        });
        fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&claude_json).unwrap(),
        )
        .unwrap();

        // No workspace root known (e.g. doctor run outside any project):
        // there is no project key to look up, so no hits at all.
        let (stable, dev) = read_mcp_commands(&home, None);
        assert!(stable.is_empty());
        assert!(dev.is_empty());
    }

    // -- roster probe: empty/unparseable output must not fake a match --

    #[test]
    fn roster_from_probe_output_empty_is_treated_as_incomplete() {
        assert_eq!(roster_from_probe_output(""), None);
        assert_eq!(
            roster_from_probe_output("no recognizable plugin lines here\n"),
            None
        );
    }

    #[test]
    fn roster_from_probe_output_nonempty_parses() {
        let text = "Enabled languages (1):\n- Rust (id: rust, v1.2.3): [rs]\n";
        assert_eq!(
            roster_from_probe_output(text),
            Some(["rust".to_string()].into_iter().collect())
        );
    }

    #[test]
    fn roster_probe_incomplete_detected_when_dev_installed() {
        let mut inputs = base_inputs();
        inputs.dev_installed = true;
        inputs.stable_roster = None;
        inputs.dev_roster = None;
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Warn).contains(&"roster-probe-incomplete"));
        // An empty-vs-empty roster must never be reported as a match.
        assert!(!codes_with(&findings, Severity::Ok).contains(&"roster-match"));

        // Clean case: without a dev install there is nothing to cross-check,
        // so an inability to probe must not surface this finding.
        let mut clean = base_inputs();
        clean.dev_installed = false;
        clean.stable_roster = None;
        clean.dev_roster = None;
        let clean_findings = evaluate(&clean);
        assert!(!codes_with(&clean_findings, Severity::Warn).contains(&"roster-probe-incomplete"));
    }

    // -- roster drift with / without a CWD graph --

    #[test]
    fn roster_drift_with_graph_fails() {
        let mut inputs = base_inputs();
        inputs.cwd_graph_present = true;
        inputs.stable_roster = Some(
            ["rust".to_string(), "python".to_string()]
                .into_iter()
                .collect(),
        );
        inputs.dev_roster = Some(["rust".to_string()].into_iter().collect());
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Fail).contains(&"incompatible-graph-risk"));
    }

    #[test]
    fn roster_drift_without_graph_warns_only() {
        let mut inputs = base_inputs();
        inputs.cwd_graph_present = false;
        inputs.stable_roster = Some(
            ["rust".to_string(), "python".to_string()]
                .into_iter()
                .collect(),
        );
        inputs.dev_roster = Some(["rust".to_string()].into_iter().collect());
        let findings = evaluate(&inputs);
        assert!(!codes_with(&findings, Severity::Fail).contains(&"incompatible-graph-risk"));
        assert!(codes_with(&findings, Severity::Warn).contains(&"roster-drift"));
    }

    #[test]
    fn matching_rosters_ok() {
        let mut inputs = base_inputs();
        inputs.cwd_graph_present = true;
        let roster: BTreeSet<String> = ["rust".to_string(), "python".to_string()]
            .into_iter()
            .collect();
        inputs.stable_roster = Some(roster.clone());
        inputs.dev_roster = Some(roster);
        let findings = evaluate(&inputs);
        assert!(codes_with(&findings, Severity::Ok).contains(&"roster-match"));
    }
}
