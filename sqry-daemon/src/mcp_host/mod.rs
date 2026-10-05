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
pub mod redaction;

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
use sqry_mcp::error::{RpcError, rpc_error_to_mcp};
use sqry_mcp::tools_schema;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::error::DaemonError;
use crate::ipc::tool_core::{self, ExecuteVerdict, PLUGIN_SELECTION_WARNING_KEY};
use crate::rebuild::RebuildDispatcher;
use crate::workspace::loaded::PublishedGraph;
use crate::workspace::{
    WorkspaceBuilder, WorkspaceKey, WorkspaceManager, WorkspaceState, plugin_selection_warning_for,
};
use error_map::{
    daemon_err_to_mcp, daemon_err_to_mcp_for_rebuild_index, daemon_err_to_mcp_with_tool,
};

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
    /// The daemon's rebuild dispatcher: `rebuild_index` over a resident
    /// workspace rebuilds it in place through it, as `daemon/rebuild` does,
    /// so the old generation keeps serving (and its file watcher keeps
    /// watching) until the new one is published.
    dispatcher: Arc<RebuildDispatcher>,
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
    /// How every tool response and error is redacted before it is sent
    /// (decision D-i7-5): the daemon's MCP redaction preset, bound per
    /// request to the workspace the request names.
    redaction: Arc<redaction::McpRedaction>,
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
    /// the active feature flags disable one of the 17 daemon-supported
    /// tools, that tool is hidden from `tools/list` AND rejected by
    /// `call_tool` with `InvalidArgument`, matching the standalone
    /// `SqryServer` contract.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manager: Arc<WorkspaceManager>,
        dispatcher: Arc<RebuildDispatcher>,
        workspace_builder: Arc<dyn WorkspaceBuilder>,
        tool_executor: Arc<QueryExecutor>,
        cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
        tool_timeout: Duration,
        daemon_version: &'static str,
        redaction: Arc<redaction::McpRedaction>,
    ) -> Self {
        Self::with_tools(
            manager,
            dispatcher,
            workspace_builder,
            tool_executor,
            cpu_executor,
            tool_timeout,
            daemon_version,
            tools_schema::daemon_supported_tools(),
            redaction,
        )
    }

    /// Build a handler with an explicit pre-filtered tool list. Used by
    /// the M-1 fix unit tests to inject a synthetic feature-flag set
    /// without process-wide env-var manipulation. The advertised set
    /// (`tools/list`) and the authorization set (`call_tool`) are both
    /// derived from the supplied `tools` vec, so they remain in lockstep
    /// regardless of how the caller filtered them.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_tools(
        manager: Arc<WorkspaceManager>,
        dispatcher: Arc<RebuildDispatcher>,
        workspace_builder: Arc<dyn WorkspaceBuilder>,
        tool_executor: Arc<QueryExecutor>,
        cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
        tool_timeout: Duration,
        daemon_version: &'static str,
        tools: Vec<rmcp::model::Tool>,
        redaction: Arc<redaction::McpRedaction>,
    ) -> Self {
        let enabled_tool_names: HashSet<String> =
            tools.iter().map(|t| t.name.as_ref().to_owned()).collect();
        Self {
            manager,
            dispatcher,
            workspace_builder,
            tool_executor,
            cpu_executor,
            tool_timeout,
            daemon_version,
            tools,
            enabled_tool_names,
            redaction,
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
                 the daemon's preloaded workspace state at zero rebuild cost. \
                 The plugin roster for each workspace is resolved from its \
                 .sqry/graph/manifest.json (fast-path default when no index \
                 exists); a resident graph narrower or wider than the manifest \
                 is reported in every tool response under \
                 plugin_selection_warning. This surface exposes the \
                 daemon-hosted tool subset.",
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
        // Every response and error is redacted as the standalone server
        // redacts it (decision D-i7-5), with the redactor bound to the
        // workspace the request names; a request that names none (an
        // unknown tool, a missing or unresolvable path) is redacted
        // unbound, which still redacts every absolute path.
        let root = self.redaction_root(&name, &args_value);
        let outcome = self.call_tool_unredacted(name, args_value).await;
        self.redaction.redact_outcome(root.as_deref(), outcome)
    }
}

impl DaemonMcpHandler {
    /// The workspace a request names, for binding its redactor: for
    /// `rebuild_index` the directory it rebuilds (a file's parent), for
    /// every other tool the registered workspace that owns the path, or
    /// the path itself when none does. `None` when the path is missing,
    /// relative or does not resolve.
    fn redaction_root(&self, name: &str, args_value: &Value) -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(extract_path_arg(args_value)?.as_str()).to_path_buf();
        if !path.is_absolute() {
            return None;
        }
        if name == "rebuild_index" {
            let canonical = std::fs::canonicalize(&path).ok()?;
            return if canonical.is_dir() {
                Some(canonical)
            } else {
                canonical.parent().map(std::path::Path::to_path_buf)
            };
        }
        let canonical = tool_core::resolve_path_for_acquisition(&path).ok()?;
        Some(
            self.manager
                .find_owning_workspace_root(&canonical)
                .unwrap_or(canonical),
        )
    }

    /// One tool call, before redaction.
    async fn call_tool_unredacted(
        &self,
        name: String,
        args_value: Value,
    ) -> Result<CallToolResult, McpError> {
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
        // dedicated path BEFORE the generic `path`-argument check below.
        //
        // #566: standalone `RebuildIndexParams::path` defaults to `"."`
        // (see `default_path()` at `sqry-mcp/src/tools/params.rs:1587`),
        // which standalone resolves against the client's launch directory
        // via the MCP `roots/list` callback. The daemon has no such client
        // directory, so an omitted or relative `path` cannot be resolved
        // correctly (it would canonicalize against the daemon's own CWD,
        // silently targeting the wrong workspace). Require an explicit
        // absolute path instead: omitted `path` is rejected here, and a
        // present-but-relative `path` is rejected in `handle_rebuild_index`.
        //
        // Strictness: if `path` is PRESENT but not a string, fail with
        // `InvalidArgument`, matching standalone serde rejection of
        // `{"path": 42}` shaped requests.
        if name == "rebuild_index" {
            let path = match args_value.as_object().and_then(|m| m.get("path")) {
                Some(raw) => raw.as_str().map(String::from).ok_or_else(|| {
                    daemon_err_to_mcp(DaemonError::InvalidArgument {
                        reason: format!("rebuild_index: `path` must be a string, got: {raw}"),
                    })
                })?,
                None => {
                    return Err(daemon_err_to_mcp(DaemonError::InvalidArgument {
                        reason: "rebuild_index: workspace `path` is required in daemon mode; \
                                 the daemon has no client working directory to default to. \
                                 Pass an absolute path."
                            .to_string(),
                    }));
                }
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
            &self.dispatcher,
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
    ///   node / edge / file counts, and an "already exists" message:
    ///   no fresh build, no bogus `builtAt = now()`.
    /// - When a graph is resident and `force=false` (a workspace
    ///   `daemon/load` built in memory, with no index on disk), answer
    ///   the same way from the resident generation: nothing is built.
    /// - Macro options (`cfg_flags`, `expand_cache`, `reset_macro_options`)
    ///   with `force=false` are refused whenever a graph exists, on disk or
    ///   resident, because no build would honour them.
    /// - With `force=true` over a resident workspace, rebuild it in place
    ///   through the [`RebuildDispatcher`], as `daemon/rebuild` does, under
    ///   the key the workspace is registered under: the inputs are resolved
    ///   before anything is reserved, the old generation keeps serving until
    ///   the new one is published, a refusal or a failure leaves it resident
    ///   in the state it was in, the file watcher keeps watching, and the
    ///   durable persist records the roster and the macro options. The
    ///   answer is the generation the caller's own iteration published,
    ///   waited for at most [`RebuildDispatcher::outcome_wait`].
    /// - Otherwise (no resident graph), refuse a nested index without
    ///   `force` as the standalone server does, then load the workspace
    ///   through `WorkspaceManager::load_published` with a builder that
    ///   builds and persists the index the same way
    ///   ([`WorkspaceBuilder::prepare_durable_build`], decision D-i7-2),
    ///   start its file watcher, and answer with the generation it
    ///   published. When another load published the workspace first (S2,
    ///   round 7), nothing was built with this call's options, so the call
    ///   is answered as over a resident graph: a rebuild in place with
    ///   `force`, the existing graph (or the need-force refusal) without.
    ///
    /// The arguments are parsed through the shared `RebuildIndexParams` and
    /// the macro request is checked before anything is loaded or queued.
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

        // The arguments are read through the standalone server's own type
        // (`sqry_mcp::daemon_params::parse_rebuild_index_params` over
        // `RebuildIndexParams`), the type whose schema both hosts
        // advertise: an unknown field (a misspelled `cfg_flag`) or a wrong
        // type is refused (`-32602`) rather than dropped or coerced, and
        // `force` defaults to `true`. `path` was checked present and a
        // string by the caller; the shared type's `"."` default never
        // applies here.
        let params =
            sqry_mcp::daemon_params::parse_rebuild_index_params(args_value).map_err(|err| {
                daemon_err_to_mcp(DaemonError::InvalidArgument {
                    reason: format!("rebuild_index: {err}"),
                })
            })?;
        let force = params.force;
        let macro_request = macro_request_of(&params);

        // #566: reject a relative `path` before canonicalizing. `rebuild_index`
        // bypasses `resolve_path` (it drives `WorkspaceManager::get_or_load`
        // directly, per the mutating-rebuild contract), so it needs its own
        // absolute-path guard: `std::fs::canonicalize` on a relative path
        // resolves it against the daemon's process CWD ($HOME), silently
        // targeting the wrong workspace. The daemon has no client working
        // directory to resolve a relative path against.
        if !std::path::Path::new(path).is_absolute() {
            return Err(daemon_err_to_mcp(DaemonError::InvalidArgument {
                reason: format!(
                    "rebuild_index: workspace `path` must be absolute in daemon mode; received \
                     relative path {path:?}. The daemon has no client working directory to \
                     resolve it against. Pass an absolute path."
                ),
            }));
        }

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

        let key = WorkspaceKey::new(canonical_root.clone(), ProjectRootMode::default(), 0);

        // Cache-hit path: an index on disk and `!force` reports the
        // existing manifest, so the daemon never claims a fresh
        // `builtAt = now()` for an unchanged index. When the workspace is
        // resident, the envelope still reports its roster verdict and graph
        // metadata.
        //
        // Surface parity W1 round 3 (design D17): whenever a manifest is on
        // disk and `force` is not set, it is classified through the refusing
        // resolver first, so an id this binary did not compile is refused by
        // name and an unreadable manifest by file (the same refusals
        // `daemon/load` gives), whether or not the workspace is resident:
        // without `force` nothing replaces a manifest it cannot read. The
        // record is dropped: this leg builds nothing.
        //
        // An index is its manifest and its snapshot: a manifest whose
        // snapshot is gone is no index a load could read (the read paths
        // refuse it as not indexed), so it is not reported as one; the call
        // goes on to build it, keeping the recorded selection
        // (DAEMON_FOLLOWUP, round 7).
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&canonical_root);
        if storage.exists() && !force {
            let snapshot_present = storage.snapshot_exists();
            // Surface parity W4 (W4-D8): a macro option given beside
            // `force=false` over an existing index reaches no build, so it
            // is refused instead of being dropped, as standalone refuses it.
            if snapshot_present && !macro_request.is_empty() {
                return Err(rpc_error_to_mcp(RpcError::macro_options_need_force(
                    &canonical_root,
                )));
            }
            self.workspace_builder
                .roster_for(&canonical_root)
                .map_err(|err| daemon_err_to_mcp_for_rebuild_index(err, path))?;
            if snapshot_present {
                let (plugin_selection_warning, graph_metadata) =
                    self.resident_roster_context(&key, &canonical_root, None);
                return build_rebuild_index_cache_hit_response(
                    &canonical_root,
                    &root_display,
                    &storage,
                    start,
                    plugin_selection_warning,
                    graph_metadata,
                );
            }
            // A manifest without its snapshot answers no cache hit: the
            // build below runs and replaces the manifest. Without `force`
            // that is allowed only over a manifest this call can read, so
            // an unreadable one is refused here, whatever the builder's
            // roster resolution reads.
            storage.load_manifest().map_err(|err| {
                daemon_err_to_mcp_with_tool(
                    DaemonError::WorkspaceManifestUnreadable {
                        root: canonical_root.clone(),
                        manifest_path: storage.manifest_path().to_path_buf(),
                        reason: err.to_string(),
                    },
                    "rebuild_index",
                )
            })?;
        }

        // Fresh / force-rebuild path.

        // A generation is resident (a published graph with its record, in a
        // serving state): `daemon/load` may have built it in memory with no
        // index on disk.
        let resident = self
            .manager
            .resident_snapshot(&key)
            .filter(|(state, published)| state.is_serving() && published.roster.is_some())
            .map(|(_, published)| published);

        // When the time the answer reports is not now (an answer from a
        // resident generation that built nothing), it is that generation's.
        let mut built_at = chrono::Utc::now();
        let (published, message) = match resident {
            Some(resident) if !force => self.answer_from_an_existing_graph(
                &key,
                &canonical_root,
                resident,
                &macro_request,
                &mut built_at,
            )?,
            Some(_) => {
                // F6: rebuild the resident workspace in place, through the
                // dispatcher `daemon/rebuild` uses. The old generation keeps
                // serving until the new one is published; a refusal or a
                // failure leaves it resident; the watcher keeps watching.
                // The request is checked first (S6, round 7), so a refused
                // one never reaches the lane.
                crate::rebuild::validate_macro_request(&canonical_root, &macro_request)
                    .map_err(|err| daemon_err_to_mcp_for_rebuild_index(err, path))?;
                let report = self
                    .rebuild_resident(&key, &canonical_root, path, macro_request)
                    .await?;
                (report.published, "Index rebuilt successfully.".to_string())
            }
            None => {
                // No resident generation: load the workspace with a builder
                // that builds and persists the index (decision D-i7-2), so
                // the options are recorded as `daemon/rebuild` records them.
                //
                // With no index at the root this call would create one, so
                // it is refused, as the standalone server refuses it, when
                // an ancestor in the same project already has one and the
                // caller did not opt in with `force` (cluster E, design E.3;
                // S5, round 7). Then the request is checked (S6, round 7).
                // Both run before the loader registers the workspace. Every
                // later refusal (the builder's preparation, the narrowing
                // guard, the budget) and every failure of the durable build
                // is put back by the loader as the gate found it
                // (`RebuildIndexBuilder::failed_load_leaves_no_slot`, B2 of
                // the round 7 audit), so a refused or failed call leaves no
                // `Failed` entry that would break later queries.
                if !storage.exists() {
                    refuse_a_nested_index(&canonical_root, force)?;
                }
                crate::rebuild::validate_macro_request(&canonical_root, &macro_request)
                    .map_err(|err| daemon_err_to_mcp_for_rebuild_index(err, path))?;
                let manager = Arc::clone(&self.manager);
                let builder: Arc<dyn WorkspaceBuilder> = Arc::new(RebuildIndexBuilder {
                    inner: Arc::clone(&self.workspace_builder),
                    macro_request: macro_request.clone(),
                });
                let key_for_task = key.clone();
                let working_set_estimate = initial_working_set_estimate();
                // Surface parity W1 round 4 (design D20): the loader hands
                // back the generation it published (graph and record as one
                // value), and the envelope below describes exactly that
                // value. Nothing re-reads the slot, which a second publisher
                // (the publish hook's own work, a rebuild) could have
                // advanced between the load and the answer.
                let (published, origin) = tokio::task::spawn_blocking(move || {
                    manager.load_published(&key_for_task, &*builder, working_set_estimate)
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
                .map_err(|e| daemon_err_to_mcp_for_rebuild_index(e, path))?;
                match origin {
                    crate::workspace::LoadOrigin::Built => {
                        // A workspace this call made resident is watched,
                        // as one `daemon/load` made resident is: edits
                        // rebuild it, and the next `rebuild_index` finds it
                        // resident and rebuilds it in place.
                        self.dispatcher.start_watching(&key);
                        let message = if force {
                            "Index rebuilt successfully."
                        } else {
                            "Index built successfully."
                        };
                        (published, message.to_string())
                    }
                    // S2 (round 7): another load published the workspace
                    // between this call's resident check and its load gate,
                    // with that caller's builder, so the generation was not
                    // built with this call's options and must not be
                    // reported as built. The call is answered as it would
                    // have been had it found the generation resident: with
                    // `force`, a rebuild in place with its own options;
                    // without, the existing graph (or the need-force
                    // refusal of its options).
                    crate::workspace::LoadOrigin::Found if !force => self
                        .answer_from_an_existing_graph(
                            &key,
                            &canonical_root,
                            published,
                            &macro_request,
                            &mut built_at,
                        )?,
                    crate::workspace::LoadOrigin::Found => {
                        let report = self
                            .rebuild_resident(&key, &canonical_root, path, macro_request)
                            .await?;
                        (report.published, "Index rebuilt successfully.".to_string())
                    }
                }
            }
        };

        // Surface parity W1 (S8): the mutating tool's envelope is not the
        // one exception. The record published beside the graph is compared
        // against the manifest the same way the read-only path compares it,
        // and `graph_metadata` is filled from the resident graph through the
        // constructor the standalone tools use instead of `None`.
        let (plugin_selection_warning, graph_metadata) =
            self.resident_roster_context(&key, &canonical_root, Some(&published));

        #[allow(clippy::cast_possible_truncation)]
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let node_count = published.graph.node_count() as u64;
        let edge_count = published.graph.edge_count() as u64;
        let files_indexed = published.graph.indexed_files().count() as u64;

        let data = RebuildIndexData {
            success: true,
            root_path: root_display.clone(),
            node_count,
            edge_count,
            files_indexed,
            built_at: built_at.to_rfc3339(),
            message: Some(message),
        };

        let execution = ToolExecution {
            data,
            used_index: false,
            used_graph: true,
            graph_metadata,
            execution_ms: elapsed_ms,
            next_page_token: None,
            total: Some(1),
            truncated: Some(false),
            candidates_scanned: None,
            workspace_path: root_display,
        };

        finalize_rebuild_index_response(execution, plugin_selection_warning)
    }

    /// The answer to `rebuild_index force=false` over a graph that already
    /// exists in the daemon (F3): nothing is built. Macro options would
    /// reach no build, so they are refused with the shared need-force
    /// refusal, as on the cache-hit leg; otherwise the answer is "Index
    /// already exists" with that generation, and `built_at` becomes its
    /// last successful build time.
    fn answer_from_an_existing_graph(
        &self,
        key: &WorkspaceKey,
        root: &std::path::Path,
        existing: Arc<PublishedGraph>,
        macro_request: &sqry_core::graph::unified::build::MacroOptionsRequest,
        built_at: &mut chrono::DateTime<chrono::Utc>,
    ) -> Result<(Arc<PublishedGraph>, String), McpError> {
        if !macro_request.is_empty() {
            return Err(rpc_error_to_mcp(RpcError::macro_options_need_force(root)));
        }
        if let Some(last_good_at) = self
            .manager
            .lookup(key)
            .and_then(|ws| *ws.last_good_at.read())
        {
            *built_at = chrono::DateTime::<chrono::Utc>::from(last_good_at);
        }
        Ok((
            existing,
            "Index already exists. Use force=true to rebuild.".to_string(),
        ))
    }

    /// Rebuild a resident workspace in place for `rebuild_index`: a forced
    /// rebuild through the [`RebuildDispatcher`] with this call's macro
    /// request, answered with the report of the iteration that consumed it
    /// (F6). The dispatcher resolves the roster, the macro options and the
    /// narrowing guard before it reserves memory, publishes into the
    /// resident workspace only once the durable persist committed (which
    /// records the roster and the options), and leaves the workspace
    /// resident on a refusal or a failure.
    ///
    /// # Errors
    ///
    /// The iteration's refusal or failure through
    /// [`daemon_err_to_mcp_for_rebuild_index`] (an unusable recorded expand
    /// cache echoes `requested_path`, the request's own `path`); a request whose macro options a
    /// queued rebuild does not share is refused (`InvalidArgument`);
    /// `RebuildOutcomeTimeout` (the rebuild continues; not retryable) when
    /// the outcome does not arrive within
    /// [`RebuildDispatcher::outcome_wait`] (the rebuild continues).
    async fn rebuild_resident(
        &self,
        key: &WorkspaceKey,
        root: &std::path::Path,
        requested_path: &str,
        macro_request: sqry_core::graph::unified::build::MacroOptionsRequest,
    ) -> Result<crate::rebuild::RebuildReport, McpError> {
        let forced = sqry_core::watch::ChangeSet {
            changed_files: Vec::new(),
            git_state_changed: true,
            git_change_class: Some(sqry_core::watch::GitChangeClass::TreeDiverged),
        };
        let bound = self.dispatcher.outcome_wait();
        let outcome = self
            .dispatcher
            .handle_changes_for_rebuild_index(key, forced, macro_request)
            .await;
        match tokio::time::timeout(bound, outcome.wait()).await {
            Ok(own) => own.map_err(|err| daemon_err_to_mcp_for_rebuild_index(err, requested_path)),
            Err(_elapsed) => Err(daemon_err_to_mcp_for_rebuild_index(
                DaemonError::RebuildOutcomeTimeout {
                    root: root.to_path_buf(),
                    secs: bound.as_secs(),
                    deadline_ms: u64::try_from(bound.as_millis()).unwrap_or(u64::MAX),
                },
                requested_path,
            )),
        }
    }

    /// The `plugin_selection_warning` value and `graph_metadata` block for
    /// the generation the tool answers with (surface parity W1, S8; round
    /// 4, design D20).
    ///
    /// `Some(published)` is the generation the caller just obtained from
    /// `get_or_load_published`: the warning is computed from its record and
    /// the metadata from its graph, with no read of the workspace slot and
    /// no state check, because the pair in hand is the one the envelope's
    /// counts are read from. `None` is the cache-hit leg: one observation
    /// of the resident workspace, its state and its published generation
    /// under one guard (`WorkspaceManager::resident_snapshot`, design D25;
    /// one load of the generation, design D14), the record compared and
    /// the graph described from that one observation, the metadata only
    /// while the observed state is `Loaded`. The pre-round-5 leg loaded the
    /// generation through `lookup` and then read the state with no guard,
    /// so an eviction completing between the two could answer with a
    /// warning computed from a record whose graph was no longer resident
    /// beside a null `graph_metadata`, or describe the placeholder. A
    /// workspace that is not resident, or holds the placeholder generation
    /// after eviction (`roster: None`), yields `(None, None)`: there is no
    /// record to compare and no graph to describe, which is the contract
    /// (design D20, Q8; T45 and VR16).
    fn resident_roster_context(
        &self,
        key: &WorkspaceKey,
        root: &std::path::Path,
        published: Option<&Arc<PublishedGraph>>,
    ) -> (Option<Value>, Option<sqry_mcp::execution::GraphMetadata>) {
        let load_roster = self.workspace_builder.load_roster();
        if let Some(published) = published {
            let warning = published
                .roster
                .as_ref()
                .and_then(|record| plugin_selection_warning_for(root, record, &load_roster));
            let graph_metadata = Some(sqry_mcp::daemon_adapter::graph_metadata_for_resident_graph(
                root,
                &published.graph,
            ));
            return (warning, graph_metadata);
        }
        // One observation (design D25): the state and the generation are
        // read under one guard, so the record compared and the graph
        // described are the same publish, and the state that gates the
        // metadata is the state that generation was observed under.
        let Some((state, published)) = self.manager.resident_snapshot(key) else {
            return (None, None);
        };
        let warning = published
            .roster
            .as_ref()
            .and_then(|record| plugin_selection_warning_for(root, record, &load_roster));
        let graph_metadata = (state == WorkspaceState::Loaded).then(|| {
            sqry_mcp::daemon_adapter::graph_metadata_for_resident_graph(root, &published.graph)
        });
        (warning, graph_metadata)
    }
}

/// The macro build options request a daemon-hosted `rebuild_index` call
/// expresses (surface parity W4, W4-D8): `cfg_flags`, `expand_cache` and
/// `reset_macro_options` as the shared `RebuildIndexParams` parsed them
/// (an absent component keeps the manifest's record; serde already took
/// `null` as absent for the two optional fields and refused it for the
/// boolean, as the standalone server does). The request is not checked
/// here: `MacroOptionsRequest::validate` runs before anything is built.
fn macro_request_of(
    params: &sqry_mcp::daemon_params::RebuildIndexParams,
) -> sqry_core::graph::unified::build::MacroOptionsRequest {
    sqry_core::graph::unified::build::MacroOptionsRequest {
        cfg_flags: params.cfg_flags.clone(),
        expand_cache_dir: params.expand_cache.as_ref().map(std::path::PathBuf::from),
        reset: params.reset_macro_options,
    }
}

/// Refuse to create an index at `root` when an ancestor in the same
/// project already has one and `force` (the MCP opt-in to a nested index,
/// cluster E, design E.3) is not set: the standalone server's refusal,
/// through its constructor
/// ([`sqry_mcp::error::RpcError::nested_index_refused`]), so both hosts
/// answer it alike. Nothing was built.
fn refuse_a_nested_index(root: &std::path::Path, force: bool) -> Result<(), McpError> {
    match sqry_core::workspace::assert_no_ancestor_graph(root, force) {
        Ok(()) => Ok(()),
        Err(sqry_core::workspace::NestedIndexError::AncestorExists {
            requested,
            ancestor_graph,
            boundary,
        }) => Err(rpc_error_to_mcp(RpcError::nested_index_refused(
            &requested,
            &ancestor_graph,
            &boundary,
        ))),
    }
}

/// The builder the daemon-hosted `rebuild_index` loads a workspace that is
/// not resident with: it carries one call's macro build options request
/// into the loader and builds through the inner builder's durable
/// preparation ([`WorkspaceBuilder::prepare_durable_build`]), so the index
/// is persisted with the roster and the options it was built with, as
/// `daemon/rebuild` and the standalone `rebuild_index` persist it (decision
/// D-i7-2).
///
/// The loader calls only [`WorkspaceBuilder::prepare_build`], which this
/// builder overrides with the inner builder's durable preparation for this
/// call's request; `build` runs that preparation, and
/// `build_with_macro_options` is the inner builder's durable preparation
/// for the request it is given. `load_persisted`, `build_virtual_source`,
/// `roster_for` and `load_roster` are the inner builder's; the other
/// `prepare_*` methods are the trait's defaults over those.
#[derive(Debug)]
struct RebuildIndexBuilder {
    inner: Arc<dyn WorkspaceBuilder>,
    macro_request: sqry_core::graph::unified::build::MacroOptionsRequest,
}

impl WorkspaceBuilder for RebuildIndexBuilder {
    fn build(
        &self,
        workspace_root: &std::path::Path,
    ) -> Result<crate::workspace::BuiltGraph, DaemonError> {
        self.prepare_build(workspace_root)?()
    }

    fn build_with_macro_options(
        &self,
        workspace_root: &std::path::Path,
        macro_request: &sqry_core::graph::unified::build::MacroOptionsRequest,
    ) -> Result<crate::workspace::BuiltGraph, DaemonError> {
        self.inner
            .prepare_durable_build(workspace_root, macro_request)?()
    }

    fn load_persisted(
        &self,
        workspace_root: &std::path::Path,
    ) -> Result<crate::workspace::BuiltGraph, DaemonError> {
        self.inner.load_persisted(workspace_root)
    }

    fn build_virtual_source(
        &self,
        source: &dyn crate::workspace::revision::VirtualSourceReader,
        selection_root: &std::path::Path,
    ) -> Result<crate::workspace::BuiltGraph, DaemonError> {
        self.inner.build_virtual_source(source, selection_root)
    }

    fn roster_for(
        &self,
        selection_root: &std::path::Path,
    ) -> Result<Arc<crate::workspace::RosterRecord>, DaemonError> {
        self.inner.roster_for(selection_root)
    }

    fn load_roster(&self) -> Arc<sqry_core::plugin::PluginManager> {
        self.inner.load_roster()
    }

    fn prepare_build<'a>(
        &'a self,
        workspace_root: &std::path::Path,
    ) -> Result<crate::workspace::builder::PreparedBuild<'a>, DaemonError> {
        self.inner
            .prepare_durable_build(workspace_root, &self.macro_request)
    }

    /// The durable build's refusals write nothing and its failures put the
    /// old pair back, so a failed load through this builder is the
    /// caller's own and leaves no `Failed` slot (B2, round 7 audit).
    fn failed_load_leaves_no_slot(&self) -> bool {
        true
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
    plugin_selection_warning: Option<Value>,
    graph_metadata: Option<sqry_mcp::execution::GraphMetadata>,
) -> Result<CallToolResult, McpError> {
    use sqry_core::graph::unified::persistence::load_header_from_path;
    use sqry_mcp::execution::{RebuildIndexData, ToolExecution};

    // Surface parity W1 round 3 (design D15, D17): after the caller has
    // classified the manifest through the resolver, this arm is reached
    // only if the file became unreadable between the resolve and this
    // read. It is the defence, and it answers with the same typed refusal
    // (`-32001`, `manifest_path`, `repair_command`) as every other daemon
    // path, never with a generic build failure.
    let manifest = storage.load_manifest().map_err(|e| {
        daemon_err_to_mcp_with_tool(
            DaemonError::WorkspaceManifestUnreadable {
                root: canonical_root.to_path_buf(),
                manifest_path: storage.manifest_path().to_path_buf(),
                reason: e.to_string(),
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
        graph_metadata,
        execution_ms: elapsed_ms,
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: root_display.to_string(),
    };

    finalize_rebuild_index_response(execution, plugin_selection_warning)
}

/// Render a `ToolExecution<RebuildIndexData>` through the shared
/// `sqry-mcp` response builder so the daemon-hosted MCP wire envelope
/// matches the standalone transport byte-for-byte. Both `content[0].text`
/// and `structured_content` carry the same payload — wire parity with
/// the 14 query tools that route through `classify_and_execute`.
fn finalize_rebuild_index_response(
    execution: sqry_mcp::execution::ToolExecution<sqry_mcp::execution::RebuildIndexData>,
    plugin_selection_warning: Option<Value>,
) -> Result<CallToolResult, McpError> {
    let mut payload = sqry_mcp::daemon_adapter::tool_response_json(execution)?;
    // Same key, same position as the read-only path
    // (`tool_core::acquire_and_execute`): top level of the tool payload,
    // absent when the resident roster matches the manifest.
    if let Some(warning) = plugin_selection_warning
        && let Value::Object(map) = &mut payload
    {
        map.insert(PLUGIN_SELECTION_WARNING_KEY.into(), warning);
    }
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
// dispatcher, builder, executors, timeout, shutdown) into the MCP handler.
// issue #503 Phase 2 adds `cpu_executor`; the rebuild path adds
// `dispatcher`, taking the count to 10, and the redaction (D-i7-5) to 11.
#[allow(clippy::too_many_arguments)]
pub async fn host_mcp_on_streams<R, W>(
    reader: R,
    writer: W,
    manager: Arc<WorkspaceManager>,
    dispatcher: Arc<RebuildDispatcher>,
    workspace_builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor,
    tool_timeout: Duration,
    daemon_version: &'static str,
    redaction: Arc<redaction::McpRedaction>,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    use rmcp::ServiceExt;

    let handler = DaemonMcpHandler::new(
        manager,
        dispatcher,
        workspace_builder,
        tool_executor,
        cpu_executor,
        tool_timeout,
        daemon_version,
        redaction,
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

    /// The arguments are the shared type's (S6, round 7): `reset_macro_options`
    /// takes exactly what standalone's `bool` takes (omitted is false, an
    /// explicit `null` or any non-boolean is refused), `cfg_flags` and
    /// `expand_cache` take `null` as absent, and an unknown field is refused.
    #[test]
    fn rebuild_index_arguments_are_parsed_by_the_shared_type() {
        use serde_json::json;
        let parse = |v: serde_json::Value| {
            sqry_mcp::daemon_params::parse_rebuild_index_params(&v).map(|p| macro_request_of(&p))
        };
        assert!(!parse(json!({"path": "/ws"})).expect("omitted").reset);
        assert!(
            parse(json!({"path": "/ws", "reset_macro_options": true}))
                .expect("true")
                .reset
        );
        for refused in [
            json!({"path": "/ws", "reset_macro_options": null}),
            json!({"path": "/ws", "reset_macro_options": "yes"}),
            json!({"path": "/ws", "cfg_flag": ["test"]}),
            json!({"path": "/ws", "force": "yes"}),
        ] {
            assert!(parse(refused.clone()).is_err(), "{refused} must be refused");
        }
        let nulls = parse(json!({"path": "/ws", "cfg_flags": null, "expand_cache": null}))
            .expect("null options");
        assert_eq!(nulls.cfg_flags, None);
        assert_eq!(nulls.expand_cache_dir, None);
    }
    use crate::workspace::builder::EmptyGraphBuilder;

    /// A dispatcher over `manager` with the manager's own configuration.
    /// The caller builds that configuration while it holds `TEST_ENV_LOCK`.
    fn test_dispatcher(
        manager: &Arc<WorkspaceManager>,
        config: Arc<crate::config::DaemonConfig>,
    ) -> Arc<RebuildDispatcher> {
        RebuildDispatcher::new(
            Arc::clone(manager),
            config,
            Arc::new(crate::workspace::WorkspaceRosterResolver::new()),
        )
    }

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
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Synthetic handler: a manager with no workspaces + an empty
        // executor is sufficient because `get_info` never touches
        // either field. This protects the wire shape without the
        // weight of a real workspace bringup.
        use crate::config::DaemonConfig;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            test_builder(),
            executor,
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            Arc::new(redaction::McpRedaction::disabled()),
        );

        let info = handler.get_info();
        assert_eq!(info.server_info.name, "sqry-daemon-mcp");
        assert_eq!(info.server_info.version, "0.0.0-test");
        assert!(info.capabilities.tools.is_some());
        let instructions = info.instructions.as_deref().unwrap_or_default();
        assert!(
            instructions.contains("daemon-hosted"),
            "instructions must mention daemon-hosted mode: {instructions}"
        );
        // T16 (surface parity W1): the instructions state what is true
        // about the roster and name the warning key, and no longer
        // promise standalone equivalence.
        assert!(
            instructions.contains(
                "The plugin roster for each workspace is resolved from its \
                 .sqry/graph/manifest.json (fast-path default when no index exists)"
            ),
            "instructions must state how the roster is chosen: {instructions}"
        );
        assert!(
            instructions.contains("plugin_selection_warning"),
            "instructions must name the warning key: {instructions}"
        );
        assert!(
            !instructions.contains("same behaviour"),
            "instructions must not promise standalone equivalence: {instructions}"
        );
    }

    #[test]
    fn handler_tools_list_is_subset_of_daemon_supported_names() {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use crate::config::DaemonConfig;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            test_builder(),
            executor,
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            Arc::new(redaction::McpRedaction::disabled()),
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
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use crate::config::DaemonConfig;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
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
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            test_builder(),
            executor,
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            filtered,
            Arc::new(redaction::McpRedaction::disabled()),
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
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use crate::config::DaemonConfig;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let executor = Arc::new(QueryExecutor::new());
        let handler = DaemonMcpHandler::new(
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            test_builder(),
            executor,
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            Arc::new(redaction::McpRedaction::disabled()),
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
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use crate::config::DaemonConfig;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
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
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            test_builder(),
            executor,
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            only_semantic_search,
            Arc::new(redaction::McpRedaction::disabled()),
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

    /// T51 (surface parity W1 round 5, design D25, codex R5-2's second
    /// site; battery row K40's oracle):
    /// `resident_roster_context_describes_one_observation`. The cache-hit
    /// leg compares the record and describes the graph from one
    /// observation of the slot (`resident_snapshot`: state and generation
    /// under one guard). The plant attempts an eviction at
    /// `SnapshotObserved`, the instant the state was read and the
    /// generation is about to be loaded: under the repaired leg the
    /// attempt cannot take the write lock (`WouldBlock`) and the leg
    /// answers the warning and the metadata of the generation it
    /// observed; under the pre-round-5 leg (`lookup`, `published()`, then
    /// `load_state()` with no guard) the plant evicted and the leg answered
    /// a warning computed from a record whose graph was no longer resident
    /// beside a null `graph_metadata`.
    #[test]
    fn resident_roster_context_describes_one_observation() {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use std::sync::Weak;

        use sqry_core::graph::unified::persistence::{
            BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
        };
        use sqry_core::project::canonicalize_path;

        use crate::config::DaemonConfig;
        use crate::workspace::builder::FunctionGraphBuilder;
        use crate::workspace::manager::{ObservationPhase, TryEvictOutcome};

        const NODES: u32 = 5;

        // `graph_metadata_for_resident_graph` reads the sqry-mcp payload
        // caches' telemetry; `IpcServer::bind` initialises them in the
        // daemon process and this test calls the leg directly, so it
        // initialises them the same way (idempotent).
        sqry_mcp::init_mcp_caches(
            &sqry_mcp::McpConfig::load_or_default().expect("default MCP config"),
        )
        .expect("MCP caches initialise");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = canonicalize_path(tmp.path()).expect("canonical root");

        // The T41 fixture shape in miniature: an `include_all` manifest on
        // disk beside a fast-path record in the pair, so the warning is
        // computable (the manifest names ids the record does not carry).
        let storage = GraphStorage::new(&root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        let all_ids: Vec<String> = sqry_plugin_registry::create_plugin_manager_all()
            .plugins()
            .iter()
            .map(|plugin| plugin.metadata().id.to_string())
            .collect();
        let mut manifest = Manifest::new(
            root.display().to_string(),
            0,
            0,
            "0".repeat(64),
            BuildProvenance::new("0.0.0-test", "test:t51"),
        );
        manifest.plugin_selection = Some(PluginSelectionManifest {
            active_plugin_ids: all_ids,
            high_cost_mode: Some("include_all".to_string()),
        });
        manifest
            .save(storage.manifest_path())
            .expect("manifest written");

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let builder: Arc<dyn WorkspaceBuilder> =
            Arc::new(FunctionGraphBuilder::with_fast_path_record(NODES));
        let handler = DaemonMcpHandler::new(
            Arc::clone(&manager),
            test_dispatcher(&manager, config),
            Arc::clone(&builder),
            Arc::new(QueryExecutor::new()),
            crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            Duration::from_secs(60),
            "0.0.0-test",
            Arc::new(redaction::McpRedaction::disabled()),
        );
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);
        let published = manager
            .get_or_load_published(&key, builder.as_ref(), 0)
            .expect("the fixture generation publishes");
        assert_eq!(published.graph.node_count(), NODES as usize);

        let evict: Arc<parking_lot::Mutex<Option<TryEvictOutcome>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let weak: Weak<WorkspaceManager> = Arc::downgrade(&manager);
        let plant_key = key.clone();
        let plant_evict = Arc::clone(&evict);
        manager.install_observation_plant_for_test(Arc::new(move |phase| {
            if phase != ObservationPhase::SnapshotObserved || plant_evict.lock().is_some() {
                return;
            }
            if let Some(manager) = weak.upgrade() {
                *plant_evict.lock() = Some(manager.try_evict_for_test(&plant_key));
            }
        }));

        let (warning, metadata) = handler.resident_roster_context(&key, &root, None);
        let evict_outcome = *evict.lock();
        let slot_state = manager
            .lookup(&key)
            .expect("the slot is resident")
            .load_state();
        println!(
            "R5-2 snapshot plant: evict={evict_outcome:?} warning_present={} \
             metadata_present={} slot_state={slot_state:?}",
            warning.is_some(),
            metadata.is_some()
        );
        assert_eq!(
            (warning.is_some(), metadata.is_some()),
            (true, true),
            "the record compared and the graph described must be one observation"
        );
        assert_eq!(
            evict_outcome,
            Some(TryEvictOutcome::WouldBlock),
            "the eviction attempted at SnapshotObserved cannot take the write lock while \
             the snapshot holds its read guard"
        );
        let metadata = metadata.expect("asserted present");
        assert_eq!(
            metadata.total_nodes,
            u64::from(NODES),
            "the metadata describes the generation whose record was compared"
        );
        let warning = warning.expect("asserted present");
        let missing: Vec<&str> = warning["missing_plugin_ids"]
            .as_array()
            .expect("missing_plugin_ids array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            !missing.is_empty(),
            "the warning names the ids the include_all manifest carries beyond the \
             fast-path record: {warning}"
        );
        assert_eq!(slot_state, WorkspaceState::Loaded);

        // Control that the deferred eviction takes effect: evicted, the
        // same call answers (None, None) (T45's form).
        assert!(
            manager.evict_for_test(&key),
            "the eviction the plant could not perform runs once the leg has answered"
        );
        let (warning_after, metadata_after) = handler.resident_roster_context(&key, &root, None);
        println!(
            "R5-2 snapshot after eviction: warning_present={} metadata_present={} slot_state={:?}",
            warning_after.is_some(),
            metadata_after.is_some(),
            manager.lookup(&key).map(|ws| ws.load_state())
        );
        assert!(
            warning_after.is_none() && metadata_after.is_none(),
            "an evicted slot yields (None, None)"
        );
    }
}
