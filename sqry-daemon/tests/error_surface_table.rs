//! F8 (integration round 7, DAEMON_FOLLOWUP): the table of every refusal
//! the daemon answers on its two surfaces, IPC (`DaemonError`'s code,
//! message and `error.data`) and the daemon-hosted MCP (the mapper the live
//! host calls), and what the standalone `sqry-mcp` server answers for the
//! same refusal. `docs/cli/daemon.md` ("Error codes on the two daemon
//! surfaces") prints the table; this test holds the code to it.
//!
//! The table is derived from the code, not listed beside it:
//!
//! - [`expect`] is an exhaustive `match` over `DaemonError` with no
//!   wildcard arm, so a new variant does not compile until it is classified.
//! - [`samples`] holds one value of each variant, and
//!   [`every_variant_has_a_sample`] counts the variants `error.rs` declares
//!   and fails until a new one has a sample too.
//! - [`every_constructor_the_daemon_calls_is_classified`] scans the daemon's
//!   sources for the `sqry_mcp::error::RpcError` constructors it calls and
//!   fails until a new one is classified in [`CONSTRUCTORS`].
//!
//! The method layer's own shapes (`MethodError`, including a `DaemonError`
//! with no code of its own) are crate-private; their table is the unit test
//! `ipc::methods::tests::every_method_error_shape_matches_the_table`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sqry_daemon::DaemonError;
use sqry_daemon::mcp_host::error_map::{
    daemon_err_to_mcp_for_rebuild_index, daemon_err_to_mcp_with_tool,
};
use sqry_mcp::error::{ExpandCacheOrigin, RpcError, render_error_chain, rpc_error_to_mcp};

/// The tool a generic row is answered for on the daemon-hosted MCP.
const TOOL: &str = "semantic_search";

/// The `path` argument of the `rebuild_index` request a row answers, as the
/// caller sent it.
const REQUESTED_PATH: &str = "./repo";

/// Which daemon-hosted MCP mapper answers the refusal on the live host.
#[derive(Debug, Clone, Copy)]
enum Route {
    /// A tool call (`daemon_err_to_mcp_with_tool`).
    Tool,
    /// `rebuild_index` (`daemon_err_to_mcp_for_rebuild_index`), which
    /// echoes the request's `path` in `details.reset_arguments.path`.
    RebuildIndex,
}

/// A part of an MCP envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Code,
    Message,
    Data,
}

/// What the standalone `sqry-mcp` server answers for the same refusal.
enum Standalone {
    /// Exactly the daemon-hosted envelope (code, message and `data`),
    /// built by the standalone server's own constructor.
    Same(rmcp::ErrorData),
    /// The standalone server gives the same refusal in another envelope.
    /// `envelope` is that envelope where this test can build it, and
    /// `differs` names the parts that differ from the daemon's; `how` says
    /// what the standalone server sends where the envelope cannot be built
    /// here (it comes from its graph loader, not a constructor).
    Differs {
        envelope: Option<rmcp::ErrorData>,
        differs: &'static [Part],
        how: &'static str,
    },
    /// The standalone server never gives this refusal.
    DaemonOnly,
}

/// How a row's IPC message relates to its MCP message.
#[derive(Debug, PartialEq, Eq)]
enum Text {
    SameText,
    /// The same reason, then a remedy naming the CLI flags on IPC and the
    /// wire fields on MCP.
    RemedyByAudience,
}

struct Expect {
    name: &'static str,
    /// `None`: the variant has no code of its own, and the method layer
    /// answers `-32603` "Internal error" with `data.reason`.
    ipc_code: Option<i32>,
    mcp_code: i32,
    mcp_kind: &'static str,
    route: Route,
    standalone: Standalone,
    text: Text,
    /// `error.data.root` on IPC; `None` asserts the key is absent.
    ipc_root: Option<String>,
    /// `error.data.details.root` on MCP; `None` asserts it is absent.
    mcp_root: Option<String>,
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

fn root() -> PathBuf {
    PathBuf::from("/repo")
}

/// The classification of every `DaemonError` variant. No wildcard arm: a
/// new variant is a compile error here until it is classified.
#[allow(clippy::too_many_lines)]
fn expect(err: &DaemonError) -> Expect {
    use Standalone::{DaemonOnly, Differs, Same};
    use Text::{RemedyByAudience, SameText};
    // The catch-all MCP arm: `internal`, the IPC text, no details.
    let internal = |name, ipc_code: Option<i32>, ipc_root: Option<String>| Expect {
        name,
        ipc_code,
        mcp_code: -32603,
        mcp_kind: "internal",
        route: Route::Tool,
        standalone: DaemonOnly,
        text: SameText,
        ipc_root,
        mcp_root: None,
    };
    match err {
        DaemonError::Config { .. } => internal("Config", None, None),
        DaemonError::Io(_) => internal("Io", None, None),
        DaemonError::AlreadyRunning { .. } => internal("AlreadyRunning", None, None),
        DaemonError::AutoStartTimeout { .. } => internal("AutoStartTimeout", None, None),
        DaemonError::SignalSetup { .. } => internal("SignalSetup", None, None),
        DaemonError::InvalidArgument { reason } => Expect {
            name: "InvalidArgument",
            ipc_code: Some(-32602),
            mcp_code: -32602,
            mcp_kind: "validation_error",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(RpcError::invalid_argument(reason.clone()))),
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        DaemonError::RpcErrorPreserved(rpc) => Expect {
            name: "RpcErrorPreserved",
            ipc_code: Some(rpc.code),
            mcp_code: rpc.code,
            mcp_kind: "validation_error",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(rpc.clone())),
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        DaemonError::WorkspaceBuildFailed { root, reason } => Expect {
            name: "WorkspaceBuildFailed",
            ipc_code: Some(-32001),
            mcp_code: -32603,
            mcp_kind: "workspace_not_ready",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(RpcError::workspace_build_failed(
                root, reason,
            ))),
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::WorkspaceManifestUnreadable {
            root,
            manifest_path,
            reason,
        } => Expect {
            name: "WorkspaceManifestUnreadable",
            ipc_code: Some(-32001),
            mcp_code: -32603,
            mcp_kind: "workspace_not_ready",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(RpcError::workspace_manifest_unreadable(
                root,
                manifest_path,
                reason,
            ))),
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::WorkspaceIncompatibleGraph { root, reason } => Expect {
            name: "WorkspaceIncompatibleGraph",
            ipc_code: Some(-32005),
            mcp_code: -32603,
            mcp_kind: "workspace_incompatible_graph",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(RpcError::workspace_incompatible_graph(
                root, reason,
            ))),
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::QueryTooBroad { reason, details } => Expect {
            name: "QueryTooBroad",
            ipc_code: Some(-32602),
            mcp_code: -32602,
            mcp_kind: "query_too_broad",
            route: Route::Tool,
            standalone: Same(rpc_error_to_mcp(RpcError::query_too_broad(
                reason.clone(),
                details.clone(),
            ))),
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        // Every live route that answers this refusal is `rebuild_index`'s,
        // which echoes the request's `path` in `reset_arguments`.
        DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } => Expect {
            name: "RebuildMacroOptionsUnavailable",
            ipc_code: Some(-32022),
            mcp_code: -32602,
            mcp_kind: "rebuild_macro_options_unavailable",
            route: Route::RebuildIndex,
            standalone: Same(rpc_error_to_mcp(
                RpcError::rebuild_macro_options_unavailable(root, expand_cache_dir, *origin)
                    .with_reset_path(REQUESTED_PATH),
            )),
            text: RemedyByAudience,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::RebuildWouldNarrowSelection { root, .. } => Expect {
            name: "RebuildWouldNarrowSelection",
            ipc_code: Some(-32021),
            mcp_code: -32602,
            mcp_kind: "rebuild_would_narrow_selection",
            route: Route::RebuildIndex,
            standalone: DaemonOnly,
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        // The absent manifest: the standalone server answers a never
        // indexed root on its own terms (no shared refusal). The absent
        // snapshot beside a manifest: it answers from its graph loader.
        DaemonError::WorkspaceNotIndexed {
            root, missing_path, ..
        } => {
            let snapshot_form = missing_path.ends_with("snapshot.sqry");
            Expect {
                name: "WorkspaceNotIndexed",
                ipc_code: Some(-32001),
                mcp_code: -32603,
                mcp_kind: "workspace_not_ready",
                route: Route::Tool,
                standalone: if snapshot_form {
                    Differs {
                        envelope: None,
                        differs: &[Part::Message, Part::Data],
                        how: "-32603, data null, \"Failed to load graph at <root>: read snapshot ...\"",
                    }
                } else {
                    DaemonOnly
                },
                text: SameText,
                ipc_root: Some(text(root)),
                mcp_root: Some(text(root)),
            }
        }
        DaemonError::WorkspaceSnapshotUnreadable { root, .. } => Expect {
            name: "WorkspaceSnapshotUnreadable",
            ipc_code: Some(-32001),
            mcp_code: -32603,
            mcp_kind: "workspace_not_ready",
            route: Route::Tool,
            standalone: Differs {
                envelope: None,
                differs: &[Part::Message, Part::Data],
                how: "-32603, data null, \"Failed to load graph at <root>: snapshot integrity check \
                      failed ...\"",
            },
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::WorkspaceReloadFailed { root, .. } => Expect {
            name: "WorkspaceReloadFailed",
            ipc_code: Some(-32004),
            mcp_code: -32603,
            mcp_kind: "workspace_not_ready",
            route: Route::Tool,
            standalone: DaemonOnly,
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        DaemonError::WorkspaceStaleExpired { root, .. } => Expect {
            name: "WorkspaceStaleExpired",
            ipc_code: Some(-32002),
            mcp_code: -32603,
            mcp_kind: "workspace_stale_expired",
            route: Route::Tool,
            standalone: DaemonOnly,
            text: SameText,
            ipc_root: Some(text(root)),
            mcp_root: Some(text(root)),
        },
        // The standalone server's timeout carries the same data; its
        // message names the tool where the daemon's names the workspace.
        DaemonError::ToolTimeout { deadline_ms, .. } => Expect {
            name: "ToolTimeout",
            ipc_code: Some(-32000),
            mcp_code: -32603,
            mcp_kind: "deadline_exceeded",
            route: Route::Tool,
            standalone: Differs {
                envelope: Some(rpc_error_to_mcp(RpcError::deadline_exceeded(
                    TOOL,
                    *deadline_ms,
                    500,
                ))),
                differs: &[Part::Message],
                how: "\"Tool '<tool>' exceeded deadline of <n>ms\"",
            },
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        DaemonError::RebuildOutcomeTimeout { .. } => Expect {
            name: "RebuildOutcomeTimeout",
            ipc_code: Some(-32000),
            mcp_code: -32603,
            mcp_kind: "deadline_exceeded",
            route: Route::RebuildIndex,
            standalone: DaemonOnly,
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        // The standalone server renders an internal failure as the error
        // chain alone, with no `data`.
        DaemonError::Internal(cause) => Expect {
            name: "Internal",
            ipc_code: Some(-32603),
            mcp_code: -32603,
            mcp_kind: "internal",
            route: Route::Tool,
            standalone: Differs {
                envelope: Some(rmcp::ErrorData::internal_error(
                    render_error_chain(cause),
                    None,
                )),
                differs: &[Part::Message, Part::Data],
                how: "the error chain without \"internal error: \", data null",
            },
            text: SameText,
            ipc_root: None,
            mcp_root: None,
        },
        DaemonError::MemoryBudgetExceeded { .. } => {
            internal("MemoryBudgetExceeded", Some(-32003), None)
        }
        DaemonError::WorkspaceEvicted { root } => {
            internal("WorkspaceEvicted", Some(-32004), Some(text(root)))
        }
        DaemonError::WorkspaceNotLoaded { root } => {
            internal("WorkspaceNotLoaded", Some(-32004), Some(text(root)))
        }
        DaemonError::WorkspaceOversize { root, .. } => {
            internal("WorkspaceOversize", Some(-32006), Some(text(root)))
        }
        DaemonError::WorkspacePinned { root } => {
            internal("WorkspacePinned", Some(-32010), Some(text(root)))
        }
        DaemonError::ResetWhileLoading { root } => {
            internal("ResetWhileLoading", Some(-32008), Some(text(root)))
        }
        DaemonError::ResetCancellationDispatched { root, .. } => internal(
            "ResetCancellationDispatched",
            Some(-32009),
            Some(text(root)),
        ),
        DaemonError::SocketSetup { .. } => internal("SocketSetup", Some(-32007), None),
        DaemonError::RevisionSelectorAmbiguous { .. } => {
            internal("RevisionSelectorAmbiguous", Some(-32011), None)
        }
        DaemonError::RevisionObjectMissing { .. } => {
            internal("RevisionObjectMissing", Some(-32012), None)
        }
        DaemonError::RevisionSourceUnavailable { .. } => {
            internal("RevisionSourceUnavailable", Some(-32013), None)
        }
        DaemonError::CheckoutFilterUnsupported { .. } => {
            internal("CheckoutFilterUnsupported", Some(-32014), None)
        }
        DaemonError::SubmoduleUnavailable { .. } => {
            internal("SubmoduleUnavailable", Some(-32015), None)
        }
        DaemonError::DirtySnapshotChanged { root } => {
            internal("DirtySnapshotChanged", Some(-32016), Some(text(root)))
        }
        DaemonError::ArtifactKeyMismatch { .. } => {
            internal("ArtifactKeyMismatch", Some(-32017), None)
        }
        DaemonError::ManagedWorktreeInUse { .. } => {
            internal("ManagedWorktreeInUse", Some(-32018), None)
        }
        DaemonError::RevisionDiskBudgetExceeded { .. } => {
            internal("RevisionDiskBudgetExceeded", Some(-32019), None)
        }
        DaemonError::RevisionQueryRequiresExplicitSelector { .. } => {
            internal("RevisionQueryRequiresExplicitSelector", Some(-32020), None)
        }
    }
}

/// One value of every `DaemonError` variant (two of `WorkspaceNotIndexed`,
/// whose two forms the standalone server answers differently, and two of
/// the expand cache's origins).
#[allow(clippy::too_many_lines)]
fn samples() -> Vec<DaemonError> {
    let root = root();
    let manifest = root.join(".sqry/graph/manifest.json");
    let snapshot = root.join(".sqry/graph/snapshot.sqry");
    let cache = root.join("cache");
    vec![
        DaemonError::Config {
            path: root.join("daemon.toml"),
            source: anyhow::anyhow!("bad key"),
        },
        DaemonError::Io(std::io::Error::other("disk gone")),
        DaemonError::WorkspaceBuildFailed {
            root: root.clone(),
            reason: "plugin panic".into(),
        },
        DaemonError::WorkspaceStaleExpired {
            root: root.clone(),
            age_hours: 48,
            cap_hours: 24,
            last_good_at: None,
            last_error: Some("plugin panic".into()),
        },
        DaemonError::MemoryBudgetExceeded {
            limit_bytes: 1,
            current_bytes: 1,
            reserved_bytes: 0,
            retained_bytes: 0,
            requested_bytes: 2,
        },
        DaemonError::WorkspaceEvicted { root: root.clone() },
        DaemonError::WorkspaceNotLoaded { root: root.clone() },
        DaemonError::WorkspaceIncompatibleGraph {
            root: root.clone(),
            reason: "unknown plugin ids: [x]".into(),
        },
        DaemonError::ToolTimeout {
            root: root.clone(),
            secs: 60,
            deadline_ms: 60_000,
        },
        DaemonError::RebuildOutcomeTimeout {
            root: root.clone(),
            secs: 600,
            deadline_ms: 600_000,
        },
        DaemonError::InvalidArgument {
            reason: "a refused argument".into(),
        },
        DaemonError::RpcErrorPreserved(RpcError::validation("a tool's own validation")),
        DaemonError::Internal(anyhow::anyhow!("a daemon fault")),
        DaemonError::AlreadyRunning {
            socket: root.join("sqryd.sock"),
            lock: root.join("sqryd.lock"),
            owner_pid: Some(7),
        },
        DaemonError::AutoStartTimeout {
            timeout_secs: 5,
            socket: root.join("sqryd.sock"),
        },
        DaemonError::SignalSetup {
            source: std::io::Error::other("ENOSYS"),
        },
        DaemonError::WorkspaceOversize {
            root: root.clone(),
            measured_bytes: 3,
            limit_bytes: 2,
            current_loaded_bytes: 0,
        },
        DaemonError::WorkspacePinned { root: root.clone() },
        DaemonError::ResetWhileLoading { root: root.clone() },
        DaemonError::ResetCancellationDispatched {
            root: root.clone(),
            retry_after_ms: 100,
        },
        DaemonError::SocketSetup {
            path: root.join("sqryd.sock"),
            reason: "EACCES".into(),
        },
        DaemonError::QueryTooBroad {
            reason: "query rejected: too broad".into(),
            details: json!({ "source": "static_estimate", "kind": "query_too_broad", "limit": 1 }),
        },
        DaemonError::RevisionSelectorAmbiguous {
            selector: "v1".into(),
            matches: vec!["a".into(), "b".into()],
        },
        DaemonError::RevisionObjectMissing {
            object: "deadbeef".into(),
            path: Some(root.join("a.rs")),
        },
        DaemonError::RevisionSourceUnavailable {
            reason: "gone".into(),
            path: Some(root.join("a.rs")),
        },
        DaemonError::CheckoutFilterUnsupported {
            filter: "lfs".into(),
            path: Some(root.join("a.rs")),
        },
        DaemonError::SubmoduleUnavailable {
            path: root.join("sub"),
            gitlink_oid: Some("cafe".into()),
        },
        DaemonError::DirtySnapshotChanged { root: root.clone() },
        DaemonError::ArtifactKeyMismatch {
            artifact_id: "art".into(),
            reason: "mismatch".into(),
        },
        DaemonError::ManagedWorktreeInUse {
            worktree: root.join("wt"),
            reason: "leased".into(),
        },
        DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes: 1,
            requested_bytes: 2,
            current_bytes: 1,
        },
        DaemonError::RebuildWouldNarrowSelection {
            root: root.clone(),
            missing_plugin_ids: vec!["json".into()],
            restore_command: "sqry index --force --include-high-cost /repo".into(),
        },
        DaemonError::RebuildMacroOptionsUnavailable {
            root: root.clone(),
            expand_cache_dir: cache.clone(),
            origin: ExpandCacheOrigin::Recorded,
        },
        DaemonError::RebuildMacroOptionsUnavailable {
            root: root.clone(),
            expand_cache_dir: cache,
            origin: ExpandCacheOrigin::Requested,
        },
        DaemonError::RevisionQueryRequiresExplicitSelector {
            reason: "ambiguous".into(),
        },
        DaemonError::WorkspaceManifestUnreadable {
            root: root.clone(),
            manifest_path: manifest.clone(),
            reason: "EOF".into(),
        },
        DaemonError::workspace_not_indexed(&root, manifest),
        DaemonError::workspace_snapshot_missing(&root, snapshot.clone()),
        DaemonError::WorkspaceSnapshotUnreadable {
            root: root.clone(),
            snapshot_path: snapshot,
            reason: "bad magic".into(),
        },
        DaemonError::WorkspaceReloadFailed {
            root,
            reload_failure: "snapshot cannot be loaded".into(),
        },
    ]
}

/// The part of a message before the first `; `: the reason, without the
/// remedy.
fn reason_of(message: &str) -> &str {
    message.split("; ").next().unwrap_or_default()
}

/// The parts in which two envelopes differ.
fn parts_differing(a: &rmcp::ErrorData, b: &rmcp::ErrorData) -> Vec<Part> {
    let mut parts = Vec::new();
    if a.code != b.code {
        parts.push(Part::Code);
    }
    if a.message != b.message {
        parts.push(Part::Message);
    }
    if a.data != b.data {
        parts.push(Part::Data);
    }
    parts
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_refusal_kind_matches_the_table() {
    let mut checked = 0;
    for err in samples() {
        let row = expect(&err);
        let ipc_message = err.to_string();
        let ipc_data = err.error_data().unwrap_or(Value::Null);
        let ipc_code = err.jsonrpc_code();
        let mcp = match row.route {
            Route::Tool => daemon_err_to_mcp_with_tool(err, TOOL),
            Route::RebuildIndex => daemon_err_to_mcp_for_rebuild_index(err, REQUESTED_PATH),
        };
        let mcp_data = mcp.data.clone().unwrap_or(Value::Null);
        let mcp_kind = mcp_data["kind"].as_str().unwrap_or_default().to_string();
        println!(
            "| {} | {:?} | {} | {} | {} |",
            row.name,
            ipc_code,
            mcp.code.0,
            mcp_kind,
            match &row.standalone {
                Standalone::Same(_) => "standalone server's".to_string(),
                Standalone::Differs { differs, how, .. } =>
                    format!("standalone differs in {differs:?}: {how}"),
                Standalone::DaemonOnly => "daemon's".to_string(),
            }
        );
        assert_eq!(ipc_code, row.ipc_code, "{}: IPC code", row.name);
        assert_eq!(mcp.code.0, row.mcp_code, "{}: MCP code", row.name);
        assert_eq!(mcp_kind, row.mcp_kind, "{}: MCP kind", row.name);
        match &row.standalone {
            Standalone::Same(shared) => assert!(
                parts_differing(&mcp, shared).is_empty(),
                "{}: the MCP envelope is the standalone server's\n daemon: {mcp:?}\n standalone: \
                 {shared:?}",
                row.name
            ),
            Standalone::Differs {
                envelope: Some(standalone),
                differs,
                ..
            } => assert_eq!(
                parts_differing(&mcp, standalone),
                differs.to_vec(),
                "{}: the parts in which the standalone envelope differs\n daemon: {mcp:?}\n \
                 standalone: {standalone:?}",
                row.name
            ),
            // Built by the standalone server's graph loader: `-32603` with
            // no `data` (the `how`), where the daemon names its kind.
            Standalone::Differs { envelope: None, .. } => assert!(
                mcp.data.is_some(),
                "{}: the daemon's envelope carries data the standalone one lacks",
                row.name
            ),
            Standalone::DaemonOnly => {}
        }
        match row.text {
            Text::SameText => assert_eq!(
                ipc_message,
                mcp.message.as_ref(),
                "{}: one text on both surfaces",
                row.name
            ),
            Text::RemedyByAudience => {
                assert_eq!(
                    reason_of(&ipc_message),
                    reason_of(&mcp.message),
                    "{}: one reason on both surfaces",
                    row.name
                );
                assert_ne!(
                    ipc_message,
                    mcp.message.as_ref(),
                    "{}: the remedies differ by audience",
                    row.name
                );
            }
        }
        // The root, unconditionally: present where the row says, under the
        // key the row names, and absent elsewhere.
        assert_eq!(
            ipc_data.get("root").and_then(Value::as_str),
            row.ipc_root.as_deref(),
            "{}: IPC error.data.root (data: {ipc_data})",
            row.name
        );
        assert_eq!(
            mcp_data["details"].get("root").and_then(Value::as_str),
            row.mcp_root.as_deref(),
            "{}: MCP error.data.details.root (data: {mcp_data})",
            row.name
        );
        checked += 1;
    }
    assert_eq!(checked, samples().len(), "every sample was checked");
}

/// The variants `error.rs` declares, by name, read from the source: every
/// line indented four spaces inside `pub enum DaemonError { ... }` that
/// starts with an upper-case identifier.
fn declared_variants() -> BTreeSet<String> {
    let source = include_str!("../src/error.rs");
    let body = source
        .split_once("pub enum DaemonError {")
        .expect("error.rs declares DaemonError")
        .1
        .split_once("\n}\n")
        .expect("DaemonError's body closes")
        .0;
    body.lines()
        .filter_map(|line| line.strip_prefix("    "))
        .filter(|line| line.starts_with(|c: char| c.is_ascii_uppercase()))
        .map(|line| {
            line.chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect::<String>()
        })
        .collect()
}

#[test]
fn every_variant_has_a_sample() {
    let declared = declared_variants();
    let sampled: BTreeSet<String> = samples()
        .iter()
        .map(|err| expect(err).name.to_string())
        .collect();
    println!(
        "{} variants declared, {} sampled",
        declared.len(),
        sampled.len()
    );
    assert_eq!(
        sampled, declared,
        "every DaemonError variant has a sample in the table, and only those"
    );
    // The one count the source and the table agree on today.
    assert_eq!(declared.len(), 38, "DaemonError's variant count");
}

/// What the daemon-hosted MCP sends for each `sqry_mcp::error::RpcError`
/// constructor the daemon calls (all through `rpc_error_to_mcp`, so the
/// envelope is the standalone server's by construction): the MCP code
/// (`-32602` stays `invalid_params`, every other code is `internal_error`,
/// so the timeout's `-32000` reaches MCP as `-32603`) and the kind.
const CONSTRUCTORS: &[(&str, i32, &str)] = &[
    ("deadline_exceeded", -32603, "deadline_exceeded"),
    ("invalid_argument", -32602, "validation_error"),
    ("macro_options_need_force", -32602, "validation_error"),
    ("nested_index_refused", -32602, "validation_error"),
    ("query_too_broad", -32602, "query_too_broad"),
    (
        "rebuild_macro_options_unavailable",
        -32602,
        "rebuild_macro_options_unavailable",
    ),
    ("validation_with_data", -32602, "validation_error"),
    ("workspace_build_failed", -32603, "workspace_not_ready"),
    ("workspace_index_removed", -32603, "workspace_not_ready"),
    (
        "workspace_incompatible_graph",
        -32603,
        "workspace_incompatible_graph",
    ),
    (
        "workspace_manifest_unreadable",
        -32603,
        "workspace_not_ready",
    ),
];

/// The `RpcError::<name>` constructors the daemon's sources call (methods
/// such as `with_reset_path` are not constructors and are left out).
fn constructors_the_daemon_calls() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read the daemon's sources") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let methods: HashSet<&str> = ["with_reset_path"].into_iter().collect();
    let mut found = BTreeSet::new();
    for file in files {
        let source = std::fs::read_to_string(&file).expect("read a source file");
        for (_, rest) in source
            .match_indices("RpcError::")
            .map(|(i, _)| ((), &source[i + 10..]))
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            if !name.is_empty() && !methods.contains(name.as_str()) {
                found.insert(name);
            }
        }
    }
    found
}

#[test]
fn every_constructor_the_daemon_calls_is_classified() {
    let called = constructors_the_daemon_calls();
    let classified: BTreeSet<String> = CONSTRUCTORS
        .iter()
        .map(|(name, ..)| (*name).to_string())
        .collect();
    assert_eq!(
        called, classified,
        "every RpcError constructor the daemon calls is in CONSTRUCTORS"
    );
    let root = root();
    let built: BTreeMap<&str, RpcError> = [
        (
            "deadline_exceeded",
            RpcError::deadline_exceeded(TOOL, 1000, 500),
        ),
        ("invalid_argument", RpcError::invalid_argument("r")),
        (
            "macro_options_need_force",
            RpcError::macro_options_need_force(&root),
        ),
        (
            "nested_index_refused",
            RpcError::nested_index_refused(&root.join("sub"), &root.join(".sqry/graph"), &root),
        ),
        (
            "query_too_broad",
            RpcError::query_too_broad("q", Value::Null),
        ),
        (
            "rebuild_macro_options_unavailable",
            RpcError::rebuild_macro_options_unavailable(
                &root,
                &root.join("cache"),
                ExpandCacheOrigin::Requested,
            ),
        ),
        (
            "validation_with_data",
            RpcError::validation_with_data("v", json!({})),
        ),
        (
            "workspace_build_failed",
            RpcError::workspace_build_failed(&root, "r"),
        ),
        (
            "workspace_index_removed",
            RpcError::workspace_index_removed(&root, "r"),
        ),
        (
            "workspace_incompatible_graph",
            RpcError::workspace_incompatible_graph(&root, "r"),
        ),
        (
            "workspace_manifest_unreadable",
            RpcError::workspace_manifest_unreadable(&root, &root.join("m"), "r"),
        ),
    ]
    .into_iter()
    .collect();
    for (name, code, kind) in CONSTRUCTORS {
        let mcp = rpc_error_to_mcp(built[name].clone());
        assert_eq!(mcp.code.0, *code, "{name}: MCP code");
        assert_eq!(
            mcp.data.as_ref().and_then(|data| data["kind"].as_str()),
            Some(*kind),
            "{name}: MCP kind"
        );
    }
}

/// The text the two surfaces used to disagree on (F8): the build failure
/// named the root on IPC only, and the cost-gate refusal carried a prefix
/// that repeated the gate's own ("query rejected by cost gate: query
/// rejected: ..." on IPC, "query rejected: query rejected: ..." on MCP).
#[test]
fn the_texts_f8_named_are_one_text() {
    let failed = DaemonError::WorkspaceBuildFailed {
        root: Path::new("/repo").to_path_buf(),
        reason: "plugin panic".into(),
    };
    assert_eq!(failed.to_string(), "workspace build failed: plugin panic");
    let broad = DaemonError::QueryTooBroad {
        reason: "query rejected: predicate `name~=.` is unbounded".into(),
        details: Value::Null,
    };
    assert_eq!(
        broad.to_string(),
        "query rejected: predicate `name~=.` is unbounded"
    );
    assert_eq!(
        daemon_err_to_mcp_with_tool(broad, TOOL).message,
        "query rejected: predicate `name~=.` is unbounded"
    );
}
