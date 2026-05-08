//! In-daemon MCP host.
//!
//! `DaemonMcpHandler` is an rmcp `ServerHandler` that serves
//! `tools/call` directly from the daemon's preloaded workspace state,
//! routing every request through Phase 8b's
//! `daemon_adapter::execute_*_for_daemon` wrappers via the shared
//! `tool_core::classify_and_execute` pipeline (Phase 8c U6).
//!
//! Per Codex iter-2 §F (architectural decision F-B): the daemon hosts
//! an rmcp `ServerHandler` in-process on each MCP shim byte-pump
//! connection, so MCP tool behaviour is bit-identical with direct
//! sqryd JSON-RPC tool dispatch. The 15-tool subset is enumerated in
//! [`sqry_mcp::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES`] and
//! dispatched by [`sqry_mcp::daemon_adapter::dispatch::dispatch_by_name`]
//! (Phase 8c U7).
//!
//! # Lifecycle
//!
//! [`host_mcp_on_streams`] is the entrypoint for the Phase 8c shim
//! router (U10): given a raw `(AsyncRead, AsyncWrite)` pair produced
//! by the shim byte-pump transport, build a [`DaemonMcpHandler`],
//! bind it to an rmcp service, and wait for either cooperative
//! shutdown (cancellation token fires → `service.cancel()`) or the
//! rmcp runtime to drain naturally on peer disconnect.
//!
//! # Error mapping
//!
//! `call_tool` uses [`error_map::daemon_err_to_mcp_with_tool`] so any
//! [`crate::error::DaemonError`] surfaces through the same 4-key
//! `{kind, retryable, retry_after_ms, details}` envelope as the
//! standalone `sqry-mcp` path, with `details.tool` populated by the
//! inbound method name for `ToolTimeout`.

pub mod error_map;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ToolsCapability,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{Value, json};
use sqry_core::project::ProjectRootMode;
use sqry_core::query::executor::QueryExecutor;
use sqry_mcp::daemon_adapter::WorkspaceContext;
use sqry_mcp::daemon_adapter::dispatch::{dispatch_by_name, dispatch_sqry_ask};
use sqry_mcp::daemon_params::params_to_sqry_ask_args;
use sqry_mcp::execution::build_translator_config_for_path;
use sqry_mcp::tool_args::SqryAskParams;
use sqry_mcp::tools_schema;
use sqry_nl::{Translator, TranslatorConfig};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::error::DaemonError;
use crate::ipc::tool_core::{self, ExecuteVerdict};
use crate::workspace::{LoadedWorkspace, WorkspaceBuilder, WorkspaceKey, WorkspaceManager};
use error_map::{daemon_err_to_mcp, daemon_err_to_mcp_with_tool, try_onnx_runtime_missing_to_mcp};

const INITIAL_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024;

/// rmcp `ServerHandler` backing the daemon-hosted MCP surface.
///
/// Each live MCP shim connection gets its own `DaemonMcpHandler`
/// instance (cheap — it clones three `Arc`s and a
/// pre-rendered 15-entry `Tool` list). The handler re-uses the
/// daemon's long-lived [`WorkspaceManager`] / [`QueryExecutor`] so
/// tool dispatch is free of graph-rebuild cost.
///
/// `tools` is pre-computed in [`DaemonMcpHandler::new`] so every
/// `tools/list` reply is a cheap `Vec::clone` — no need to invoke the
/// filter + feature-flag traversal on every request.
///
/// `enabled_tool_names` is the canonical authorization set for
/// `call_tool`. It is derived from the **same** feature-flag filter
/// as `tools` (see [`tools_schema::daemon_supported_tools`]), so the
/// advertised-vs-callable invariant is enforced bit-identically with
/// standalone `SqryServer::ensure_tool_enabled` (see
/// `sqry-mcp/src/server.rs:186-197`). Closing the
/// `tools/list`-vs-`tools/call` gap that Codex flagged as MAJOR-1 in
/// the Phase 8c end-of-phase review iter-0.
pub struct DaemonMcpHandler {
    manager: Arc<WorkspaceManager>,
    workspace_builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    tool_timeout: Duration,
    daemon_version: &'static str,
    tools: Vec<rmcp::model::Tool>,
    enabled_tool_names: HashSet<String>,
}

impl DaemonMcpHandler {
    /// Build a handler bound to the daemon's shared workspace manager
    /// and per-request tool executor.
    ///
    /// `daemon_version` is surfaced in [`ServerHandler::get_info`] so
    /// MCP clients can tell which sqryd build is servicing their
    /// requests — keep it in sync with the daemon's
    /// `ResponseMeta::daemon_version` field on the JSON-RPC path.
    ///
    /// Both `tools` and `enabled_tool_names` are derived from
    /// [`tools_schema::daemon_supported_tools`] in a single call so the
    /// `tools/list` advertised set and `call_tool` authorization set
    /// are guaranteed identical for the lifetime of this handler. If
    /// the active feature flags disable one of the 14 daemon-supported
    /// tools, that tool is hidden from `tools/list` AND rejected by
    /// `call_tool` with `InvalidArgument`, matching the standalone
    /// `SqryServer` contract.
    #[must_use]
    pub fn new(
        manager: Arc<WorkspaceManager>,
        workspace_builder: Arc<dyn WorkspaceBuilder>,
        tool_executor: Arc<QueryExecutor>,
        tool_timeout: Duration,
        daemon_version: &'static str,
    ) -> Self {
        Self::with_tools(
            manager,
            workspace_builder,
            tool_executor,
            tool_timeout,
            daemon_version,
            tools_schema::daemon_supported_tools(),
        )
    }

    /// Build a handler with an explicit pre-filtered tool list. Used by
    /// the M-1 fix unit tests to inject a synthetic feature-flag set
    /// without process-wide env-var manipulation. The advertised set
    /// (`tools/list`) and the authorization set (`call_tool`) are both
    /// derived from the supplied `tools` vec, so they remain in lockstep
    /// regardless of how the caller filtered them.
    #[must_use]
    pub fn with_tools(
        manager: Arc<WorkspaceManager>,
        workspace_builder: Arc<dyn WorkspaceBuilder>,
        tool_executor: Arc<QueryExecutor>,
        tool_timeout: Duration,
        daemon_version: &'static str,
        tools: Vec<rmcp::model::Tool>,
    ) -> Self {
        let enabled_tool_names: HashSet<String> =
            tools.iter().map(|t| t.name.as_ref().to_owned()).collect();
        Self {
            manager,
            workspace_builder,
            tool_executor,
            tool_timeout,
            daemon_version,
            tools,
            enabled_tool_names,
        }
    }

    /// Read-only accessor for the authorization set used by
    /// [`ServerHandler::call_tool`] to gate tool execution. Returned for
    /// integration / unit tests that need to assert the
    /// advertised-vs-callable invariant without actually invoking
    /// `call_tool`. Production code MUST NOT mutate the set; this
    /// accessor is therefore by-reference (no `Clone`) so callers cannot
    /// accidentally drift their own copy out of sync with the handler.
    #[must_use]
    pub fn enabled_tool_names(&self) -> &HashSet<String> {
        &self.enabled_tool_names
    }

    /// Read-only accessor for the advertised tool list returned via
    /// [`ServerHandler::list_tools`]. Provided for the same
    /// advertised-vs-callable invariant tests as
    /// [`Self::enabled_tool_names`].
    #[must_use]
    pub fn advertised_tools(&self) -> &[rmcp::model::Tool] {
        &self.tools
    }

    /// SGA04 building block — construct a daemon graph provider over
    /// this handler's [`WorkspaceManager`] / [`WorkspaceBuilder`] pair.
    ///
    /// SGA05 routes read-only MCP tool dispatch through
    /// [`tool_core::acquire_and_execute`], which builds a provider
    /// internally per request via `tool_core::daemon_graph_provider`.
    /// This accessor stays exposed for SGA06 / SGA07 surfaces that
    /// need to construct a provider explicitly (e.g. LSP integration
    /// or out-of-band parity probes) without going through the
    /// dispatch wrapper. `rebuild_index` and `sqry_ask` flows remain
    /// outside this code path — they own their own load semantics.
    #[allow(dead_code)] // Reserved for SGA06 / SGA07 surfaces.
    pub(crate) fn daemon_graph_provider(&self) -> crate::workspace::acquirer::DaemonGraphProvider {
        tool_core::daemon_graph_provider(
            Arc::clone(&self.manager),
            Arc::clone(&self.workspace_builder),
        )
    }
}

impl ServerHandler for DaemonMcpHandler {
    fn get_info(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: None }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "sqry-daemon-mcp".into(),
                version: self.daemon_version.to_owned(),
                title: None,
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "sqry MCP server (daemon-hosted). Tool calls are served from \
                 the daemon's preloaded workspace state — same behaviour as \
                 sqry-mcp's standalone mode, zero graph rebuild cost."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: self.tools.clone(),
        })
    }

    async fn call_tool(
        &self,
        req: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let name = req.name.to_string();
        let args_value = req.arguments.map_or(Value::Null, Value::Object);

        // Reject unknown / disabled tools early with a
        // validation_error envelope. Authorization runs against
        // `enabled_tool_names` — the SAME feature-flag-filtered set
        // surfaced via `list_tools` — so the advertised-vs-callable
        // contract holds (Codex iter-0 MAJOR-1 fix). The error message
        // distinguishes the two failure modes so operators can tell
        // whether the tool is unsupported by the daemon at all
        // (DAEMON_SUPPORTED_TOOL_NAMES miss) or merely disabled by the
        // current feature-flag environment (in DAEMON_SUPPORTED_TOOL_NAMES
        // but not in enabled_tool_names). Routing through
        // `daemon_err_to_mcp(InvalidArgument)` guarantees the envelope
        // shape stays in lockstep with the missing-path case (and with
        // any future change to `daemon_err_to_mcp`) — see the
        // `unknown_tool_and_missing_path_envelopes_have_identical_top_level_keys`
        // assertion test below.
        if !self.enabled_tool_names.contains(&name) {
            let reason = if tools_schema::DAEMON_SUPPORTED_TOOL_NAMES.contains(&name.as_str()) {
                format!(
                    "tool {name} is disabled by the daemon's active feature flags \
                     (see SQRY_MCP_ENABLE_* environment variables)"
                )
            } else {
                format!("unknown tool name {name}: not in DAEMON_SUPPORTED_TOOL_NAMES")
            };
            return Err(daemon_err_to_mcp(DaemonError::InvalidArgument { reason }));
        }

        // `rebuild_index` is a workspace-loading operation, not a query
        // against an already-loaded graph. It drives
        // `WorkspaceManager::get_or_load` (and optionally `unload` for
        // force) rather than `classify_and_execute`. Handle it on a
        // dedicated path BEFORE the generic `path`-argument check below
        // — standalone `RebuildIndexParams::path` is optional and
        // defaults to `"."` (see `sqry-mcp/src/tools/params.rs:1629`),
        // so a request that omits `path` must succeed here too.
        //
        // Strictness: if `path` is PRESENT but not a string, fail with
        // `InvalidArgument` — matching standalone serde rejection of
        // `{"path": 42}` shaped requests.
        if name == "rebuild_index" {
            let path = match args_value.as_object().and_then(|m| m.get("path")) {
                Some(raw) => raw.as_str().map(String::from).ok_or_else(|| {
                    daemon_err_to_mcp(DaemonError::InvalidArgument {
                        reason: format!("rebuild_index: `path` must be a string, got: {raw}"),
                    })
                })?,
                None => ".".to_string(),
            };
            return self.handle_rebuild_index(&path, &args_value).await;
        }

        // NL07: `sqry_ask` requires the per-workspace
        // `Arc<Translator>` cached on `LoadedWorkspace.nl_translator`.
        // The standard `dispatch_by_name` path cannot reach that cell
        // (its `WorkspaceContext` is ephemeral and only carries
        // `(workspace_root, graph, executor)`), so the daemon MCP
        // host owns the routing. See `dispatch_sqry_ask` in
        // sqry-mcp/src/daemon_adapter/dispatch.rs.
        //
        // SGA05 acquisition contract for `sqry_ask`:
        //
        //   * Initial workspace admission goes through
        //     `WorkspaceManager::get_or_load` (see
        //     `load_workspace_and_translator_for_sqry_ask_blocking`).
        //     This is the SAME admission path the SGA04 provider
        //     uses internally on its bounded read-only reload, so
        //     there is no parallel / bypassing load.
        //   * Translated graph-backed execution operates against the
        //     cached `LoadedWorkspace.graph` returned by that admission
        //     — i.e. the exact `Arc<CodeGraph>` the daemon's
        //     classification machinery just admitted. There is no
        //     second graph load.
        //   * `sqry_ask` is intentionally NOT routed through
        //     `acquire_and_execute` because its initial-load
        //     semantics include admission accounting (translator
        //     init, ONNX session pool) that the read-only acquisition
        //     contract is specifically not designed to drive (per
        //     SGA02 §Tool Ownership Boundary). Routing it through
        //     `ReadOnlyQuery` would conflict with the contract.
        if name == "sqry_ask" {
            return self.handle_sqry_ask(&args_value).await;
        }

        // Extract the `path` argument — every one of the remaining 14
        // daemon-supported Args types carries a path field.
        let path = extract_path_arg(&args_value).ok_or_else(|| {
            daemon_err_to_mcp(DaemonError::InvalidArgument {
                reason: format!("{name}: missing or non-string `path` argument"),
            })
        })?;

        // Build the dispatch closure. Clones are necessary because the
        // closure crosses `spawn_blocking` inside `tool_core`.
        let name_clone = name.clone();
        let args_clone = args_value.clone();
        let run = move |wctx: &WorkspaceContext| -> anyhow::Result<Value> {
            dispatch_by_name(&name_clone, wctx, &args_clone)
        };

        // SGA05: route through the shared graph acquirer so `WorkspaceEvicted`
        // triggers the daemon provider's bounded one-shot read-only reload
        // before the tool body runs. The `acquire_and_execute` helper preserves
        // the existing wire envelope shapes — Reloaded acquisitions present as
        // Fresh on the wire (no new top-level fields), Stale acquisitions
        // continue to splice `_stale_warning` (existing behaviour). Error
        // mapping at the MCP boundary still uses
        // `daemon_err_to_mcp_with_tool` so `details.tool` is populated for
        // `ToolTimeout` and the canonical 4-key envelope shape is preserved.
        //
        // `tool_name` for diagnostics is the inbound MCP method name. Because
        // the acquirer's `tool_name` field requires a `&'static str`, we map
        // through the `DAEMON_SUPPORTED_TOOL_NAMES` table to the canonical
        // 'static literal — which is guaranteed to contain `name` here
        // because `enabled_tool_names` already gated the request.
        let static_tool_name: Option<&'static str> = tools_schema::DAEMON_SUPPORTED_TOOL_NAMES
            .iter()
            .copied()
            .find(|&n| n == name.as_str());
        let verdict = tool_core::acquire_and_execute(
            Arc::clone(&self.manager),
            Arc::clone(&self.workspace_builder),
            Arc::clone(&self.tool_executor),
            self.tool_timeout,
            &path,
            static_tool_name,
            run,
        )
        .await
        .map_err(|e| daemon_err_to_mcp_with_tool(e, &name))?;

        // Wrap in `CallToolResult`, splicing `_stale_warning` on Stale.
        // Both `content` and `structured_content` carry the SAME
        // payload so clients that prefer the structured form
        // (Codex iter-2 K test) and clients that only parse text
        // (legacy rmcp stdio) see identical data.
        let payload = match verdict {
            ExecuteVerdict::Fresh { inner, .. } => inner,
            ExecuteVerdict::Stale {
                mut inner,
                stale_warning,
                ..
            } => {
                if let Value::Object(ref mut map) = inner {
                    map.insert("_stale_warning".into(), Value::String(stale_warning));
                }
                inner
            }
        };

        // Text-payload parity: standalone sqry-mcp renders
        // `content[0].text` via `serde_json::to_string_pretty(value)`
        // with `value.to_string()` as the fallback (see
        // `sqry-mcp/src/server.rs:355-360`). Mirror that exactly so
        // legacy MCP clients that parse only `content[0].text` — not
        // `structured_content` — see byte-identical output across
        // daemon-hosted and standalone modes.
        let text_payload =
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        Ok(CallToolResult {
            content: vec![Content::text(text_payload)],
            structured_content: Some(payload),
            is_error: None,
            meta: None,
        })
    }
}

impl DaemonMcpHandler {
    /// NL07 — handle the `sqry_ask` MCP tool against the daemon's
    /// per-workspace cached `Arc<Translator>`.
    ///
    /// Steps:
    ///
    /// 1. Resolve the workspace key from the request's `path`
    ///    argument (matching how the standard `dispatch_by_name` path
    ///    canonicalises a workspace root in `tool_core::resolve_path`).
    /// 2. Look up [`LoadedWorkspace`] via
    ///    [`WorkspaceManager::lookup`]. If the workspace is not
    ///    loaded, lazily load it through the registered
    ///    [`WorkspaceBuilder`] — the same admission-controlled path
    ///    the JSON-RPC `tools/*` tools use for graph queries.
    /// 3. Lazily initialise the per-workspace translator via
    ///    [`OnceCell::get_or_try_init`]. Initialisation cost (loading
    ///    `N` ONNX sessions in the [`sqry_nl::classifier::ClassifierPool`])
    ///    is paid exactly once per workspace lifetime.
    /// 4. Dispatch to
    ///    [`sqry_mcp::daemon_adapter::dispatch::dispatch_sqry_ask`]
    ///    inside [`tokio::task::spawn_blocking`] because the pool's
    ///    `acquire()` is sync.
    ///
    /// # Errors
    ///
    /// - `DaemonError::InvalidArgument` for malformed params.
    /// - `DaemonError::WorkspaceBuildFailed` when initial workspace
    ///   load or translator construction fails.
    /// - `DaemonError::Internal` for `spawn_blocking` join errors and
    ///   for any error propagated from `dispatch_sqry_ask`.
    ///
    /// All errors are mapped through `daemon_err_to_mcp_with_tool`
    /// so the MCP envelope carries `details.tool = "sqry_ask"`.
    async fn handle_sqry_ask(&self, args_value: &Value) -> Result<CallToolResult, McpError> {
        // Parse once through the same params converter used by the
        // daemon dispatch helper. This preserves standalone parity for
        // query validation, default `path = "."`, strict bool/string
        // typing, and the NL02/NL03/NL04 model/trust parameters.
        let ask_args = params_to_sqry_ask_args(args_value.clone()).map_err(|e| {
            daemon_err_to_mcp_with_tool(
                DaemonError::InvalidArgument {
                    reason: format!("sqry_ask: invalid arguments: {e}"),
                },
                "sqry_ask",
            )
        })?;
        let path = ask_args.path.clone();
        let original_execute = ask_args.execute;

        // Canonicalise the path so workspace lookup uses the same key
        // shape as `dispatch_by_name` / `tool_core`. A relative `path`
        // resolves against the daemon process CWD.
        let canonical_root = std::fs::canonicalize(&path).map_err(|e| {
            daemon_err_to_mcp_with_tool(
                DaemonError::InvalidArgument {
                    reason: format!("sqry_ask: cannot canonicalize path {path:?}: {e}"),
                },
                "sqry_ask",
            )
        })?;
        let key = WorkspaceKey::new(canonical_root.clone(), ProjectRootMode::default(), 0);

        let (loaded, translator) = self
            .load_workspace_and_translator_for_sqry_ask(key, canonical_root.clone(), ask_args)
            .await?;

        // Build a minimal WorkspaceContext for the dispatch helper.
        // `dispatch_sqry_ask` receives it for symmetry with
        // `dispatch_by_name`; the executor inside resolves its own
        // workspace via `engine_for_workspace`.
        let wctx = WorkspaceContext {
            workspace_root: canonical_root.clone(),
            graph: loaded.graph.load_full(),
            executor: Arc::clone(&self.tool_executor),
        };

        // SGA05 §`sqry_ask` Translated Command Coverage:
        //
        // The translator's `Execute`-arm output may be a graph-backed
        // CLI invocation (e.g. `sqry query "..."`, `sqry graph
        // direct-callers "<sym>"`). The pre-SGA05 daemon route invoked
        // `execute_sqry_ask_with_translator` with the caller-supplied
        // `execute=true`, which inside `sqry-mcp` shells out via
        // `Command::new(bin)` — bypassing the daemon's shared graph
        // acquisition pipeline and therefore bypassing the SGA02/SGA04
        // read-only acquisition contract.
        //
        // Fix: always run the translator with `execute=false` here so
        // we get only the rendered command back, then post-process:
        //
        //   * If the translation rendered a daemon-supported
        //     graph-backed tool AND the caller asked for `execute=true`,
        //     dispatch through `tool_core::acquire_and_execute` — the
        //     same pipeline direct MCP tool calls use. The translated
        //     tool's response is rendered as JSON and spliced into
        //     `data.executionOutput`, preserving the existing wire shape.
        //   * If the translation rendered a non-graph-backed command
        //     (text search, version, help, etc.) and `execute=true`,
        //     we set `executionOutput` to an explicit "out-of-scope"
        //     marker rather than shelling out — daemon-hosted
        //     `sqry_ask` deliberately does NOT execute commands that
        //     fall outside the SGA02/SGA04 read-only acquisition
        //     boundary.
        //   * If the caller did not ask to execute, we leave
        //     `executionOutput` unset (translation-only response).
        //
        // Standalone `sqry-mcp` (no daemon) is unaffected and still
        // shells out — its execution path is not subject to the daemon
        // acquisition contract.
        let translator_only_args = SqryAskParams {
            execute: false,
            ..params_to_sqry_ask_args(args_value.clone()).map_err(|e| {
                daemon_err_to_mcp_with_tool(
                    DaemonError::InvalidArgument {
                        reason: format!("sqry_ask: invalid arguments: {e}"),
                    },
                    "sqry_ask",
                )
            })?
        };
        let translator_only_args_value =
            serde_json::to_value(&translator_only_args).map_err(|e| {
                daemon_err_to_mcp_with_tool(
                    DaemonError::Internal(anyhow::anyhow!(
                        "sqry_ask: re-serialize translator args: {e}"
                    )),
                    "sqry_ask",
                )
            })?;

        let mut payload = Self::dispatch_sqry_ask_blocking(
            wctx,
            Arc::clone(&translator),
            translator_only_args_value,
        )
        .await?;

        if original_execute {
            self.maybe_run_translated_graph_command(&mut payload, &canonical_root, path.as_str())
                .await?;
        }

        // Wire-format parity with the 14 query tools that route
        // through `classify_and_execute` — both `content[0].text` and
        // `structured_content` carry the same payload.
        let text_payload =
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        Ok(CallToolResult {
            content: vec![Content::text(text_payload)],
            structured_content: Some(payload),
            is_error: None,
            meta: None,
        })
    }

    /// Inspect a `sqry_ask` translation payload and, if its rendered
    /// command maps to one of the daemon-supported graph-backed read-only
    /// tools, dispatch that tool through
    /// [`tool_core::acquire_and_execute`] — preserving the SGA05
    /// acquisition contract end-to-end. The translated tool's JSON
    /// response is spliced into `data.executionOutput` so existing
    /// `sqry_ask` consumers see a captured-output string in the same
    /// shape they did when the pre-SGA05 path shelled out.
    ///
    /// Caller-supplied `original_path` is the un-canonicalised `path`
    /// the `sqry_ask` MCP request carried — re-used here as the
    /// `path` argument for the translated tool dispatch so the
    /// shared acquirer canonicalises it through the same path-policy
    /// ladder it uses for direct tool calls.
    async fn maybe_run_translated_graph_command(
        &self,
        payload: &mut Value,
        canonical_root: &Path,
        original_path: &str,
    ) -> Result<(), McpError> {
        // Locate the rendered command on the translation payload, if any.
        let command = payload
            .get("data")
            .and_then(|d| d.get("command"))
            .and_then(|c| c.as_str())
            .map(str::to_owned);
        let Some(command) = command else {
            return Ok(());
        };

        match parse_translated_graph_command(&command, original_path) {
            Some((tool_name, mut tool_args)) => {
                // SGA05: dispatch through the shared acquirer so the
                // translated tool consumes a graph admitted by the
                // daemon's read-only acquisition path. The daemon
                // provider canonicalises the path internally (PathPolicy
                // default), so we pass the un-canonicalised
                // `original_path` here exactly as direct tool calls do.
                ensure_path_arg(&mut tool_args, original_path);
                let exec_output = self
                    .dispatch_translated_graph_tool(tool_name, tool_args, original_path)
                    .await?;
                splice_execution_output(payload, exec_output);
                Ok(())
            }
            None => {
                // Not a graph-backed tool we route — refuse to shell
                // out (per SGA02 §Tool Ownership Boundary) and leave
                // an explicit marker so callers can distinguish
                // "we did not run this command" from "command produced
                // empty output". Out-of-scope translations include
                // `sqry index --status`, `sqry visualize ...`, and any
                // future CLI that does not have a daemon-supported
                // MCP equivalent.
                let marker = format!(
                    "[daemon] translated command {command:?} is not a daemon-supported \
                     graph-backed tool; daemon-hosted sqry_ask does not shell out for \
                     out-of-scope commands. Route via the daemon's MCP tools (e.g. \
                     semantic_search) or run the CLI from your client. \
                     workspace_root={}",
                    canonical_root.display()
                );
                splice_execution_output(payload, marker);
                Ok(())
            }
        }
    }

    /// SGA05 dispatch seam — given a daemon-supported graph-backed tool
    /// name and its arguments, route through
    /// [`tool_core::acquire_and_execute`] and return the rendered JSON
    /// response as a pretty-printed string. Used by
    /// [`Self::maybe_run_translated_graph_command`] (production) and
    /// the SGA07 parity test (`feature = "test-hooks"`).
    #[doc(hidden)]
    pub async fn dispatch_translated_graph_tool(
        &self,
        tool_name: &'static str,
        tool_args: Value,
        path: &str,
    ) -> Result<String, McpError> {
        // Authorization: restrict to the same daemon-supported set the
        // direct-tool path enforces. `rebuild_index` is mutating
        // (excluded), `sqry_ask` is the wrapper itself (excluded), and
        // any other unknown name is rejected.
        if !tools_schema::DAEMON_SUPPORTED_TOOL_NAMES.contains(&tool_name)
            || tool_name == "rebuild_index"
            || tool_name == "sqry_ask"
        {
            return Err(daemon_err_to_mcp_with_tool(
                DaemonError::InvalidArgument {
                    reason: format!(
                        "sqry_ask translated dispatch: tool {tool_name} is not a \
                         daemon-supported graph-backed read-only tool"
                    ),
                },
                "sqry_ask",
            ));
        }

        let name_clone: &'static str = tool_name;
        let args_clone = tool_args;
        let run = move |wctx: &WorkspaceContext| -> anyhow::Result<Value> {
            dispatch_by_name(name_clone, wctx, &args_clone)
        };

        let verdict = tool_core::acquire_and_execute(
            Arc::clone(&self.manager),
            Arc::clone(&self.workspace_builder),
            Arc::clone(&self.tool_executor),
            self.tool_timeout,
            path,
            Some(name_clone),
            run,
        )
        .await
        .map_err(|e| daemon_err_to_mcp_with_tool(e, "sqry_ask"))?;

        let payload = match verdict {
            ExecuteVerdict::Fresh { inner, .. } => inner,
            ExecuteVerdict::Stale {
                mut inner,
                stale_warning,
                ..
            } => {
                if let Value::Object(ref mut map) = inner {
                    map.insert("_stale_warning".into(), Value::String(stale_warning));
                }
                inner
            }
        };

        Ok(serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()))
    }

    /// Load the `sqry_ask` workspace and initialise its per-workspace translator.
    ///
    /// # Errors
    ///
    /// Returns an MCP error when workspace load fails, translator construction fails,
    /// or the blocking task cannot be joined.
    async fn load_workspace_and_translator_for_sqry_ask(
        &self,
        key: WorkspaceKey,
        canonical_root: PathBuf,
        ask_args: SqryAskParams,
    ) -> Result<(Arc<LoadedWorkspace>, Arc<Translator>), McpError> {
        let manager = Arc::clone(&self.manager);
        let builder = Arc::clone(&self.workspace_builder);

        tokio::task::spawn_blocking(move || {
            load_workspace_and_translator_for_sqry_ask_blocking(
                &manager,
                builder.as_ref(),
                &key,
                canonical_root.as_path(),
                &ask_args,
            )
        })
        .await
        .map_err(|join_err| {
            daemon_err_to_mcp_with_tool(
                DaemonError::Internal(anyhow::anyhow!("spawn_blocking join: {join_err}")),
                "sqry_ask",
            )
        })?
        .map_err(map_sqry_ask_translator_init_error)
    }

    /// Dispatch `sqry_ask` without blocking the daemon runtime.
    ///
    /// # Errors
    ///
    /// Returns an MCP error when dispatch fails or the blocking task cannot be joined.
    async fn dispatch_sqry_ask_blocking(
        wctx: WorkspaceContext,
        translator: Arc<Translator>,
        args_value: Value,
    ) -> Result<Value, McpError> {
        tokio::task::spawn_blocking(move || dispatch_sqry_ask(&wctx, &translator, &args_value))
            .await
            .map_err(|join_err| {
                daemon_err_to_mcp_with_tool(
                    DaemonError::Internal(anyhow::anyhow!(
                        "sqry_ask: spawn_blocking join: {join_err}"
                    )),
                    "sqry_ask",
                )
            })?
            .map_err(map_sqry_ask_dispatch_error)
    }

    /// Handle `rebuild_index` with full response-shape parity against
    /// standalone `sqry-mcp::execution::execute_rebuild_index`.
    ///
    /// Behavioural contract (mirrors `sqry-mcp/src/execution/tools/index.rs`):
    ///
    /// - `path` accepts both directories AND files. File paths resolve
    ///   to their parent directory as the effective workspace root
    ///   (standalone parity — a client calling `rebuild_index` with
    ///   `path=src/lib.rs` must see a success response in daemon mode
    ///   too, not `InvalidArgument`).
    /// - `force` defaults to `true` when omitted (standalone
    ///   `RebuildIndexParams::force` uses `#[serde(default = "default_true")]`).
    /// - When the on-disk index exists at the resolved root and
    ///   `force=false`, return the existing manifest's `built_at`,
    ///   node / edge / file counts, and an "already exists" message —
    ///   no fresh build, no bogus `builtAt = now()`.
    /// - Otherwise, drive `WorkspaceManager::get_or_load` (with an
    ///   admission-scaled working-set estimate) and return the
    ///   just-built graph stats with a fresh RFC3339 timestamp.
    ///
    /// Response rendering routes through
    /// [`sqry_mcp::daemon_adapter::tool_response_json`] so the wire
    /// envelope ( `data`, `execution_ms`, `used_graph`, `total`,
    /// `truncated`, `workspace_path`) is byte-identical with the
    /// standalone sqry-mcp transport.
    async fn handle_rebuild_index(
        &self,
        path: &str,
        args_value: &Value,
    ) -> Result<CallToolResult, McpError> {
        use sqry_mcp::execution::{RebuildIndexData, ToolExecution};

        let start = std::time::Instant::now();

        // Default `force` to `true` — matching the standalone MCP
        // `RebuildIndexParams` schema where `force` defaults to
        // `default_true` (see `sqry-mcp/src/tools/params.rs:1633`).
        // Standalone uses serde which rejects non-boolean `force` with
        // an InvalidParams error; the daemon mirrors that strict-type
        // behaviour here rather than silently falling back to `true`
        // for e.g. `"force": "yes"` or `"force": 1` (Codex gpt-5.4
        // iter-1 coverage note — non-boolean force parity).
        let force = match args_value.as_object().and_then(|m| m.get("force")) {
            Some(raw) => raw.as_bool().ok_or_else(|| {
                daemon_err_to_mcp(DaemonError::InvalidArgument {
                    reason: format!("rebuild_index: `force` must be a boolean, got: {raw}"),
                })
            })?,
            None => true,
        };

        // Canonicalise the target. Standalone accepts file paths and
        // rebuilds the parent directory; do the same here.
        let canonical_target = std::fs::canonicalize(path).map_err(|e| {
            daemon_err_to_mcp(DaemonError::InvalidArgument {
                reason: format!("rebuild_index: cannot canonicalize path {path:?}: {e}"),
            })
        })?;

        let canonical_root: std::path::PathBuf = if canonical_target.is_dir() {
            canonical_target.clone()
        } else if let Some(parent) = canonical_target.parent() {
            parent.to_path_buf()
        } else {
            return Err(daemon_err_to_mcp(DaemonError::InvalidArgument {
                reason: format!(
                    "rebuild_index: cannot derive workspace root from {} (no parent directory)",
                    canonical_target.display()
                ),
            }));
        };

        let root_display = path_to_forward_slash(&canonical_root);

        // Cache-hit path: standalone returns the existing manifest
        // when `.sqry/graph/` exists and `!force` — do the same so the
        // daemon never claims a fresh `builtAt = now()` for an
        // unchanged on-disk index.
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&canonical_root);
        if storage.exists() && !force {
            return build_rebuild_index_cache_hit_response(
                &canonical_root,
                &root_display,
                &storage,
                start,
            );
        }

        // Fresh / force-rebuild path.
        let key = WorkspaceKey::new(canonical_root.clone(), ProjectRootMode::default(), 0);

        if force {
            self.manager.unload(&key);
        }

        let working_set_estimate = initial_working_set_estimate();

        let manager = Arc::clone(&self.manager);
        let builder = Arc::clone(&self.workspace_builder);
        let key_for_task = key.clone();

        let graph = tokio::task::spawn_blocking(move || {
            manager.get_or_load(&key_for_task, &*builder, working_set_estimate)
        })
        .await
        .map_err(|join_err| {
            daemon_err_to_mcp_with_tool(
                DaemonError::WorkspaceBuildFailed {
                    root: canonical_root.clone(),
                    reason: format!("rebuild_index: task join error: {join_err}"),
                },
                "rebuild_index",
            )
        })?
        .map_err(|e| daemon_err_to_mcp_with_tool(e, "rebuild_index"))?;

        #[allow(clippy::cast_possible_truncation)]
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let node_count = graph.node_count() as u64;
        let edge_count = graph.edge_count() as u64;
        let files_indexed = graph.indexed_files().count() as u64;

        let data = RebuildIndexData {
            success: true,
            root_path: root_display.clone(),
            node_count,
            edge_count,
            files_indexed,
            built_at: chrono::Utc::now().to_rfc3339(),
            message: Some(if force {
                "Index rebuilt successfully.".to_string()
            } else {
                "Index built successfully.".to_string()
            }),
        };

        let execution = ToolExecution {
            data,
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: elapsed_ms,
            next_page_token: None,
            total: Some(1),
            truncated: Some(false),
            candidates_scanned: None,
            workspace_path: root_display,
        };

        finalize_rebuild_index_response(execution)
    }
}

/// Estimate the admission working set for initial daemon-hosted tool loads.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
#[must_use]
fn initial_working_set_estimate() -> u64 {
    (INITIAL_WORKING_SET_BYTES as f64 * crate::config::WORKING_SET_MULTIPLIER) as u64
}

fn load_workspace_and_translator_for_sqry_ask_blocking(
    manager: &Arc<WorkspaceManager>,
    builder: &dyn WorkspaceBuilder,
    key: &WorkspaceKey,
    canonical_root: &Path,
    ask_args: &SqryAskParams,
) -> Result<(Arc<LoadedWorkspace>, Arc<Translator>), DaemonError> {
    let working_set_estimate = initial_working_set_estimate();
    let _graph = manager.get_or_load(key, builder, working_set_estimate)?;
    let loaded = manager
        .lookup(key)
        .ok_or_else(|| DaemonError::WorkspaceBuildFailed {
            root: canonical_root.to_path_buf(),
            reason: "sqry_ask: workspace was loaded but disappeared from manager".to_string(),
        })?;

    let translator = loaded
        .nl_translator
        .get_or_try_init(|| build_translator_for_sqry_ask(canonical_root, ask_args))?
        .clone();

    Ok((loaded, translator))
}

fn build_translator_for_sqry_ask(
    scoped_path: &Path,
    args: &SqryAskParams,
) -> Result<Arc<Translator>, DaemonError> {
    let cfg = build_daemon_sqry_ask_translator_config(scoped_path, args);
    match Translator::new(cfg) {
        Ok(translator) => Ok(Arc::new(translator)),
        Err(nl_err @ sqry_nl::NlError::OnnxRuntimeMissing { .. }) => {
            Err(DaemonError::Internal(anyhow::Error::new(nl_err)))
        }
        Err(err) => Err(DaemonError::WorkspaceBuildFailed {
            root: scoped_path.to_path_buf(),
            reason: format!("sqry_ask: translator init failed: {err}"),
        }),
    }
}

fn map_sqry_ask_translator_init_error(err: DaemonError) -> McpError {
    if let DaemonError::Internal(ref any_err) = err
        && let Some(nl_err) = any_err.downcast_ref::<sqry_nl::NlError>()
        && let Some(mcp_err) = try_onnx_runtime_missing_to_mcp(nl_err)
    {
        return mcp_err;
    }
    daemon_err_to_mcp_with_tool(err, "sqry_ask")
}

fn map_sqry_ask_dispatch_error(err: anyhow::Error) -> McpError {
    if let Some(nl_err) = err.downcast_ref::<sqry_nl::NlError>()
        && let Some(mcp_err) = try_onnx_runtime_missing_to_mcp(nl_err)
    {
        return mcp_err;
    }
    daemon_err_to_mcp_with_tool(DaemonError::Internal(err), "sqry_ask")
}

/// Extract the `path` argument from tool args. Only valid for the 15
/// daemon-supported tool types — do NOT extend to the full 34-tool
/// standalone inventory without auditing which tool types carry a
/// `path` field.
fn extract_path_arg(args: &Value) -> Option<String> {
    args.as_object()?.get("path")?.as_str().map(String::from)
}

#[must_use]
fn build_daemon_sqry_ask_translator_config(
    scoped_path: &Path,
    args: &SqryAskParams,
) -> TranslatorConfig {
    build_translator_config_for_path(scoped_path, args)
}

/// SGA05 — parse a translator-rendered CLI command into the
/// daemon-supported MCP tool name + arguments JSON. Returns `None`
/// when the command is not a graph-backed read-only tool the daemon
/// MCP host can dispatch (text search, version, help, visualize, etc.).
///
/// Supported shapes (mirrors `sqry-nl::assembler` outputs):
///
/// - `sqry query "<expr>" [--limit N] [--language X] [--path "P"]`
///   → `semantic_search { query, max_results }`. The `query` argument is
///   the raw expression — `dispatch_by_name` invokes the same planner
///   parser standalone `sqry-mcp::semantic_search` uses.
/// - `sqry graph direct-callers "<sym>" [--language X]`
///   → `direct_callers { symbol, max_results }`.
/// - `sqry graph direct-callees "<sym>" [--language X]`
///   → `direct_callees { symbol, max_results }`.
/// - `sqry graph trace-path "<from>" "<to>" [--max-depth N]`
///   → `trace_path { from_symbol, to_symbol, max_hops, max_paths }`.
///
/// `path_arg` is propagated into every returned args object so the
/// translated tool dispatch lands on the same workspace root the
/// outer `sqry_ask` request named.
fn parse_translated_graph_command(command: &str, path_arg: &str) -> Option<(&'static str, Value)> {
    let tokens = tokenize_translated_command(command)?;
    let mut iter = tokens.iter().map(String::as_str);
    let bin = iter.next()?;
    if bin != "sqry" {
        return None;
    }
    let sub = iter.next()?;
    let remaining: Vec<&str> = iter.collect();
    match sub {
        "query" => parse_translated_query(&remaining, path_arg),
        "graph" => parse_translated_graph(&remaining, path_arg),
        // `search`, `visualize`, `index`, `--version`, `--help`, etc.
        // are out-of-scope for daemon graph acquisition.
        _ => None,
    }
}

fn parse_translated_query(rest: &[&str], path_arg: &str) -> Option<(&'static str, Value)> {
    // First positional argument is the query expression.
    let mut iter = rest.iter().copied().peekable();
    let query_expr = iter.next()?;
    let mut max_results: i64 = 100;
    while let Some(tok) = iter.next() {
        match tok {
            "--limit" => {
                if let Some(v) = iter.next()
                    && let Ok(n) = v.parse::<i64>()
                {
                    max_results = n.clamp(1, 5000);
                }
            }
            // Accept and ignore --language / --path; the daemon path
            // overrides --path with the canonical request path below.
            "--language" | "--path" => {
                let _ = iter.next();
            }
            _ => {}
        }
    }
    Some((
        "semantic_search",
        json!({
            "query": query_expr,
            "path": path_arg,
            "max_results": max_results,
            "context_lines": 0,
            "include_classpath": false,
        }),
    ))
}

fn parse_translated_graph(rest: &[&str], path_arg: &str) -> Option<(&'static str, Value)> {
    let mut iter = rest.iter().copied();
    let cmd = iter.next()?;
    match cmd {
        "direct-callers" => {
            let symbol = iter.next()?;
            Some((
                "direct_callers",
                json!({
                    "symbol": symbol,
                    "path": path_arg,
                    "max_results": 100,
                }),
            ))
        }
        "direct-callees" => {
            let symbol = iter.next()?;
            Some((
                "direct_callees",
                json!({
                    "symbol": symbol,
                    "path": path_arg,
                    "max_results": 100,
                }),
            ))
        }
        "trace-path" => {
            let from = iter.next()?;
            let to = iter.next()?;
            let mut max_hops: i64 = 10;
            while let Some(tok) = iter.next() {
                if tok == "--max-depth"
                    && let Some(v) = iter.next()
                    && let Ok(n) = v.parse::<i64>()
                {
                    max_hops = n.clamp(1, 50);
                }
            }
            Some((
                "trace_path",
                json!({
                    "from_symbol": from,
                    "to_symbol": to,
                    "path": path_arg,
                    "max_hops": max_hops,
                    "max_paths": 5,
                }),
            ))
        }
        // `graph stats`, `graph cycles`, `graph show-dependencies` — not
        // emitted by the current translator; reject them so we surface a
        // clear out-of-scope marker rather than guessing.
        _ => None,
    }
}

/// Tokenize a translator-rendered CLI command string into shell-style
/// argv components. Strips the surrounding double quotes from quoted
/// arguments while preserving inner content (the translator escapes
/// embedded quotes via `\\"`).
fn tokenize_translated_command(command: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut in_double = false;
    let mut in_single = false;
    let mut prev_escape = false;
    let mut started = false;
    for c in command.chars() {
        if prev_escape {
            buf.push(c);
            prev_escape = false;
            started = true;
            continue;
        }
        if c == '\\' && (in_double || in_single) {
            prev_escape = true;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            started = true;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            started = true;
            continue;
        }
        if c.is_whitespace() && !in_double && !in_single {
            if started {
                out.push(std::mem::take(&mut buf));
                started = false;
            }
            continue;
        }
        buf.push(c);
        started = true;
    }
    if in_double || in_single {
        return None;
    }
    if started {
        out.push(buf);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Inject (or override) the `path` field on an args JSON object so the
/// downstream `acquire_and_execute` call canonicalises the same
/// workspace root the outer `sqry_ask` request named. Translated
/// commands carry `--path` only when the translator decides to scope
/// the rendered CLI; for daemon dispatch we always use the explicit
/// `sqry_ask` path argument.
fn ensure_path_arg(args: &mut Value, path_arg: &str) {
    if let Value::Object(map) = args {
        map.insert("path".into(), Value::String(path_arg.to_string()));
    }
}

/// Splice an `executionOutput` string into a `sqry_ask` translation
/// payload's `data` object. Preserves byte-compatibility with the
/// pre-SGA05 wire shape — the camelCase field name matches
/// `NlTranslationData::execution_output` under
/// `serde(rename_all = "camelCase")`.
fn splice_execution_output(payload: &mut Value, output: String) {
    if let Some(Value::Object(data)) = payload.get_mut("data") {
        data.insert("executionOutput".into(), Value::String(output));
    }
}

/// Render a path as a forward-slash string for JSON wire output,
/// mirroring `sqry-mcp`'s `execution::symbol_utils::path_to_forward_slash`.
/// Kept private to the daemon MCP host because the helper in `sqry-mcp`
/// is `pub(crate)`; normalising here avoids a duplicate public surface
/// while keeping the wire form identical.
fn path_to_forward_slash(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Build the daemon's `rebuild_index` response for the cache-hit path
/// (on-disk index exists, caller did not request `force=true`).
///
/// Mirrors `sqry-mcp`'s standalone cache-hit branch at
/// `sqry-mcp/src/execution/tools/index.rs:165-209`: loads the manifest
/// for `built_at` and node / edge counts, prefers
/// `snapshot_header.file_count` for the authoritative file count, and
/// falls back to summing per-language counts from the manifest when the
/// snapshot header cannot be read (CLI-built indexes).
///
/// Never triggers a fresh build or warms daemon memory — that is the
/// `force=true` path's job.
fn build_rebuild_index_cache_hit_response(
    canonical_root: &std::path::Path,
    root_display: &str,
    storage: &sqry_core::graph::unified::persistence::GraphStorage,
    start: std::time::Instant,
) -> Result<CallToolResult, McpError> {
    use sqry_core::graph::unified::persistence::load_header_from_path;
    use sqry_mcp::execution::{RebuildIndexData, ToolExecution};

    let manifest = storage.load_manifest().map_err(|e| {
        daemon_err_to_mcp_with_tool(
            DaemonError::WorkspaceBuildFailed {
                root: canonical_root.to_path_buf(),
                reason: format!(
                    "rebuild_index: index exists at {} but manifest is unreadable: {e}",
                    canonical_root.display()
                ),
            },
            "rebuild_index",
        )
    })?;

    let files_indexed: u64 = if let Ok(header) = load_header_from_path(storage.snapshot_path()) {
        u64::try_from(header.file_count).unwrap_or(0)
    } else if !manifest.file_count.is_empty() {
        u64::try_from(manifest.file_count.values().sum::<usize>()).unwrap_or(0)
    } else {
        0
    };

    let data = RebuildIndexData {
        success: true,
        root_path: root_display.to_string(),
        node_count: u64::try_from(manifest.node_count).unwrap_or(0),
        edge_count: u64::try_from(manifest.edge_count).unwrap_or(0),
        files_indexed,
        built_at: manifest.built_at,
        message: Some("Index already exists. Use force=true to rebuild.".to_string()),
    };

    #[allow(clippy::cast_possible_truncation)]
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let execution = ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: elapsed_ms,
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: root_display.to_string(),
    };

    finalize_rebuild_index_response(execution)
}

/// Render a `ToolExecution<RebuildIndexData>` through the shared
/// `sqry-mcp` response builder so the daemon-hosted MCP wire envelope
/// matches the standalone transport byte-for-byte. Both `content[0].text`
/// and `structured_content` carry the same payload — wire parity with
/// the 14 query tools that route through `classify_and_execute`.
fn finalize_rebuild_index_response(
    execution: sqry_mcp::execution::ToolExecution<sqry_mcp::execution::RebuildIndexData>,
) -> Result<CallToolResult, McpError> {
    let payload = sqry_mcp::daemon_adapter::tool_response_json(execution)?;
    let text_payload =
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    Ok(CallToolResult {
        content: vec![Content::text(text_payload)],
        structured_content: Some(payload),
        is_error: None,
        meta: None,
    })
}

/// Host an rmcp `ServerHandler` on raw byte-pump streams.
///
/// Called by the Phase 8c shim router (U10) for each
/// `ShimProtocol::Mcp` connection after
/// `ShimRegisterAck { accepted: true }` has been written.
///
/// The function blocks until:
///   * the peer disconnects (rmcp's inner loop drains naturally); OR
///   * `shutdown` fires, in which case the rmcp service's own
///     cancellation token is tripped and the inner loop drains
///     cooperatively.
///
/// Shutdown is plumbed through a short forwarder task: it awaits
/// `shutdown.cancelled()` and then flips the rmcp cancellation token
/// we obtained from `RunningService::cancellation_token`. This avoids
/// the `tokio::select!` ownership problem (both
/// `RunningService::waiting` and `RunningService::cancel` consume
/// `self`), while still giving the daemon top-level a clean way to
/// preempt a parked `tools/list` loop.
///
/// # Errors
///
/// Propagates:
///   * rmcp initialisation errors (`Self::InitializeError`).
///   * `tokio::task::JoinError` from `service.waiting()` surfaces as
///     `anyhow::Error`.
pub async fn host_mcp_on_streams<R, W>(
    reader: R,
    writer: W,
    manager: Arc<WorkspaceManager>,
    workspace_builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    tool_timeout: Duration,
    daemon_version: &'static str,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    use rmcp::ServiceExt;

    let handler = DaemonMcpHandler::new(
        manager,
        workspace_builder,
        tool_executor,
        tool_timeout,
        daemon_version,
    );
    let service = handler.serve((reader, writer)).await?;

    // `RunningService::waiting` and `cancel` both consume `self`, so
    // we cannot `select!` on `waiting()` while also branching into
    // `cancel()` on the shutdown path. Instead, snapshot the rmcp
    // cancellation token (cheap `Arc` clone) and forward our shutdown
    // token into it through a detached task. When `shutdown` fires,
    // the forwarder cancels the rmcp service, which triggers the rmcp
    // inner loop to drain cleanly — `waiting()` returns the resulting
    // `QuitReason` and we map it to a unit success. Biased ordering is
    // unnecessary: `CancellationToken::cancelled()` wakes immediately
    // on the already-cancelled path, so the forwarder observes the
    // signal before any other task can race it.
    let service_ct = service.cancellation_token();
    let shutdown_fwd = shutdown.clone();
    // The forwarder + `service.waiting()` race is safe because
    // `CancellationToken::cancel()` is idempotent and `.abort()` on a
    // completed task is a no-op. Either:
    // - shutdown fires first → forwarder flips rmcp token →
    //   `waiting()` returns.
    // - peer disconnects first → `waiting()` returns →
    //   `forwarder.abort()` cancels it (if it has not already
    //   observed the cancellation independently).
    // - both fire nearly-simultaneously → idempotent `cancel()`, no
    //   double-cancel hazard.
    let forwarder = tokio::spawn(async move {
        shutdown_fwd.cancelled().await;
        service_ct.cancel();
    });

    // `waiting()` consumes the service; it returns when either the
    // peer disconnects OR the rmcp cancellation token we just linked
    // to fires.
    let wait_result = service.waiting().await;

    // Best-effort cleanup: if the peer disconnected before our
    // shutdown fired, the forwarder is still parked on
    // `shutdown.cancelled()` — abort it so we don't leak the task.
    forwarder.abort();

    wait_result.map(|_| ()).map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::builder::EmptyGraphBuilder;

    /// Test helper: build a workspace builder for synthetic handler tests.
    fn test_builder() -> Arc<dyn WorkspaceBuilder> {
        Arc::new(EmptyGraphBuilder)
    }

    #[test]
    fn extract_path_arg_returns_path_when_present() {
        let v = serde_json::json!({"path": "/tmp/ws", "other": 42});
        assert_eq!(extract_path_arg(&v), Some("/tmp/ws".into()));
    }

    #[test]
    fn extract_path_arg_returns_none_when_missing() {
        let v = serde_json::json!({"other": 42});
        assert_eq!(extract_path_arg(&v), None);
    }

    #[test]
    fn extract_path_arg_returns_none_when_not_string() {
        let v = serde_json::json!({"path": 42});
        assert_eq!(extract_path_arg(&v), None);
    }

    #[test]
    fn extract_path_arg_returns_none_on_non_object() {
        let v = serde_json::Value::Null;
        assert_eq!(extract_path_arg(&v), None);
        let v = serde_json::json!([1, 2, 3]);
        assert_eq!(extract_path_arg(&v), None);
    }

    #[test]
    fn daemon_sqry_ask_translator_config_preserves_model_and_trust_params() {
        let args = SqryAskParams {
            query: "find call sites".to_string(),
            path: ".".to_string(),
            execute: false,
            model_dir: Some("sqry-nl-models".to_string()),
            allow_unverified_model: true,
            allow_model_download: true,
        };

        let cfg = build_daemon_sqry_ask_translator_config(std::path::Path::new("workspace"), &args);

        assert_eq!(
            cfg.model_dir_override.as_deref(),
            Some(std::path::Path::new("sqry-nl-models")),
            "daemon sqry_ask must thread request model_dir into TranslatorConfig"
        );
        assert!(
            cfg.allow_unverified_model,
            "daemon sqry_ask must thread allow_unverified_model into TranslatorConfig"
        );
        assert!(
            cfg.allow_model_download,
            "daemon sqry_ask must thread allow_model_download into TranslatorConfig"
        );
        assert_eq!(
            cfg.working_directory.as_deref(),
            Some("workspace"),
            "daemon sqry_ask must scope TranslatorConfig to the canonical request path"
        );
    }

    #[test]
    fn get_info_advertises_daemon_identity_and_tool_capability() {
        // Synthetic handler: a manager with no workspaces + an empty
        // executor is sufficient because `get_info` never touches
        // either field. This protects the wire shape without the
        // weight of a real workspace bringup.
        use crate::config::DaemonConfig;

        let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            manager,
            test_builder(),
            executor,
            Duration::from_secs(60),
            "0.0.0-test",
        );

        let info = handler.get_info();
        assert_eq!(info.server_info.name, "sqry-daemon-mcp");
        assert_eq!(info.server_info.version, "0.0.0-test");
        assert!(info.capabilities.tools.is_some());
        assert!(
            info.instructions
                .as_deref()
                .unwrap_or_default()
                .contains("daemon-hosted"),
            "instructions must mention daemon-hosted mode"
        );
    }

    #[test]
    fn handler_tools_list_is_subset_of_daemon_supported_names() {
        use crate::config::DaemonConfig;

        let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            manager,
            test_builder(),
            executor,
            Duration::from_secs(60),
            "0.0.0-test",
        );

        // Every tool exposed by the handler must appear in the
        // authoritative `DAEMON_SUPPORTED_TOOL_NAMES` constant.
        // Feature flags may subset the list below 15, but must never
        // go outside of it.
        for tool in &handler.tools {
            assert!(
                tools_schema::DAEMON_SUPPORTED_TOOL_NAMES.contains(&tool.name.as_ref()),
                "tool {:?} must be in DAEMON_SUPPORTED_TOOL_NAMES",
                tool.name
            );
        }
    }

    /// Silent-desync guard: both the unknown-tool rejection path and
    /// the missing-`path` rejection path in [`DaemonMcpHandler::call_tool`]
    /// route through `daemon_err_to_mcp(DaemonError::InvalidArgument)`,
    /// so their MCP envelopes must share the canonical 4-key top-level
    /// shape (`kind`, `retryable`, `retry_after_ms`, `details`). Any
    /// future change to `daemon_err_to_mcp` that drifts one envelope
    /// relative to the other will fail this assertion — catching the
    /// regression before it reaches clients.
    #[test]
    fn unknown_tool_and_missing_path_envelopes_have_identical_top_level_keys() {
        use std::collections::BTreeSet;

        let err_unknown = daemon_err_to_mcp(DaemonError::InvalidArgument {
            reason: "unknown tool name bogus_tool: not in DAEMON_SUPPORTED_TOOL_NAMES".into(),
        });
        let err_missing = daemon_err_to_mcp(DaemonError::InvalidArgument {
            reason: "semantic_search: missing or non-string `path` argument".into(),
        });

        let keys_unknown: BTreeSet<String> = err_unknown
            .data
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let keys_missing: BTreeSet<String> = err_missing
            .data
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys_unknown, keys_missing,
            "unknown-tool and missing-path envelopes must share the \
             canonical 4-key top-level shape"
        );
        // Belt-and-suspenders: confirm the shared shape is the
        // documented 4 canonical keys (not some other drifted set).
        let expected: BTreeSet<String> = ["kind", "retryable", "retry_after_ms", "details"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(keys_unknown, expected);
    }

    // -------------------------------------------------------------
    // Codex iter-0 MAJOR-1 fix: feature-flag-disabled tools must be
    // rejected by `call_tool` so the advertised-vs-callable contract
    // holds. The three tests below pin the invariant at three layers:
    //
    //   1. The `with_tools` constructor builds `enabled_tool_names`
    //      from the supplied filtered list — NOT the raw constant.
    //   2. The advertised set and the authorization set are
    //      bit-identical (no drift between `list_tools` and `call_tool`).
    //   3. A name in `DAEMON_SUPPORTED_TOOL_NAMES` but absent from the
    //      supplied filtered list is correctly classified as
    //      "disabled by feature flags" rather than "unknown" — so the
    //      operator-facing error message tells them to flip the env
    //      var rather than questioning whether they typoed the name.
    // -------------------------------------------------------------

    #[test]
    fn with_tools_derives_enabled_set_from_filtered_list_not_constant() {
        use crate::config::DaemonConfig;

        let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let executor = Arc::new(QueryExecutor::new());

        // Build a synthetic 2-tool filtered subset (simulating
        // SQRY_MCP_ENABLE_GRAPH=false + SQRY_MCP_ENABLE_EXPORT=false +
        // SQRY_MCP_ENABLE_SEMANTIC_DIFF=false +
        // SQRY_MCP_ENABLE_DEPENDENCY_IMPACT=false) by taking just two
        // entries from the full daemon-supported list.
        let full = tools_schema::daemon_supported_tools();
        assert!(
            full.len() >= 2,
            "test prerequisite: default daemon_supported_tools must yield >= 2 tools"
        );
        let filtered: Vec<rmcp::model::Tool> = full
            .iter()
            .filter(|t| {
                let n: &str = t.name.as_ref();
                n == "semantic_search" || n == "find_unused"
            })
            .cloned()
            .collect();
        assert_eq!(filtered.len(), 2, "synthetic filter must yield exactly 2");

        let handler = DaemonMcpHandler::with_tools(
            manager,
            test_builder(),
            executor,
            Duration::from_secs(60),
            "0.0.0-test",
            filtered,
        );

        let enabled = handler.enabled_tool_names();
        assert_eq!(
            enabled.len(),
            2,
            "enabled_tool_names must equal the filtered list size, not 15"
        );
        assert!(enabled.contains("semantic_search"));
        assert!(enabled.contains("find_unused"));

        // Tools that exist in DAEMON_SUPPORTED_TOOL_NAMES but were
        // filtered out (e.g. `trace_path` is gated by
        // SQRY_MCP_ENABLE_GRAPH) MUST NOT appear in the enabled set.
        assert!(
            !enabled.contains("trace_path"),
            "trace_path is in DAEMON_SUPPORTED_TOOL_NAMES but was excluded \
             from the synthetic filter; enabled_tool_names must reflect the \
             filter, not the unfiltered constant"
        );
        assert!(
            !enabled.contains("export_graph"),
            "export_graph excluded from synthetic filter — enabled set must \
             not contain it"
        );
        assert!(
            !enabled.contains("semantic_diff"),
            "semantic_diff excluded from synthetic filter — enabled set must \
             not contain it"
        );
        assert!(
            !enabled.contains("dependency_impact"),
            "dependency_impact excluded from synthetic filter — enabled set \
             must not contain it"
        );
    }

    #[test]
    fn advertised_and_enabled_sets_are_bit_identical() {
        use crate::config::DaemonConfig;

        let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            manager,
            test_builder(),
            executor,
            Duration::from_secs(60),
            "0.0.0-test",
        );

        let advertised: HashSet<String> = handler
            .advertised_tools()
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        let enabled = handler.enabled_tool_names();

        assert_eq!(
            &advertised, enabled,
            "list_tools advertised set and call_tool authorization set MUST be bit-identical \
             — any divergence breaks the advertised-vs-callable contract (Codex iter-0 MAJOR-1)"
        );
    }

    /// Phase-level invariant: a tool that sits in the global daemon
    /// catalogue (`DAEMON_SUPPORTED_TOOL_NAMES`) but is gated off by
    /// the active feature flags must produce a "disabled by feature
    /// flags" error message rather than an "unknown tool" message.
    /// This lets operators distinguish typos from configuration issues.
    #[test]
    fn disabled_tool_rejection_distinguishes_disabled_from_unknown() {
        use crate::config::DaemonConfig;

        let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let executor = Arc::new(QueryExecutor::new());

        // Build a handler with a single-tool whitelist so every other
        // catalogue entry is "disabled by feature flags" from the
        // handler's POV.
        let full = tools_schema::daemon_supported_tools();
        let only_semantic_search: Vec<rmcp::model::Tool> = full
            .iter()
            .filter(|t| {
                let n: &str = t.name.as_ref();
                n == "semantic_search"
            })
            .cloned()
            .collect();
        assert_eq!(only_semantic_search.len(), 1);

        let handler = DaemonMcpHandler::with_tools(
            manager,
            test_builder(),
            executor,
            Duration::from_secs(60),
            "0.0.0-test",
            only_semantic_search,
        );

        // Sanity: the enabled set is correctly the singleton.
        assert_eq!(handler.enabled_tool_names().len(), 1);
        assert!(handler.enabled_tool_names().contains("semantic_search"));

        // The disabled-vs-unknown branch in `call_tool` is purely a
        // function of the handler state + the input name; we can
        // reproduce its decision tree without spinning up an async
        // runtime by replicating the predicate it uses.
        let disabled_name = "trace_path"; // in DAEMON_SUPPORTED_TOOL_NAMES, not enabled
        let unknown_name = "this_tool_does_not_exist_anywhere"; // truly unknown

        assert!(
            !handler.enabled_tool_names().contains(disabled_name),
            "trace_path must be classified as disabled, not enabled"
        );
        assert!(
            tools_schema::DAEMON_SUPPORTED_TOOL_NAMES.contains(&disabled_name),
            "trace_path must remain in DAEMON_SUPPORTED_TOOL_NAMES — if not, \
             this test must be updated to pick a different gated tool"
        );
        assert!(
            !handler.enabled_tool_names().contains(unknown_name),
            "synthetic unknown name must not be in the enabled set"
        );
        assert!(
            !tools_schema::DAEMON_SUPPORTED_TOOL_NAMES.contains(&unknown_name),
            "synthetic unknown name must not be in DAEMON_SUPPORTED_TOOL_NAMES"
        );
    }
}
