//! Redaction of the daemon-hosted MCP's responses (integration round 7,
//! decision D-i7-5).
//!
//! The standalone `sqry-mcp` server redacts every tool response, the
//! refusals included, under the preset its configuration names (`minimal`
//! when nothing is configured): a path under the request's workspace reads
//! `<workspace>/...`, and any other absolute path is redacted whatever
//! directory it starts in, under a path key and in message text, within
//! the in-text limits `sqry-mcp-redaction`'s `rules::pattern` lists and
//! pins (round 8, decision D-i8-21, which names each limit and what it
//! leaves readable, among them a path holding whitespace, one glued to a
//! word or after a `-`, and a UNC server name holding `$`). The daemon host
//! redacted nothing, so the same call
//! through `sqry-mcp --daemon` printed absolute host paths under the
//! default preset.
//!
//! [`McpRedaction`] gives the daemon host the same redaction. The preset is
//! the daemon's own MCP configuration (`McpConfig::redaction_preset`, from
//! `SQRY_REDACTION_PRESET` in the daemon's environment, `minimal` when it
//! is unset, read trimmed and in any letter case; an unknown name refuses
//! the bind, decision D-i8-20; no configuration file is read), the same
//! source the daemon
//! host already takes its tool
//! feature flags from (`SQRY_MCP_ENABLE_*`). Each request's redactor is
//! bound to the workspace the request names, as the standalone server binds
//! it: the root, and the logical workspace read from the root's
//! `.sqry-workspace` registry or, failing that, the single root.
//!
//! The construction mirrors the standalone server's
//! (`SqryServer::create_redactor`, `SqryServer::redactor_for_workspace` and
//! `resolve_logical_workspace_for_root` in `sqry-mcp`, which are private to
//! that crate); `sqry-daemon/tests/mcp_host_redaction.rs` holds the two
//! hosts to the same observable rules.

use std::path::Path;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::CallToolResult;
use serde_json::Value;
use sqry_core::workspace::LogicalWorkspace;
use sqry_mcp_redaction::{
    LogicalWorkspaceView, RedactionConfig, RedactionPreset, Redactor, compute_source_root_id,
};

/// The workspace registry file a root may carry (the literal the standalone
/// server and the LSP read).
const SQRY_WORKSPACE_FILENAME: &str = ".sqry-workspace";

/// The redaction the daemon host applies to every tool response.
#[derive(Debug, Clone)]
pub struct McpRedaction {
    /// The preset's redactor, bound to no workspace; `None` only when
    /// redaction is off on purpose ([`Self::disabled`], for a test that
    /// compares raw payloads). No preset name turns it off.
    base: Option<Arc<Redactor>>,
}

impl McpRedaction {
    /// No redaction at all: payloads and errors pass through untouched.
    /// For tests that compare a daemon-hosted payload with the standalone
    /// executor's raw output. Not the preset `none`, which still rewrites
    /// excluded paths.
    #[must_use]
    pub fn disabled() -> Self {
        Self { base: None }
    }

    /// The redaction for the preset `preset`, as the standalone server
    /// builds it (`SqryServer::create_redactor`): a preset name read
    /// trimmed and in any letter case (`none`, `minimal`, `relative`,
    /// `standard`, `strict`: [`RedactionPreset::parse`]), with the
    /// fine-grained `SQRY_REDACT_*` overrides from the environment.
    ///
    /// # Errors
    ///
    /// An unknown preset, or a configuration the redactor refuses (an
    /// over-long `SQRY_HASH_SALT`, an unparseable `SQRY_PRESERVE_PATHS`).
    /// Both used to disable redaction with a warning, so a daemon started
    /// with `SQRY_REDACTION_PRESET=Strict` served every client unredacted;
    /// the IPC server now refuses to bind instead (decision D-i8-20).
    pub fn from_preset(preset: &str) -> anyhow::Result<Self> {
        let parsed = RedactionPreset::parse(preset).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown redaction preset {preset:?}: expected one of {} (any letter case); \
                 refusing to serve unredacted",
                RedactionPreset::NAMES.join(", ")
            )
        })?;
        let redactor = Redactor::new(RedactionConfig::from_preset_with_env(parsed.name()))
            .map_err(|err| {
                anyhow::anyhow!(
                    "The redaction configuration for preset {:?} is refused ({err}); \
                     refusing to serve unredacted",
                    parsed.name()
                )
            })?;
        Ok(Self {
            base: Some(Arc::new(redactor)),
        })
    }

    /// The redaction the daemon's MCP configuration names.
    ///
    /// # Errors
    ///
    /// As [`Self::from_preset`].
    pub fn from_mcp_config(config: &sqry_mcp::McpConfig) -> anyhow::Result<Self> {
        Self::from_preset(&config.redaction_preset)
    }

    /// `true` when responses are redacted at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.base.is_some()
    }

    /// The redactor for one request: the preset's, bound to `root` and its
    /// logical workspace when the request names one, unbound otherwise.
    fn redactor_for(&self, root: Option<&Path>) -> Option<Arc<Redactor>> {
        let base = self.base.as_ref()?;
        let Some(root) = root else {
            return Some(Arc::clone(base));
        };
        let mut config = base.config().clone();
        config.workspace_root = Some(root.to_path_buf());
        let bound = match logical_workspace_for_root(root) {
            Some(logical) => {
                Redactor::with_logical_workspace(config, logical_workspace_view(&logical))
            }
            None => Redactor::new(config),
        };
        match bound {
            Ok(redactor) => Some(Arc::new(redactor)),
            Err(err) => {
                // The standalone server answers unredacted here; the daemon
                // keeps the unbound redactor, which still redacts every
                // absolute path.
                tracing::warn!(
                    workspace = %root.display(),
                    "binding the redactor to the workspace failed, redacting unbound: {err}"
                );
                Some(Arc::clone(base))
            }
        }
    }

    /// Redact a successful tool result: its structured payload, and the
    /// text content rendered again from the redacted payload (the
    /// standalone server renders its text from the redacted response).
    #[must_use]
    pub fn redact_result(&self, root: Option<&Path>, result: CallToolResult) -> CallToolResult {
        let Some(redactor) = self.redactor_for(root) else {
            return result;
        };
        let Some(mut payload) = result.structured_content else {
            return result;
        };
        redactor.redact(&mut payload);
        let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        super::call_tool_result_with_text_and_structured(text, payload)
    }

    /// Redact a tool error as the standalone server does
    /// ([`sqry_mcp::error::redact_mcp_error`]): the message and the whole
    /// `data` object.
    #[must_use]
    pub fn redact_error(&self, root: Option<&Path>, err: McpError) -> McpError {
        let redactor = self.redactor_for(root);
        sqry_mcp::error::redact_mcp_error(redactor.as_deref(), err)
    }

    /// Redact one outcome of a tool call.
    ///
    /// # Errors
    ///
    /// The outcome's error, redacted.
    pub fn redact_outcome(
        &self,
        root: Option<&Path>,
        outcome: Result<CallToolResult, McpError>,
    ) -> Result<CallToolResult, McpError> {
        match outcome {
            Ok(result) => Ok(self.redact_result(root, result)),
            Err(err) => Err(self.redact_error(root, err)),
        }
    }

    /// Redact a JSON value with the redactor bound to `root` (for a test
    /// that inspects a value the host would send).
    #[doc(hidden)]
    pub fn redact_value(&self, root: Option<&Path>, value: &mut Value) {
        if let Some(redactor) = self.redactor_for(root) {
            redactor.redact(value);
        }
    }
}

/// The logical workspace at `root`: the root's `.sqry-workspace` registry
/// when it has a readable one, otherwise `root` as a single source root.
fn logical_workspace_for_root(root: &Path) -> Option<LogicalWorkspace> {
    let registry = root.join(SQRY_WORKSPACE_FILENAME);
    if registry.is_file() {
        match LogicalWorkspace::from_sqry_workspace(&registry) {
            Ok(workspace) => return Some(workspace),
            Err(error) => tracing::debug!(
                workspace_root = %root.display(),
                registry = %registry.display(),
                %error,
                "failed to load .sqry-workspace; redacting against the single root"
            ),
        }
    }
    match LogicalWorkspace::single_root(root.to_path_buf()) {
        Ok(workspace) => Some(workspace),
        Err(error) => {
            tracing::debug!(
                workspace_root = %root.display(),
                %error,
                "single-root logical workspace could not be built; redacting against the root"
            );
            None
        }
    }
}

/// The redaction view of a logical workspace: its short id, its source
/// roots keyed by their ids, its member folders and its exclusions.
fn logical_workspace_view(workspace: &LogicalWorkspace) -> LogicalWorkspaceView {
    let workspace_id_short = workspace.workspace_id().as_short_hex();
    let source_roots = workspace
        .source_roots()
        .iter()
        .map(|root| {
            let id = compute_source_root_id(&workspace_id_short, &root.path);
            (id, root.path.clone())
        })
        .collect();
    let member_folders = workspace
        .member_folders()
        .iter()
        .map(|member| member.path.clone())
        .collect();
    LogicalWorkspaceView {
        workspace_id_short,
        source_roots,
        member_folders,
        exclusions: workspace.exclusions().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace root the tests bind: an explicit path outside every
    /// directory the old in-string pattern listed (`/home`, `/Users`,
    /// `/var`, `/srv`, `/opt`, `/tmp`, `/etc`), so the result does not
    /// depend on where the host keeps its temporary files. Redaction is
    /// textual and needs no such directory on disk.
    const ROOT: &str = "/dev/shm/sqry-r8-daemon-redaction/ws";

    /// Absolute paths outside the bound root and outside the old seven
    /// prefixes, in the Unix and Windows forms a refusal's text may carry.
    const OUTSIDE: &[&str] = &[
        "/mnt/cache/x",
        "/nix/store/abc123-pkg/lib",
        "/root/.ssh/id_ed25519",
        "/data/x",
        "/dev/shm/sqry-r8-daemon-redaction/other",
        "C:\\Users\\me\\proj\\a.rs",
        "c:/proj/a.rs",
        "\\\\server\\share\\x",
    ];

    /// A path under the bound root reads `<workspace>` under the default
    /// preset, and every other absolute path in the text is redacted
    /// whatever its prefix; disabled redaction leaves the text as it is.
    #[test]
    fn the_default_preset_redacts_a_workspace_path() {
        let root = Path::new(ROOT);
        let message = format!("rebuild of {ROOT} refused: {}", OUTSIDE.join(" and "));
        let minimal = McpRedaction::from_mcp_config(&sqry_mcp::McpConfig::default())
            .expect("the default preset builds");
        assert!(minimal.is_enabled());
        let mut value = serde_json::json!({
            "message": message,
            "details": { "expand_cache_dir": OUTSIDE[0], "root": ROOT }
        });
        minimal.redact_value(Some(root), &mut value);
        let redacted = value["message"].as_str().expect("message");
        assert!(
            redacted.starts_with("rebuild of <workspace> refused: "),
            "{value}"
        );
        for path in OUTSIDE.iter().chain([&ROOT]) {
            assert!(!value.to_string().contains(path), "{path} in {value}");
        }
        for prefix in [
            "/mnt",
            "/nix",
            "/root",
            "/data",
            "/dev/shm",
            "c:/",
            "\\\\server",
        ] {
            assert!(!value.to_string().contains(prefix), "{prefix} in {value}");
        }

        let mut untouched = serde_json::json!({ "message": message });
        McpRedaction::disabled().redact_value(Some(root), &mut untouched);
        assert_eq!(untouched["message"], serde_json::json!(message));
    }

    /// Audit S7 (plant D03): the text content a client reads is redacted
    /// as the structured content is. `redact_result` renders the text from
    /// the redacted payload; rendered from the payload before redaction,
    /// the text carried the absolute root while the structured content
    /// did not, and no test read the text.
    #[test]
    fn the_text_content_is_redacted_as_the_structured_content_is() {
        let root = Path::new(ROOT);
        let payload = serde_json::json!({
            "root": ROOT,
            "file": format!("{ROOT}/a.rs"),
            "note": format!("cache at {}", OUTSIDE[1])
        });
        let result = super::super::call_tool_result_with_text_and_structured(
            serde_json::to_string_pretty(&payload).expect("json"),
            payload,
        );
        let minimal = McpRedaction::from_mcp_config(&sqry_mcp::McpConfig::default())
            .expect("the default preset builds");
        let redacted = minimal.redact_result(Some(root), result);
        let text: String = redacted
            .content
            .iter()
            .filter_map(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        let structured = redacted
            .structured_content
            .clone()
            .expect("structured")
            .to_string();
        assert!(!text.is_empty(), "the result carries a text content");
        for shown in [ROOT, OUTSIDE[1]] {
            assert!(!structured.contains(shown), "structured: {structured}");
            assert!(!text.contains(shown), "text: {text}");
        }
    }

    /// Round 8 (D-i8-20): a miscased or padded preset name is that preset,
    /// and an unknown one is refused, never read as no redaction. Before,
    /// `Strict` and `bogus-preset` both disabled redaction.
    #[test]
    fn a_preset_name_is_read_in_any_case_and_an_unknown_one_is_refused() {
        let root = Path::new(ROOT);
        let message = format!("cannot read {ROOT}/src/lib.rs or {}", OUTSIDE[0]);
        for (preset, hashed) in [
            ("Strict", true),
            (" MINIMAL ", false),
            ("Standard\n", false),
        ] {
            let redaction = McpRedaction::from_preset(preset)
                .unwrap_or_else(|err| panic!("{preset:?} is a preset: {err}"));
            assert!(redaction.is_enabled(), "{preset:?}");
            let mut value = serde_json::json!({ "message": message });
            redaction.redact_value(Some(root), &mut value);
            let text = value.to_string();
            assert!(
                !text.contains(ROOT) && !text.contains(OUTSIDE[0]),
                "{preset:?}: {text}"
            );
            assert_eq!(text.contains("lib.rs"), !hashed, "{preset:?}: {text}");
        }
        for preset in ["bogus-preset", "", "  ", "strictest"] {
            let err = McpRedaction::from_preset(preset)
                .err()
                .unwrap_or_else(|| panic!("{preset:?} must be refused"));
            let shown = err.to_string();
            assert!(
                shown.contains(&format!("{preset:?}"))
                    && shown.contains("refusing to serve unredacted"),
                "{preset:?}: {shown}"
            );
        }
        let config = sqry_mcp::McpConfig {
            redaction_preset: "Strict".to_string(),
            ..sqry_mcp::McpConfig::default()
        };
        assert!(McpRedaction::from_mcp_config(&config).is_ok_and(|r| r.is_enabled()));
    }
}
