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
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::Value;
use sqry_core::project::ProjectRootMode;
use sqry_core::query::executor::QueryExecutor;
use sqry_mcp::daemon_adapter::WorkspaceContext;
use sqry_mcp::daemon_adapter::dispatch::dispatch_by_name;
use sqry_mcp::tools_schema;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::error::DaemonError;
use crate::ipc::tool_core::{self, ExecuteVerdict};
use crate::workspace::{WorkspaceBuilder, WorkspaceKey, WorkspaceManager};
use error_map::{daemon_err_to_mcp, daemon_err_to_mcp_with_tool};

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
    /// issue #503 Phase 2: shared dedicated CPU executor so daemon-hosted MCP
    /// tool work runs on the same num_cpus Rayon pool as the JSON-RPC path
    /// (one fairness domain), not the global pool. `Arc`-backed clone of the
    /// pool created in `IpcServer::bind`.
    cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
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
        cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
        tool_timeout: Duration,
        daemon_version: &'static str,
    ) -> Self {
        Self::with_tools(
            manager,
            workspace_builder,
            tool_executor,
            cpu_executor,
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
        cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
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
            cpu_executor,
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
    /// dispatch wrapper. The `rebuild_index` flow remains outside this
    /// code path — it owns its own load semantics.
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
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(Implementation::new(
                "sqry-daemon-mcp",
                self.daemon_version.to_owned(),
            ))
            .with_instructions(
                "sqry MCP server (daemon-hosted). Tool calls are served from \
                 the daemon's preloaded workspace state — same behaviour as \
                 sqry-mcp's standalone mode, zero graph rebuild cost.",
            )
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
        // `A_cancellation.md` §2 + `00_contracts.md` §3.CC-1: the
        // daemon's `tool_core::execute_with_timeout` now hands the
        // closure a borrowed `&CancellationToken` so deadline-driven
        // cancellation flows into `dispatch_by_name`. The dispatcher
        // itself only routes the token through tools whose inner body
        // uses the executor's `*_cancellable` overloads (today:
        // `semantic_search`); other tools are tracked under IMP-A's
        // deferred follow-up and silently ignore the token until then.
        let run = move |wctx: &WorkspaceContext,
                        cancel: &sqry_core::query::cancellation::CancellationToken|
              -> anyhow::Result<Value> {
            dispatch_by_name(&name_clone, wctx, &args_clone, cancel)
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
            &self.cpu_executor,
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
        Ok(call_tool_result_with_text_and_structured(
            text_payload,
            payload,
        ))
    }
}

impl DaemonMcpHandler {
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

/// Extract the `path` argument from tool args. Only valid for the 15
/// daemon-supported tool types — do NOT extend to the full 34-tool
/// standalone inventory without auditing which tool types carry a
/// `path` field.
fn extract_path_arg(args: &Value) -> Option<String> {
    args.as_object()?.get("path")?.as_str().map(String::from)
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
    Ok(call_tool_result_with_text_and_structured(
        text_payload,
        payload,
    ))
}

fn call_tool_result_with_text_and_structured(
    text_payload: String,
    payload: Value,
) -> CallToolResult {
    let mut result = CallToolResult::structured(payload);
    debug_assert_eq!(result.is_error, Some(false));
    // Preserve the pre-rmcp-1.6 daemon wire shape: successful daemon tool
    // calls omit `isError` and carry pretty JSON in the text content.
    result.content = vec![Content::text(text_payload)];
    result.is_error = None;
    result
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
// Wiring entrypoint: forwards the daemon's shared dependencies (manager,
// builder, executors, timeout, shutdown) into the MCP handler. issue #503
// Phase 2 adds `cpu_executor`, taking the count to 9.
#[allow(clippy::too_many_arguments)]
pub async fn host_mcp_on_streams<R, W>(
    reader: R,
    writer: W,
    manager: Arc<WorkspaceManager>,
    workspace_builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
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
        cpu_executor,
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
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
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
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
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
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
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
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
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
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
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
