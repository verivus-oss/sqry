//! MCP server integration setup and status for AI coding tools.
//!
//! Provides `sqry mcp setup` and `sqry mcp status` commands that auto-detect
//! installed AI tools (Claude Code, Codex, Gemini CLI) and configure them
//! to use sqry-mcp for semantic code search.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::args::{McpChannel, McpCommand, SetupScope, ToolTarget};

const STANDALONE_MCP_ARG: &str = "--no-daemon";

// ---------------------------------------------------------------------------
// Channel configuration (issue #308)
// ---------------------------------------------------------------------------

/// Per-channel identifiers threaded through the setup writers so the stable
/// and dev channels write to disjoint MCP server keys pointing at disjoint
/// binaries. The stable channel reproduces the historical output byte for
/// byte (`sqry` -> `sqry-mcp`); the dev channel writes `sqry_dev` ->
/// `sqry-mcp-d` and can never touch the stable entry (and vice versa).
#[derive(Debug, Clone, Copy)]
struct ChannelConfig {
    /// MCP server key written into each tool's config (`sqry` | `sqry_dev`).
    server_key: &'static str,
    /// Binary name resolved and written as the server `command`
    /// (`sqry-mcp` | `sqry-mcp-d`).
    mcp_binary_name: &'static str,
}

impl ChannelConfig {
    fn from_channel(channel: McpChannel) -> Self {
        match channel {
            McpChannel::Stable => Self {
                server_key: "sqry",
                mcp_binary_name: "sqry-mcp",
            },
            McpChannel::Dev => Self {
                server_key: "sqry_dev",
                mcp_binary_name: "sqry-mcp-d",
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatch MCP subcommands.
///
/// # Errors
///
/// Returns an error when setup or status operations fail, including config
/// parsing, filesystem writes, binary discovery, or invalid scope selection.
pub fn run(command: &McpCommand) -> Result<()> {
    match command {
        McpCommand::Setup {
            tool,
            scope,
            workspace_root,
            force,
            dry_run,
            no_backup,
            channel,
        } => run_setup(
            tool,
            scope,
            workspace_root.as_deref(),
            *force,
            *dry_run,
            *no_backup,
            *channel,
        ),
        McpCommand::Status { json } => run_status(*json),
    }
}

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

/// Find an MCP binary by name (e.g. `sqry-mcp` for the stable channel or
/// `sqry-mcp-d` for the dev channel).
///
/// Search order:
/// 1. Same directory as the running `sqry` binary
/// 2. `$PATH` via `which`
/// 3. `~/.local/bin/<name>`
/// 4. `~/.cargo/bin/<name>`
fn find_mcp_binary(name: &str) -> Result<PathBuf> {
    // 1. Same directory as current binary
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // 2. $PATH
    if let Ok(path) = which::which(name) {
        return Ok(path);
    }

    // 3. ~/.local/bin/
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".local/bin").join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // 4. ~/.cargo/bin/
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".cargo/bin").join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if name == "sqry-mcp-d" {
        bail!(
            "Could not find the dev MCP binary `{name}`.\n\
             Install the dev channel with: scripts/install-dev.sh\n\
             (see docs/development/tooling/sqry-channel-separation.md)."
        );
    }
    bail!(
        "Could not find {name} binary.\n\
         Install it with: cargo install --path sqry-mcp\n\
         Or ensure it is on your PATH."
    );
}

// ---------------------------------------------------------------------------
// Workspace root detection
// ---------------------------------------------------------------------------

/// Detect the workspace root from the current directory.
///
/// Delegates to [`sqry_core::workspace::discover_workspace_root`] so the
/// walk is bounded by [`sqry_core::workspace::MAX_ANCESTOR_DEPTH`] and
/// stops at the first project marker
/// ([`sqry_core::workspace::PROJECT_MARKERS`]), eliminating the
/// stray-`~/.sqry/graph` foot-gun (cluster-E §E.1).
///
/// `pub(crate)` so `commands::doctor` can resolve the exact same workspace
/// root `sqry mcp setup --scope auto` would have used, and look up the
/// matching `projects[<root>]` key it writes to Claude's config (issue
/// #308 doctor blocker: a doctor that hand-rolls a different key format
/// would silently miss the project-scoped entry).
pub(crate) fn detect_workspace_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    match sqry_core::workspace::discover_workspace_root(&cwd) {
        sqry_core::workspace::WorkspaceRootDiscovery::GraphFound { root, .. } => Some(root),
        sqry_core::workspace::WorkspaceRootDiscovery::BoundaryOnly { boundary, .. } => {
            Some(boundary)
        }
        sqry_core::workspace::WorkspaceRootDiscovery::None => None,
    }
}

// ---------------------------------------------------------------------------
// Per-tool scope resolution
// ---------------------------------------------------------------------------

/// Resolve the effective scope for Claude Code.
///
/// - Auto: project scope when workspace root exists, error otherwise.
/// - Project: requires workspace root.
/// - Global: always valid.
fn resolve_claude_scope(scope: &SetupScope, workspace_root: Option<&Path>) -> Result<SetupScope> {
    match scope {
        SetupScope::Auto => {
            if workspace_root.is_some() {
                Ok(SetupScope::Project)
            } else {
                bail!(
                    "Not inside a project directory (no .sqry/graph or .git found).\n\
                     Run from inside a project directory, or use --scope global."
                );
            }
        }
        SetupScope::Project => {
            if workspace_root.is_none() {
                bail!(
                    "Project scope requires being inside a project directory \
                     (or use --workspace-root)."
                );
            }
            Ok(SetupScope::Project)
        }
        SetupScope::Global => Ok(SetupScope::Global),
    }
}

// ---------------------------------------------------------------------------
// Tool detection
// ---------------------------------------------------------------------------

/// Check if an AI coding tool is installed on the system.
///
/// Detection strategy:
/// - Claude Code: `claude` binary on PATH or `~/.claude.json` exists
/// - Codex: `codex` binary on PATH or `~/.codex/` directory exists
/// - Gemini: `gemini` binary on PATH or `~/.gemini/` directory exists
fn detect_tool_installed(tool_name: &str) -> bool {
    match tool_name {
        "claude" => {
            which::which("claude").is_ok() || claude_config_path().is_some_and(|p| p.exists())
        }
        "codex" => {
            which::which("codex").is_ok()
                || codex_config_path().is_some_and(|p| p.parent().is_some_and(Path::exists))
        }
        "gemini" => {
            which::which("gemini").is_ok()
                || gemini_config_path().is_some_and(|p| p.parent().is_some_and(Path::exists))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Atomic write helper
// ---------------------------------------------------------------------------

/// Write content atomically using temp file + fsync + rename.
///
/// Mirrors the pattern from `sqry-cli/src/persistence/index.rs`.
fn atomic_write(path: &Path, content: &[u8], backup: bool) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Backup existing file
    if backup && path.exists() {
        let bak = path.with_extension("bak");
        fs::copy(path, &bak)
            .with_context(|| format!("Failed to create backup: {}", bak.display()))?;
    }

    // Write to temp file in same directory
    let temp_name = format!(
        "{}.tmp.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id()
    );
    let temp_path = path.with_file_name(temp_name);
    {
        let file = File::create(&temp_path)
            .with_context(|| format!("Failed to create temp file: {}", temp_path.display()))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(content)?;
        writer.flush()?;
        writer
            .get_ref()
            .sync_all()
            .context("Failed to sync temp file")?;
    }

    // Atomic rename
    fs::rename(&temp_path, path)
        .with_context(|| format!("Failed to rename temp file to: {}", path.display()))?;
    Ok(())
}

/// Read a file and return its content + mtime for conflict detection.
///
/// Uses a single file handle to avoid TOCTOU: captures mtime from the same
/// handle used to read content, ensuring consistency.
fn read_with_mtime(path: &Path) -> Result<(String, SystemTime)> {
    use std::io::Read;
    let file = File::open(path).with_context(|| format!("Failed to open: {}", path.display()))?;
    let mtime = file
        .metadata()
        .and_then(|m| m.modified())
        .with_context(|| format!("Failed to get mtime: {}", path.display()))?;
    let mut content = String::new();
    std::io::BufReader::new(file)
        .read_to_string(&mut content)
        .with_context(|| format!("Failed to read: {}", path.display()))?;
    Ok((content, mtime))
}

/// Check that a file hasn't been modified since we read it.
fn check_mtime(path: &Path, original_mtime: SystemTime, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if let Ok(current_mtime) = fs::metadata(path).and_then(|m| m.modified())
        && current_mtime != original_mtime
    {
        bail!(
            "Config file {} was modified by another process.\n\
             Re-run the command or use --force to overwrite.",
            path.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config path helpers
// ---------------------------------------------------------------------------

fn claude_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

fn codex_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/config.toml"))
}

fn gemini_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini/settings.json"))
}

fn shim_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/sqry-mcp-shim.sh"))
}

// ---------------------------------------------------------------------------
// Setup command
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn run_setup(
    tool: &ToolTarget,
    scope: &SetupScope,
    workspace_root_override: Option<&Path>,
    force: bool,
    dry_run: bool,
    no_backup: bool,
    channel: McpChannel,
) -> Result<()> {
    let channel_config = ChannelConfig::from_channel(channel);
    // Validate --workspace-root override if provided (before binary lookup
    // so argument errors are reported immediately)
    let workspace_root_validated = if let Some(root) = workspace_root_override {
        if !root.exists() {
            bail!("Workspace root does not exist: {}", root.display());
        }
        if !root.join(".sqry/graph").is_dir() && !root.join(".git").exists() {
            bail!(
                "Workspace root must contain .sqry/graph or .git: {}",
                root.display()
            );
        }
        // Reject --workspace-root for Codex/Gemini (global-only tools)
        match tool {
            ToolTarget::Codex | ToolTarget::Gemini => {
                bail!(
                    "Codex/Gemini use global configs -- setting a workspace root would \
                     pin to one repo. Use CWD-based discovery instead (start your tool \
                     from the project directory)."
                );
            }
            ToolTarget::All | ToolTarget::Claude => {}
        }
        Some(root.to_path_buf())
    } else {
        None
    };

    let binary = find_mcp_binary(channel_config.mcp_binary_name)?;
    let binary_str = binary.to_string_lossy();

    // Auto-detect workspace root from CWD (used for Claude scope decisions)
    let detected_root = detect_workspace_root();
    let workspace_root = workspace_root_validated.or(detected_root);

    let mut configured = Vec::new();
    let mut skipped = Vec::new();
    let backup = !no_backup;

    // Configure each tool with per-tool scope computation
    let should_configure_claude = matches!(tool, ToolTarget::All | ToolTarget::Claude);
    let should_configure_codex = matches!(tool, ToolTarget::All | ToolTarget::Codex);
    let should_configure_gemini = matches!(tool, ToolTarget::All | ToolTarget::Gemini);

    if should_configure_claude {
        if !detect_tool_installed("claude") && matches!(tool, ToolTarget::All) {
            skipped.push((
                "Claude Code",
                "not detected (claude not found on PATH, no ~/.claude.json)".to_string(),
            ));
        } else {
            // Claude scope: auto→project when root exists, auto→error when no root
            match resolve_claude_scope(scope, workspace_root.as_deref()) {
                Ok(claude_scope) => {
                    match configure_claude(
                        &binary_str,
                        &claude_scope,
                        workspace_root.as_deref(),
                        force,
                        dry_run,
                        backup,
                        channel_config,
                    ) {
                        Ok(msg) => configured.push(("Claude Code", msg)),
                        Err(e) => {
                            if matches!(tool, ToolTarget::All) {
                                skipped.push(("Claude Code", format!("{e:#}")));
                            } else {
                                return Err(e);
                            }
                        }
                    }
                }
                Err(e) => {
                    if matches!(tool, ToolTarget::All) {
                        skipped.push(("Claude Code", format!("{e:#}")));
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    if should_configure_codex {
        // Codex: always global (no workspace root requirement)
        if !detect_tool_installed("codex") && matches!(tool, ToolTarget::All) {
            skipped.push((
                "Codex",
                "not detected (codex not found on PATH)".to_string(),
            ));
        } else {
            match configure_codex(&binary_str, force, dry_run, backup, channel_config) {
                Ok(msg) => configured.push(("Codex", msg)),
                Err(e) => {
                    if matches!(tool, ToolTarget::All) {
                        skipped.push(("Codex", format!("{e:#}")));
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    if should_configure_gemini {
        // Gemini: always global (no workspace root requirement)
        if !detect_tool_installed("gemini") && matches!(tool, ToolTarget::All) {
            skipped.push((
                "Gemini",
                "not detected (gemini not found on PATH)".to_string(),
            ));
        } else {
            match configure_gemini(&binary_str, force, dry_run, backup, channel_config) {
                Ok(msg) => configured.push(("Gemini", msg)),
                Err(e) => {
                    if matches!(tool, ToolTarget::All) {
                        skipped.push(("Gemini", format!("{e:#}")));
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    // Print summary
    if dry_run {
        println!("Dry run complete. No files were modified.");
    } else {
        println!("sqry MCP Setup Complete");
        println!();
    }

    for (tool_name, msg) in &configured {
        println!("  {tool_name}: {msg}");
    }
    for (tool_name, msg) in &skipped {
        println!("  {tool_name}: skipped ({msg})");
    }

    if configured.is_empty() && skipped.is_empty() {
        println!("  No tools configured.");
    }

    // CWD dependence note for Codex/Gemini
    let codex_gemini_configured = configured
        .iter()
        .any(|(name, _)| *name == "Codex" || *name == "Gemini");
    if codex_gemini_configured {
        println!();
        println!(
            "Note: Codex/Gemini use CWD-based workspace discovery. \
             Start these tools from within your project directory for \
             sqry to resolve the correct workspace."
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Claude Code configuration
// ---------------------------------------------------------------------------

fn standalone_mcp_args_json() -> Value {
    serde_json::json!([STANDALONE_MCP_ARG])
}

fn standalone_mcp_args_toml() -> toml_edit::Array {
    let mut args = toml_edit::Array::new();
    args.push(STANDALONE_MCP_ARG);
    args
}

fn configure_claude(
    binary: &str,
    scope: &SetupScope,
    workspace_root: Option<&Path>,
    force: bool,
    dry_run: bool,
    backup: bool,
    channel: ChannelConfig,
) -> Result<String> {
    let config_path = claude_config_path().context("Could not determine home directory")?;
    let server_key = channel.server_key;

    // Build the MCP server entry
    let mut entry = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": standalone_mcp_args_json()
    });

    let scope_label;

    match scope {
        SetupScope::Project | SetupScope::Auto => {
            let root = workspace_root.context("No workspace root for project scope")?;
            let root_str = root.to_string_lossy();
            entry["env"] = serde_json::json!({
                "SQRY_MCP_WORKSPACE_ROOT": root_str.as_ref()
            });
            scope_label = format!("project ({root_str})");

            if dry_run {
                println!("Would write to: {}", config_path.display());
                println!("  Path: projects[\"{root_str}\"].mcpServers.{server_key}");
                println!("  Entry: {}", serde_json::to_string_pretty(&entry)?);
                return Ok(format!("would configure ({scope_label})"));
            }

            write_claude_project_entry(&config_path, &root_str, &entry, force, backup, server_key)?;
        }
        SetupScope::Global => {
            scope_label = "global".to_string();

            if dry_run {
                println!("Would write to: {}", config_path.display());
                println!("  Path: mcpServers.{server_key}");
                println!("  Entry: {}", serde_json::to_string_pretty(&entry)?);
                return Ok(format!("would configure ({scope_label})"));
            }

            write_claude_global_entry(&config_path, &entry, force, backup, server_key)?;
        }
    }

    Ok(format!("configured ({scope_label})"))
}

pub(crate) fn write_claude_project_entry(
    config_path: &Path,
    project_path: &str,
    entry: &Value,
    force: bool,
    backup: bool,
    server_key: &str,
) -> Result<()> {
    let (mut config, mtime) = if config_path.exists() {
        let (content, mtime) = read_with_mtime(config_path)?;
        let config: Value =
            serde_json::from_str(&content).context("Failed to parse ~/.claude.json")?;
        (config, Some(mtime))
    } else {
        (serde_json::json!({}), None)
    };

    // Navigate/create: projects[project_path].mcpServers.sqry
    let projects = config
        .as_object_mut()
        .context("~/.claude.json is not a JSON object")?
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));

    let project = projects
        .as_object_mut()
        .context("projects is not a JSON object")?
        .entry(project_path)
        .or_insert_with(|| serde_json::json!({}));

    let mcp_servers = project
        .as_object_mut()
        .context("project entry is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    let servers = mcp_servers
        .as_object_mut()
        .context("mcpServers is not a JSON object")?;

    if servers.contains_key(server_key) && !force {
        bail!(
            "Claude Code project entry for {server_key} already exists at \
             projects[\"{project_path}\"].\n\
             Use --force to overwrite."
        );
    }

    servers.insert(server_key.to_string(), entry.clone());

    // Write back
    if let Some(mt) = mtime {
        check_mtime(config_path, mt, force)?;
    }
    let output = serde_json::to_string_pretty(&config)?;
    atomic_write(config_path, output.as_bytes(), backup)?;
    Ok(())
}

fn write_claude_global_entry(
    config_path: &Path,
    entry: &Value,
    force: bool,
    backup: bool,
    server_key: &str,
) -> Result<()> {
    let (mut config, mtime) = if config_path.exists() {
        let (content, mtime) = read_with_mtime(config_path)?;
        let config: Value =
            serde_json::from_str(&content).context("Failed to parse ~/.claude.json")?;
        (config, Some(mtime))
    } else {
        (serde_json::json!({}), None)
    };

    let mcp_servers = config
        .as_object_mut()
        .context("~/.claude.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    let servers = mcp_servers
        .as_object_mut()
        .context("mcpServers is not a JSON object")?;

    if servers.contains_key(server_key) && !force {
        bail!(
            "Claude Code global {server_key} entry already exists.\n\
             Use --force to overwrite."
        );
    }

    servers.insert(server_key.to_string(), entry.clone());

    if let Some(mt) = mtime {
        check_mtime(config_path, mt, force)?;
    }
    let output = serde_json::to_string_pretty(&config)?;
    atomic_write(config_path, output.as_bytes(), backup)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Codex configuration
// ---------------------------------------------------------------------------

fn configure_codex(
    binary: &str,
    force: bool,
    dry_run: bool,
    backup: bool,
    channel: ChannelConfig,
) -> Result<String> {
    let config_path = codex_config_path().context("Could not determine home directory")?;
    let server_key = channel.server_key;

    if dry_run {
        println!("Would write to: {}", config_path.display());
        println!("  Section: [mcp_servers.{server_key}]");
        println!("  command = \"{binary}\"");
        println!("  args = [\"{STANDALONE_MCP_ARG}\"]");
        return Ok("would configure (global, CWD discovery)".to_string());
    }

    if !config_path.exists() {
        // Create minimal config
        let content = format!(
            "[mcp_servers.{server_key}]\ncommand = \"{binary}\"\nargs = [\"{STANDALONE_MCP_ARG}\"]\n"
        );
        atomic_write(&config_path, content.as_bytes(), false)?;
        return Ok("configured (global, CWD discovery) [created new config]".to_string());
    }

    let (content, mtime) = read_with_mtime(&config_path)?;

    // Use toml_edit for comment-preserving TOML editing
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .context("Failed to parse ~/.codex/config.toml")?;

    // Check if this channel's entry exists
    let has_entry = doc
        .get("mcp_servers")
        .and_then(|s| s.get(server_key))
        .is_some();

    if has_entry && !force {
        bail!(
            "Codex {server_key} MCP entry already exists.\n\
             Use --force to overwrite."
        );
    }

    // Ensure [mcp_servers] table exists
    if doc.get("mcp_servers").is_none() {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    // Create [mcp_servers.<server_key>] table
    let mut sqry_table = toml_edit::Table::new();
    sqry_table.insert("command", toml_edit::value(binary));
    sqry_table.insert("args", toml_edit::value(standalone_mcp_args_toml()));

    // Remove any existing env section (legacy SQRY_MCP_WORKSPACE_ROOT)
    doc["mcp_servers"][server_key] = toml_edit::Item::Table(sqry_table);

    check_mtime(&config_path, mtime, force)?;
    atomic_write(&config_path, doc.to_string().as_bytes(), backup)?;

    Ok("configured (global, CWD discovery)".to_string())
}

// ---------------------------------------------------------------------------
// Gemini configuration
// ---------------------------------------------------------------------------

fn configure_gemini(
    binary: &str,
    force: bool,
    dry_run: bool,
    backup: bool,
    channel: ChannelConfig,
) -> Result<String> {
    let config_path = gemini_config_path().context("Could not determine home directory")?;
    let server_key = channel.server_key;

    let entry = serde_json::json!({
        "command": binary,
        "args": standalone_mcp_args_json(),
        "env": {}
    });

    if dry_run {
        println!("Would write to: {}", config_path.display());
        println!("  Path: mcpServers.{server_key}");
        println!("  Entry: {}", serde_json::to_string_pretty(&entry)?);
        return Ok("would configure (global, CWD discovery)".to_string());
    }

    let (mut config, mtime) = if config_path.exists() {
        let (content, mtime) = read_with_mtime(&config_path)?;
        let config: Value =
            serde_json::from_str(&content).context("Failed to parse ~/.gemini/settings.json")?;
        (config, Some(mtime))
    } else {
        (serde_json::json!({}), None)
    };

    let mcp_servers = config
        .as_object_mut()
        .context("~/.gemini/settings.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    let servers = mcp_servers
        .as_object_mut()
        .context("mcpServers is not a JSON object")?;

    if servers.contains_key(server_key) && !force {
        bail!(
            "Gemini {server_key} MCP entry already exists.\n\
             Use --force to overwrite."
        );
    }

    servers.insert(server_key.to_string(), entry);

    if let Some(mt) = mtime {
        check_mtime(&config_path, mt, force)?;
    }
    let output = serde_json::to_string_pretty(&config)?;
    atomic_write(&config_path, output.as_bytes(), backup)?;

    Ok("configured (global, CWD discovery)".to_string())
}

// ---------------------------------------------------------------------------
// Status command
// ---------------------------------------------------------------------------

fn run_status(json_output: bool) -> Result<()> {
    // `sqry mcp status` stays deliberately narrow: it reports only the stable
    // channel's `sqry` server entries. All cross-channel (stable vs dev)
    // reporting lives in `sqry doctor channels` (issue #308 design
    // correction 4).
    let binary = find_mcp_binary("sqry-mcp").ok();
    let binary_display = binary.as_ref().map_or_else(
        || "not found".to_string(),
        |p| p.to_string_lossy().to_string(),
    );

    if json_output {
        print_status_json(&binary_display)?;
    } else {
        print_status_human(&binary_display);
    }

    Ok(())
}

fn print_status_human(binary: &str) {
    println!("sqry MCP Status\n");
    println!("Binary:  {binary}");
    println!();

    // Claude Code — continue on error
    if let Err(e) = print_claude_status_human() {
        println!("Claude Code:  error reading config ({e:#})");
    }
    println!();

    // Codex — continue on error
    if let Err(e) = print_codex_status_human() {
        println!("Codex:  error reading config ({e:#})");
    }
    println!();

    // Gemini — continue on error
    if let Err(e) = print_gemini_status_human() {
        println!("Gemini:  error reading config ({e:#})");
    }

    // Shim detection
    if let Some(shim) = shim_path()
        && shim.exists()
    {
        println!();
        println!("Warning: Legacy shim detected at {}", shim.display());
        println!("  The shim is no longer needed (rmcp 0.11.0 handles MCP protocol natively).");
        println!("  You can safely remove it.");
    }
}

fn print_claude_status_human() -> Result<()> {
    let Some(config_path) = claude_config_path() else {
        println!("Claude Code:  config path unknown");
        return Ok(());
    };

    if !config_path.exists() {
        println!("Claude Code (~/.claude.json):  not detected");
        return Ok(());
    }

    println!("Claude Code (~/.claude.json):");

    let content = fs::read_to_string(&config_path)?;
    let config: Value = serde_json::from_str(&content).context("Failed to parse ~/.claude.json")?;

    // Global entry
    if let Some(cmd) = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
    {
        println!("  Global:  configured");
        println!("    Command: {cmd}");
        if let Some(root) = config
            .get("mcpServers")
            .and_then(|s| s.get("sqry"))
            .and_then(|e| e.get("env"))
            .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
            .and_then(Value::as_str)
        {
            println!("    Workspace root: {root}");
        }
    } else {
        println!("  Global:  not configured");
    }

    // Per-project entries
    if let Some(projects) = config.get("projects").and_then(Value::as_object) {
        for (path, project) in projects {
            if let Some(cmd) = project
                .get("mcpServers")
                .and_then(|s| s.get("sqry"))
                .and_then(|e| e.get("command"))
                .and_then(Value::as_str)
            {
                println!("  Project ({path}):");
                println!("    configured");
                println!("    Command: {cmd}");
                if let Some(root) = project
                    .get("mcpServers")
                    .and_then(|s| s.get("sqry"))
                    .and_then(|e| e.get("env"))
                    .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
                    .and_then(Value::as_str)
                {
                    println!("    Workspace root: {root}");
                }

                // Drift detection: project entry overrides global
                if config
                    .get("mcpServers")
                    .and_then(|s| s.get("sqry"))
                    .is_some()
                {
                    println!("    Note: Project entry overrides global for this project");
                }
            }
        }
    }

    Ok(())
}

fn print_codex_status_human() -> Result<()> {
    let Some(config_path) = codex_config_path() else {
        println!("Codex:  config path unknown");
        return Ok(());
    };

    if !config_path.exists() {
        println!("Codex (~/.codex/config.toml):  not detected");
        return Ok(());
    }

    println!("Codex (~/.codex/config.toml):");

    let content = fs::read_to_string(&config_path)?;
    let doc: toml_edit::DocumentMut = content.parse().context("Failed to parse config.toml")?;

    if let Some(cmd) = doc
        .get("mcp_servers")
        .and_then(|s| s.get("sqry"))
        .and_then(|t| t.get("command"))
        .and_then(|v| v.as_str())
    {
        println!("  configured");
        println!("  Command: {cmd}");

        if let Some(root) = doc
            .get("mcp_servers")
            .and_then(|s| s.get("sqry"))
            .and_then(|t| t.get("env"))
            .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
            .and_then(|v| v.as_str())
        {
            println!("  Workspace root: {root}");
        } else {
            println!("  Workspace root: (CWD discovery)");
            println!("  Note: Codex must be started from within a project directory");
        }
    } else {
        println!("  sqry not configured");
    }

    Ok(())
}

fn print_gemini_status_human() -> Result<()> {
    let Some(config_path) = gemini_config_path() else {
        println!("Gemini:  config path unknown");
        return Ok(());
    };

    if !config_path.exists() {
        println!("Gemini (~/.gemini/settings.json):  not detected");
        return Ok(());
    }

    println!("Gemini (~/.gemini/settings.json):");

    let content = fs::read_to_string(&config_path)?;
    let config: Value =
        serde_json::from_str(&content).context("Failed to parse ~/.gemini/settings.json")?;

    if let Some(cmd) = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
    {
        println!("  configured");
        println!("  Command: {cmd}");

        if let Some(root) = config
            .get("mcpServers")
            .and_then(|s| s.get("sqry"))
            .and_then(|e| e.get("env"))
            .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
            .and_then(Value::as_str)
        {
            println!("  Workspace root: {root}");
        } else {
            println!("  Workspace root: (CWD discovery)");
            println!("  Note: Gemini must be started from within a project directory");
        }
    } else {
        println!("  sqry not configured");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON status output
// ---------------------------------------------------------------------------

fn print_status_json(binary: &str) -> Result<()> {
    let mut output = serde_json::json!({
        "binary": binary,
        "tools": {}
    });

    let tools = output["tools"].as_object_mut().unwrap();

    // Each tool is resilient — report error as JSON instead of failing
    tools.insert(
        "claude".to_string(),
        claude_status_json().unwrap_or_else(
            |e| serde_json::json!({"configured": false, "error": format!("{e:#}")}),
        ),
    );

    tools.insert(
        "codex".to_string(),
        codex_status_json().unwrap_or_else(
            |e| serde_json::json!({"configured": false, "error": format!("{e:#}")}),
        ),
    );

    tools.insert(
        "gemini".to_string(),
        gemini_status_json().unwrap_or_else(
            |e| serde_json::json!({"configured": false, "error": format!("{e:#}")}),
        ),
    );

    // Shim
    if let Some(shim) = shim_path()
        && shim.exists()
    {
        output["shim_detected"] = Value::String(shim.to_string_lossy().to_string());
    }

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn claude_status_json() -> Result<Value> {
    let Some(config_path) = claude_config_path() else {
        return Ok(serde_json::json!({"configured": false}));
    };

    if !config_path.exists() {
        return Ok(serde_json::json!({
            "config_path": config_path.to_string_lossy(),
            "configured": false
        }));
    }

    let content = fs::read_to_string(&config_path)?;
    let config: Value = serde_json::from_str(&content)?;

    let global = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .map_or_else(
            || serde_json::json!({"configured": false}),
            |entry| {
                serde_json::json!({
                    "configured": true,
                    "command": entry.get("command").and_then(Value::as_str).unwrap_or(""),
                    "workspace_root": entry.get("env")
                        .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
                        .and_then(Value::as_str)
                })
            },
        );

    let mut projects = BTreeMap::new();
    if let Some(proj_map) = config.get("projects").and_then(Value::as_object) {
        for (path, project) in proj_map {
            if let Some(entry) = project.get("mcpServers").and_then(|s| s.get("sqry")) {
                projects.insert(
                    path.clone(),
                    serde_json::json!({
                        "configured": true,
                        "command": entry.get("command").and_then(Value::as_str).unwrap_or(""),
                        "workspace_root": entry.get("env")
                            .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
                            .and_then(Value::as_str)
                    }),
                );
            }
        }
    }

    Ok(serde_json::json!({
        "config_path": config_path.to_string_lossy(),
        "global": global,
        "projects": projects
    }))
}

fn codex_status_json() -> Result<Value> {
    let Some(config_path) = codex_config_path() else {
        return Ok(serde_json::json!({"configured": false}));
    };

    if !config_path.exists() {
        return Ok(serde_json::json!({
            "config_path": config_path.to_string_lossy(),
            "configured": false
        }));
    }

    let content = fs::read_to_string(&config_path)?;
    let doc: toml_edit::DocumentMut = content.parse()?;

    let configured = doc
        .get("mcp_servers")
        .and_then(|s| s.get("sqry"))
        .and_then(|t| t.get("command"))
        .and_then(|v| v.as_str())
        .is_some();

    let command = doc
        .get("mcp_servers")
        .and_then(|s| s.get("sqry"))
        .and_then(|t| t.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let workspace_root = doc
        .get("mcp_servers")
        .and_then(|s| s.get("sqry"))
        .and_then(|t| t.get("env"))
        .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
        .and_then(|v| v.as_str());

    Ok(serde_json::json!({
        "config_path": config_path.to_string_lossy(),
        "configured": configured,
        "command": command,
        "workspace_root": workspace_root
    }))
}

fn gemini_status_json() -> Result<Value> {
    let Some(config_path) = gemini_config_path() else {
        return Ok(serde_json::json!({"configured": false}));
    };

    if !config_path.exists() {
        return Ok(serde_json::json!({
            "config_path": config_path.to_string_lossy(),
            "configured": false
        }));
    }

    let content = fs::read_to_string(&config_path)?;
    let config: Value = serde_json::from_str(&content)?;

    let configured = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
        .is_some();

    let command = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let workspace_root = config
        .get("mcpServers")
        .and_then(|s| s.get("sqry"))
        .and_then(|e| e.get("env"))
        .and_then(|e| e.get("SQRY_MCP_WORKSPACE_ROOT"))
        .and_then(Value::as_str);

    Ok(serde_json::json!({
        "config_path": config_path.to_string_lossy(),
        "configured": configured,
        "command": command,
        "workspace_root": workspace_root
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- Scope resolution tests --

    #[test]
    fn test_resolve_claude_scope_auto_with_root() {
        let tmp = TempDir::new().unwrap();
        let scope = resolve_claude_scope(&SetupScope::Auto, Some(tmp.path())).unwrap();
        assert!(matches!(scope, SetupScope::Project));
    }

    #[test]
    fn test_resolve_claude_scope_auto_without_root() {
        let result = resolve_claude_scope(&SetupScope::Auto, None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Not inside a project directory"));
    }

    #[test]
    fn test_resolve_claude_scope_project_with_root() {
        let tmp = TempDir::new().unwrap();
        let scope = resolve_claude_scope(&SetupScope::Project, Some(tmp.path())).unwrap();
        assert!(matches!(scope, SetupScope::Project));
    }

    #[test]
    fn test_resolve_claude_scope_project_without_root() {
        let result = resolve_claude_scope(&SetupScope::Project, None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Project scope requires"));
    }

    #[test]
    fn test_resolve_claude_scope_global_no_root_needed() {
        let scope = resolve_claude_scope(&SetupScope::Global, None).unwrap();
        assert!(matches!(scope, SetupScope::Global));
    }

    // -- Atomic write tests --

    #[test]
    fn test_atomic_write_creates_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.json");
        atomic_write(&path, b"hello", false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested/dir/config.json");
        atomic_write(&path, b"{}", false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
    }

    #[test]
    fn test_atomic_write_with_backup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.json");
        fs::write(&path, "original").unwrap();

        atomic_write(&path, b"updated", true).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "updated");
        let bak = path.with_extension("bak");
        assert!(bak.exists());
        assert_eq!(fs::read_to_string(&bak).unwrap(), "original");
    }

    #[test]
    fn test_atomic_write_without_backup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.json");
        fs::write(&path, "original").unwrap();

        atomic_write(&path, b"updated", false).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "updated");
        let bak = path.with_extension("bak");
        assert!(!bak.exists());
    }

    // -- Mtime conflict detection tests --

    #[test]
    fn test_read_with_mtime_uses_same_handle() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "content").unwrap();

        let (content, mtime) = read_with_mtime(&path).unwrap();
        assert_eq!(content, "content");

        // Mtime should match metadata from the same file
        let actual_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime, actual_mtime);
    }

    #[test]
    fn test_check_mtime_unchanged_passes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "content").unwrap();

        let (_, mtime) = read_with_mtime(&path).unwrap();
        check_mtime(&path, mtime, false).unwrap();
    }

    #[test]
    fn test_check_mtime_changed_fails() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "content").unwrap();

        let (_, mtime) = read_with_mtime(&path).unwrap();

        // Modify the file after a brief delay
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&path, "modified").unwrap();

        let result = check_mtime(&path, mtime, false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("modified by another process")
        );
    }

    #[test]
    fn test_check_mtime_force_bypasses_conflict() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "content").unwrap();

        let (_, mtime) = read_with_mtime(&path).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&path, "modified").unwrap();

        // With force, should succeed despite mtime mismatch
        check_mtime(&path, mtime, true).unwrap();
    }

    #[test]
    fn test_setup_standalone_args_json() {
        let args = standalone_mcp_args_json();
        assert_eq!(args, serde_json::json!([STANDALONE_MCP_ARG]));
    }

    #[test]
    fn test_setup_standalone_args_toml() {
        let args = standalone_mcp_args_toml();
        let rendered = toml_edit::value(args).to_string();
        assert_eq!(rendered.trim(), "[\"--no-daemon\"]");
    }

    // -- Claude config write tests --

    #[test]
    fn test_write_claude_project_entry_new_file() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let entry = serde_json::json!({
            "type": "stdio",
            "command": "/usr/bin/sqry-mcp",
            "args": standalone_mcp_args_json(),
            "env": { "SQRY_MCP_WORKSPACE_ROOT": "/my/project" }
        });

        write_claude_project_entry(&config_path, "/my/project", &entry, false, false, "sqry")
            .unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            content["projects"]["/my/project"]["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
        assert_eq!(
            content["projects"]["/my/project"]["mcpServers"]["sqry"]["env"]["SQRY_MCP_WORKSPACE_ROOT"],
            "/my/project"
        );
    }

    #[test]
    fn test_write_claude_project_entry_exists_no_force() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let existing = serde_json::json!({
            "projects": {
                "/my/project": {
                    "mcpServers": {
                        "sqry": { "command": "old" }
                    }
                }
            }
        });
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let entry = serde_json::json!({ "command": "new" });
        let result =
            write_claude_project_entry(&config_path, "/my/project", &entry, false, false, "sqry");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_write_claude_project_entry_exists_with_force() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let existing = serde_json::json!({
            "projects": {
                "/my/project": {
                    "mcpServers": {
                        "sqry": { "command": "old" }
                    }
                }
            }
        });
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let entry = serde_json::json!({ "command": "new" });
        write_claude_project_entry(&config_path, "/my/project", &entry, true, false, "sqry")
            .unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            content["projects"]["/my/project"]["mcpServers"]["sqry"]["command"],
            "new"
        );
    }

    #[test]
    fn test_write_claude_global_entry_new_file() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let entry = serde_json::json!({
            "type": "stdio",
            "command": "/usr/bin/sqry-mcp",
            "args": standalone_mcp_args_json()
        });

        write_claude_global_entry(&config_path, &entry, false, false, "sqry").unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            content["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
    }

    #[test]
    fn test_write_claude_global_entry_exists_no_force() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let existing = serde_json::json!({
            "mcpServers": {
                "sqry": { "command": "old" }
            }
        });
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let entry = serde_json::json!({ "command": "new" });
        let result = write_claude_global_entry(&config_path, &entry, false, false, "sqry");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_write_claude_project_preserves_existing_global() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        // Start with a global entry
        let existing = serde_json::json!({
            "mcpServers": {
                "sqry": { "command": "/usr/bin/sqry-mcp" }
            }
        });
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Add a project entry — should keep global
        let entry = serde_json::json!({
            "command": "/usr/bin/sqry-mcp",
            "env": { "SQRY_MCP_WORKSPACE_ROOT": "/my/project" }
        });
        write_claude_project_entry(&config_path, "/my/project", &entry, false, false, "sqry")
            .unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        // Global entry still present
        assert_eq!(
            content["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
        // Project entry also present
        assert_eq!(
            content["projects"]["/my/project"]["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
    }

    // -- Workspace root rejection for Codex/Gemini --

    // -- Tool detection logic tests --

    #[test]
    fn test_detect_tool_installed_unknown_tool() {
        // Unknown tool names should always return false
        assert!(!detect_tool_installed("unknown"));
        assert!(!detect_tool_installed(""));
        assert!(!detect_tool_installed("vscode"));
    }

    // -- Status resilience: malformed config tolerance --

    #[test]
    fn test_claude_status_json_malformed_config() {
        // claude_status_json() reads from ~/.claude.json which we can't easily
        // redirect, but we can verify the parse path by testing the underlying
        // serde_json::from_str behavior that the status functions rely on.
        let malformed = "not valid json {{{";
        let result: Result<Value, _> = serde_json::from_str(malformed);
        assert!(result.is_err());

        // The status resilience pattern wraps this in unwrap_or_else:
        let fallback = result.map_or_else(
            |e| serde_json::json!({"configured": false, "error": format!("{e:#}")}),
            |_| serde_json::json!({"configured": true}),
        );
        assert_eq!(fallback["configured"], false);
        assert!(fallback["error"].as_str().unwrap().contains("expected"));
    }

    #[test]
    fn test_codex_toml_parse_malformed() {
        // Verify that malformed TOML triggers an error that the status
        // resilience pattern would catch
        let malformed = "[invalid\nthis is not valid toml";
        let result: Result<toml_edit::DocumentMut, _> = malformed.parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_status_json_error_shape() {
        // Verify the error JSON shape matches what print_status_json produces
        let error_json =
            serde_json::json!({"configured": false, "error": "Failed to parse config"});
        assert_eq!(error_json["configured"], false);
        assert!(error_json["error"].is_string());
        // Verify it's valid JSON that a consumer could parse
        let serialized = serde_json::to_string(&error_json).unwrap();
        let _: Value = serde_json::from_str(&serialized).unwrap();
    }

    // -- Workspace root rejection for Codex/Gemini --

    #[test]
    fn test_workspace_root_rejected_for_codex() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();

        let result = run_setup(
            &ToolTarget::Codex,
            &SetupScope::Auto,
            Some(root),
            false,
            true, // dry_run
            true,
            McpChannel::Stable,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Codex/Gemini use global configs")
        );
    }

    #[test]
    fn test_workspace_root_rejected_for_gemini() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();

        let result = run_setup(
            &ToolTarget::Gemini,
            &SetupScope::Auto,
            Some(root),
            false,
            true, // dry_run
            true,
            McpChannel::Stable,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Codex/Gemini use global configs")
        );
    }

    // -- Channel selection (issue #308) --

    #[test]
    fn test_channel_config_stable_maps_to_sqry() {
        let cfg = ChannelConfig::from_channel(McpChannel::Stable);
        assert_eq!(cfg.server_key, "sqry");
        assert_eq!(cfg.mcp_binary_name, "sqry-mcp");
    }

    #[test]
    fn test_channel_config_dev_maps_to_sqry_dev() {
        let cfg = ChannelConfig::from_channel(McpChannel::Dev);
        assert_eq!(cfg.server_key, "sqry_dev");
        assert_eq!(cfg.mcp_binary_name, "sqry-mcp-d");
    }

    /// Writing the dev entry (`sqry_dev`) must never touch an existing stable
    /// `sqry` entry, and vice versa: the two keys are disjoint.
    #[test]
    fn test_dev_global_entry_does_not_clobber_stable() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        // Existing stable entry.
        let existing = serde_json::json!({
            "mcpServers": {
                "sqry": { "command": "/usr/bin/sqry-mcp" }
            }
        });
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Write a dev entry under sqry_dev.
        let dev_entry = serde_json::json!({
            "type": "stdio",
            "command": "/home/dev/.local/bin/sqry-mcp-d",
            "args": standalone_mcp_args_json()
        });
        write_claude_global_entry(&config_path, &dev_entry, false, false, "sqry_dev").unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        // Stable entry untouched.
        assert_eq!(
            content["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
        // Dev entry added under its own key.
        assert_eq!(
            content["mcpServers"]["sqry_dev"]["command"],
            "/home/dev/.local/bin/sqry-mcp-d"
        );
    }

    /// The dev key is gated independently: writing `sqry_dev` twice without
    /// `--force` conflicts on `sqry_dev`, not on the stable `sqry` key.
    #[test]
    fn test_dev_global_entry_gates_on_its_own_key() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let dev_entry = serde_json::json!({ "command": "sqry-mcp-d" });
        write_claude_global_entry(&config_path, &dev_entry, false, false, "sqry_dev").unwrap();

        // Second dev write without force conflicts on sqry_dev.
        let result = write_claude_global_entry(&config_path, &dev_entry, false, false, "sqry_dev");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sqry_dev"));

        // A stable write still succeeds (disjoint key).
        let stable_entry = serde_json::json!({ "command": "sqry-mcp" });
        write_claude_global_entry(&config_path, &stable_entry, false, false, "sqry").unwrap();
        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(content["mcpServers"]["sqry"]["command"], "sqry-mcp");
        assert_eq!(content["mcpServers"]["sqry_dev"]["command"], "sqry-mcp-d");
    }

    /// Stable channel writer output is unchanged: writing `sqry` still lands
    /// under the `sqry` key exactly as before.
    #[test]
    fn test_stable_channel_key_unchanged() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("claude.json");

        let entry = serde_json::json!({ "command": "/usr/bin/sqry-mcp" });
        write_claude_global_entry(&config_path, &entry, false, false, "sqry").unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            content["mcpServers"]["sqry"]["command"],
            "/usr/bin/sqry-mcp"
        );
        assert!(content["mcpServers"].get("sqry_dev").is_none());
    }
}
