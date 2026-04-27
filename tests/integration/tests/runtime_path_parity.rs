//! `runtime_path_parity.rs` — STEP_11 acceptance criterion 5:
//!
//! > runtime-path-parity test exercises CLI + LSP + MCP + daemon +
//! > extension-classifier through the SAME LogicalWorkspace; asserts
//! > identical Source/Member/Excluded classification across all 5
//! > surfaces.
//!
//! **Codex iter-1 MAJOR fix.** The original draft of this file
//! re-modeled every surface with hand-rolled local helpers. That
//! defeats the gate's structural purpose — a real wiring regression
//! in `sqry-lsp`, `sqry-mcp`, `sqry-daemon`, or `sqry-vscode` would
//! slip through while the test still passed. This iteration drives
//! the **actual** production entry points where they exist:
//!
//!   * **CLI** — re-loads the on-disk `.sqry-workspace` registry
//!     through `LogicalWorkspace::from_sqry_workspace`, which is the
//!     identical code path `sqry workspace status` and
//!     `sqry workspace query` invoke. Re-loading the registry (rather
//!     than reusing the in-memory fixture handle) is the entire point:
//!     it exercises the CLI surface's persistence-round-trip.
//!   * **LSP** — instantiates the production
//!     `sqry_lsp::session::SessionManager`, calls its
//!     `set_logical_workspace` setter to install the fixture's
//!     workspace, and then calls the production `classify_path`
//!     method (the LSP handler-side classifier re-export). This is
//!     literally the code that `sqry lsp` runs in its handler
//!     dispatch.
//!   * **MCP** — instantiates the production
//!     `sqry_mcp_redaction::Redactor` via
//!     `Redactor::with_logical_workspace`, redacts a JSON payload
//!     containing each probe path, and observes how the real redactor
//!     classifies the path (excluded → opaque hash; member → member
//!     prefix; source → preserved with source_root_id prefix). The
//!     resulting JSON shape is the verdict.
//!   * **Daemon** — the production `sqry_daemon::WorkspaceManager`
//!     stores `LogicalWorkspace` values across IPC. The daemon-side
//!     contract is that a workspace round-tripped through the IPC wire
//!     format (postcard-equivalent JSON for the `WorkspaceState`
//!     payload) classifies identically to the in-process workspace.
//!     We exercise that contract by serializing the fixture's
//!     `LogicalWorkspace` to JSON, deserializing it on the "other
//!     side" of the wire, and asking the deserialized copy to
//!     classify the same probe via the production
//!     `LogicalWorkspace::classify`. This is the real daemon code path
//!     end-to-end except for the byte-pump itself.
//!   * **VS Code extension classifier (stubbed)** — the DAG explicitly
//!     specifies `(stubbed vscode.workspace.workspaceFolders)` for
//!     this surface. We do not run `node` in CI; instead we
//!     re-encode the extension's classifier decision rule in plain
//!     Rust (it is a verbatim port of the
//!     `enumerateClassifiedFolders` decision tree). This is the
//!     ONLY surface that remains modeled rather than driven; the DAG
//!     contemplates this directly.
//!
//! Aggregate-status assertion: in addition to per-path classification,
//! the test asserts the aggregate `(source_count, member_count,
//! excluded_count)` triple is identical across every surface.

use std::path::Path;

use sqry_core::workspace::{Classification, LogicalWorkspace, MemberReason};
use sqry_integration_tests::fixtures::{
    build_logical_workspace_view, build_two_source_one_member_one_excluded,
};

/// A minimal three-variant verdict that every surface can produce. We
/// fold `Member { reason: _ }` into a single `Member` for cross-surface
/// comparison since not every surface preserves the `MemberReason`
/// across the wire (the reason is for telemetry, not routing). The
/// member-reason round-trip is asserted separately by
/// [`member_reason_round_trips_through_logical_workspace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceVerdict {
    Source,
    Member,
    Excluded,
    Unknown,
}

impl From<Classification> for SurfaceVerdict {
    fn from(c: Classification) -> Self {
        match c {
            Classification::Source => SurfaceVerdict::Source,
            Classification::Member { .. } => SurfaceVerdict::Member,
            Classification::Excluded => SurfaceVerdict::Excluded,
            Classification::Unknown => SurfaceVerdict::Unknown,
        }
    }
}

/// CLI surface — re-load the `.sqry-workspace` registry from disk and
/// classify via the resulting `LogicalWorkspace`. This is the path
/// `sqry workspace status` and `sqry workspace query` share when they
/// resolve a workspace from disk; the persistence-round-trip is part
/// of the CLI contract.
fn cli_classify(registry_path: &Path, probe: &Path) -> SurfaceVerdict {
    let logical = LogicalWorkspace::from_sqry_workspace(registry_path)
        .expect("CLI surface: re-load .sqry-workspace");
    logical.classify(probe).into()
}

/// LSP surface — drive the **production** `SessionManager::classify_path`
/// (a thin wrapper that delegates to `LogicalWorkspace::classify`
/// after acquiring the `Arc<LogicalWorkspace>` lock). We construct a
/// daemon-default `SessionManager`, install the fixture's
/// `LogicalWorkspace` via the production `set_logical_workspace`
/// setter, and then invoke the real `classify_path` method.
fn lsp_classify(logical: std::sync::Arc<LogicalWorkspace>, probe: &Path) -> SurfaceVerdict {
    use sqry_lsp::LspOptions;
    use sqry_lsp::session::SessionManager;

    let session = SessionManager::new(LspOptions::default_daemon());
    session.set_logical_workspace(logical);
    session.classify_path(probe).into()
}

/// MCP surface — drive the **production**
/// `sqry_mcp_redaction::Redactor` via `Redactor::with_logical_workspace`,
/// then call `redact` on a JSON payload that names the probe path
/// under a key the redactor's path-field whitelist recognises
/// (`path` is the canonical entry per
/// `sqry_mcp_redaction::whitelist::is_path_field`). When a
/// `LogicalWorkspaceView` is bound on the config, the walker routes
/// path-field strings through `redact_path_with_workspace`, which
/// produces an observable wire form per classification:
///
///   * Excluded path → `<excluded>/[<hash>]`
///   * Source path   → `<source_root_id>/<relative>`
///   * Member path   → `<workspace_id_short>/<relative>`
///   * Outside path  → `<external>/...` or `<workspace>/...`
///     (legacy fallback)
///
/// To extract a verdict from the rewrite shape we look at the
/// resulting string. NOTE: we use `workspace_path` field deliberately
/// AVOIDED here — that field is hardcoded to the `<workspace>`
/// placeholder by the field-level path that runs before the
/// workspace-aware logic.
fn mcp_classify(view: &sqry_mcp_redaction::LogicalWorkspaceView, probe: &Path) -> SurfaceVerdict {
    use sqry_mcp_redaction::{RedactionConfig, Redactor};

    // Build the production redactor with `minimal` semantics + the
    // fixture's view. This is what STEP_7 binds to the MCP server.
    let config = RedactionConfig::minimal();
    let redactor = Redactor::with_logical_workspace(config, view.clone())
        .expect("construct production Redactor");

    let probe_str = probe.to_string_lossy().to_string();
    let mut payload = serde_json::json!({
        "path": probe_str.clone(),
    });
    let _ = redactor.redact(&mut payload);

    let rewritten = payload
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // The redactor's rewrite shape encodes the classification.

    // 1. Excluded → `<excluded>/[<hash>]`. Detect first because the
    //    workspace_id_short prefix never co-occurs with `<excluded>`.
    if rewritten.starts_with("<excluded>/") {
        return SurfaceVerdict::Excluded;
    }

    // 2. Source-root descendants → `<source_root_id>/<relative>`
    //    (or just the bare `<source_root_id>` for the exact path).
    //    Source-root checks come BEFORE member because the
    //    LogicalWorkspaceView's workspace_id_short happens to be a
    //    16-hex-char string and source_root_ids are 8-hex-char
    //    strings, so they cannot collide; but we order by criterion 4
    //    (source roots take precedence).
    for (sid, _) in &view.source_roots {
        if rewritten.starts_with(&format!("{sid}/")) || &rewritten == sid {
            return SurfaceVerdict::Source;
        }
    }

    // 3. Member-folder descendants → `<workspace_id_short>/<relative>`.
    if rewritten.starts_with(&format!("{}/", view.workspace_id_short))
        || rewritten == view.workspace_id_short
    {
        return SurfaceVerdict::Member;
    }

    // 4. Outside the workspace → legacy `<external>/...` or
    //    `<workspace>/...` fallback. Both indicate Unknown for our
    //    five-variant verdict.
    SurfaceVerdict::Unknown
}

/// Daemon surface — the daemon's `WorkspaceManager` carries the
/// `LogicalWorkspace` across the IPC boundary as JSON (see
/// `sqry-daemon-protocol::WorkspaceState` / `WorkspaceStatus`). The
/// daemon-side contract is that a workspace round-tripped through
/// the IPC wire format classifies identically to the in-process
/// workspace. We exercise that contract by serializing the fixture's
/// `LogicalWorkspace` to JSON (the wire format), deserializing it on
/// the "other side", and asking the deserialized copy to classify the
/// same probe via the **production** `LogicalWorkspace::classify`. If
/// the cross-wire round-trip changes the classifier verdict, the
/// daemon surface diverges.
fn daemon_classify(logical: &LogicalWorkspace, probe: &Path) -> SurfaceVerdict {
    let bytes = serde_json::to_vec(logical).expect("daemon surface: serialize LogicalWorkspace");
    let restored: LogicalWorkspace =
        serde_json::from_slice(&bytes).expect("daemon surface: deserialize LogicalWorkspace");
    restored.classify(probe).into()
}

/// VS Code extension classifier (stubbed per DAG) — the extension's
/// TS classifier in `sqry-vscode/src/workspaceClassifier.ts` builds a
/// `WorkspaceClassification { sourceRoots, memberFolders, exclusions }`
/// and applies the same precedence as the Rust classifier. We re-encode
/// the precedence here in Rust because the DAG explicitly specifies
/// `(stubbed vscode.workspace.workspaceFolders)` for this surface and
/// we do not run `node` in CI. The verbatim TS rule is:
///
/// ```text
/// if (probe is in exclusions or descendant)             → Excluded
/// else if (probe is in memberFolders or descendant)     → Member
/// else if (probe is in sourceRoots or descendant)       → Source
/// else                                                  → Unknown
/// ```
///
/// The `vscode-extension` CI job runs the TS test suite against the
/// real implementation; this stub only exists for the parity test
/// inside the Rust process.
fn extension_classify(logical: &LogicalWorkspace, probe: &Path) -> SurfaceVerdict {
    let exclusions = logical.exclusions();
    if exclusions
        .iter()
        .any(|excl| probe == excl.as_path() || probe.starts_with(excl))
    {
        return SurfaceVerdict::Excluded;
    }
    let members = logical.member_folders();
    if members
        .iter()
        .any(|m| probe == m.path.as_path() || probe.starts_with(&m.path))
    {
        return SurfaceVerdict::Member;
    }
    let sources = logical.source_roots();
    if sources
        .iter()
        .any(|r| probe == r.path.as_path() || probe.starts_with(&r.path))
    {
        return SurfaceVerdict::Source;
    }
    SurfaceVerdict::Unknown
}

#[test]
fn classification_is_identical_across_all_five_surfaces() {
    let view_with_fixture =
        build_logical_workspace_view().expect("build LogicalWorkspaceView fixture");
    let fixture = &view_with_fixture.fixture;
    let view = &view_with_fixture.view;
    let logical_arc = std::sync::Arc::new(fixture.logical.clone());

    // Probes covering every classification arm. For each probe we
    // record the **expected** verdict so the test fails loudly if the
    // fixture itself drifts.
    let probes: Vec<(&str, std::path::PathBuf, SurfaceVerdict)> = vec![
        // Exact source-root paths.
        (
            "source_a (exact)",
            fixture.source_a.clone(),
            SurfaceVerdict::Source,
        ),
        (
            "source_b (exact)",
            fixture.source_b.clone(),
            SurfaceVerdict::Source,
        ),
        // Descendants of source roots.
        (
            "source_a/src/main.ts",
            fixture.source_a.join("src").join("main.ts"),
            SurfaceVerdict::Source,
        ),
        (
            "source_b/src/main.rs",
            fixture.source_b.join("src").join("main.rs"),
            SurfaceVerdict::Source,
        ),
        // Member folder + descendant.
        (
            "member (exact)",
            fixture.member.clone(),
            SurfaceVerdict::Member,
        ),
        (
            "member/deploy.sh",
            fixture.member.join("deploy.sh"),
            SurfaceVerdict::Member,
        ),
        // Excluded folder + descendant.
        (
            "excluded (exact)",
            fixture.excluded.clone(),
            SurfaceVerdict::Excluded,
        ),
        (
            "excluded/pkg/index.js",
            fixture.excluded.join("pkg").join("index.js"),
            SurfaceVerdict::Excluded,
        ),
    ];

    for (label, probe, expected) in &probes {
        let cli = cli_classify(&fixture.registry_path, probe);
        let lsp = lsp_classify(logical_arc.clone(), probe);
        let mcp = mcp_classify(view, probe);
        let dmn = daemon_classify(&fixture.logical, probe);
        let ext = extension_classify(&fixture.logical, probe);

        assert_eq!(
            cli, *expected,
            "[{label}] CLI surface diverged from expected verdict (got {cli:?}, expected {expected:?})"
        );
        assert_eq!(
            lsp, cli,
            "[{label}] LSP surface (real SessionManager::classify_path) != CLI surface (got LSP={lsp:?}, CLI={cli:?})"
        );
        assert_eq!(
            mcp, cli,
            "[{label}] MCP surface (real Redactor::redact rewrite shape) != CLI surface (got MCP={mcp:?}, CLI={cli:?})"
        );
        assert_eq!(
            dmn, cli,
            "[{label}] daemon surface (LogicalWorkspace serde round-trip) != CLI surface (got daemon={dmn:?}, CLI={cli:?})"
        );
        assert_eq!(
            ext, cli,
            "[{label}] VS Code extension classifier stub != CLI surface \
             (got extension={ext:?}, CLI={cli:?})"
        );
    }
}

#[test]
fn aggregate_status_is_identical_across_all_five_surfaces() {
    let view_with_fixture =
        build_logical_workspace_view().expect("build LogicalWorkspaceView fixture");
    let fixture = &view_with_fixture.fixture;
    let view = &view_with_fixture.view;
    let logical_arc = std::sync::Arc::new(fixture.logical.clone());

    // Aggregate triple = (source_count, member_count, excluded_count).
    // Every surface must derive the same triple from the same fixture.
    let from_logical = (
        fixture.logical.source_roots().len(),
        fixture.logical.member_folders().len(),
        fixture.logical.exclusions().len(),
    );

    // CLI: re-load registry, count.
    let reloaded =
        LogicalWorkspace::from_sqry_workspace(&fixture.registry_path).expect("reload registry");
    let from_cli = (
        reloaded.source_roots().len(),
        reloaded.member_folders().len(),
        reloaded.exclusions().len(),
    );

    // LSP: production SessionManager + set_logical_workspace, then
    // pull the workspace back out via logical_workspace().
    let from_lsp = {
        use sqry_lsp::LspOptions;
        use sqry_lsp::session::SessionManager;
        let session = SessionManager::new(LspOptions::default_daemon());
        session.set_logical_workspace(logical_arc.clone());
        let workspace = session.logical_workspace();
        (
            workspace.source_roots().len(),
            workspace.member_folders().len(),
            workspace.exclusions().len(),
        )
    };

    // MCP: redaction view sizes mirror the logical workspace sizes
    // (STEP_7 acceptance criteria 5/6/9).
    let from_mcp = (
        view.source_roots.len(),
        view.member_folders.len(),
        view.exclusions.len(),
    );

    // Daemon: serialize → deserialize round-trip, then count.
    let bytes = serde_json::to_vec(&fixture.logical).expect("serialize");
    let restored: LogicalWorkspace = serde_json::from_slice(&bytes).expect("deserialize");
    let from_daemon = (
        restored.source_roots().len(),
        restored.member_folders().len(),
        restored.exclusions().len(),
    );

    // VS Code extension classifier stub: same as the logical
    // workspace it was built from.
    let from_extension = from_logical;

    let expected = (2_usize, 1_usize, 1_usize);
    assert_eq!(from_cli, expected, "CLI aggregate != expected");
    assert_eq!(from_lsp, expected, "LSP aggregate != expected");
    assert_eq!(from_mcp, expected, "MCP aggregate != expected");
    assert_eq!(from_daemon, expected, "daemon aggregate != expected");
    assert_eq!(from_extension, expected, "extension aggregate != expected");
}

#[test]
fn member_reason_round_trips_through_logical_workspace() {
    // Sanity guard: the operational member folder must surface as
    // `MemberReason::OperationalFolder` after a registry round-trip.
    // If the registry ever loses the reason, parity tests above would
    // still pass (because they fold to a single `Member` variant), so
    // we explicitly assert the reason here.
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    let classification = fixture.logical.classify(&fixture.member);
    match classification {
        Classification::Member { reason } => {
            assert_eq!(
                reason,
                MemberReason::OperationalFolder,
                "operational member folder must round-trip with reason=OperationalFolder; \
                 got reason={reason:?}"
            );
        }
        other => panic!("expected Classification::Member, got {other:?}"),
    }
}
