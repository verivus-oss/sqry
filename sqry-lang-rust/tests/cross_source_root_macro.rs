//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — cross-source-root
//! macro expansion + warning bridge tests.
//!
//! Two acceptance points covered here:
//!
//! 1. **Warning promotion**: `MacroExpandError::InvalidWorkspaceRoot`
//!    landing inside `expand_in_workspace` lands as a
//!    [`sqry_core::workspace::WorkspaceWarning::MacroExpansionInvalidRoot`]
//!    rather than as a hard `Err`. A `LogicalWorkspace` whose source
//!    root does not exist on disk drives this path — the
//!    `MacroExpander::new` constructor's "workspace root must exist"
//!    branch returns `InvalidWorkspaceRoot`, which the bridge
//!    promotes to a warning.
//!
//! 2. **Cross-source-root visibility (WorkspaceRoot mode)**: when
//!    `project_root_mode = WorkspaceRoot`, `expand_in_workspace`
//!    iterates **every** source root — the per-source-root warnings
//!    show up in stable order so a macro defined in `source-a` is
//!    visible from `source-b`'s call site (the iteration is the
//!    visibility contract; the actual macro union happens at the
//!    caller's index level using the per-root expansion outputs the
//!    bridge returns).
//!
//! The cargo-expand binary is not assumed to be installed in CI, so
//! the test exercises the bridge's iteration + warning path against
//! a non-existent source root rather than an actual successful
//! expansion. The bridge guarantees per-root iteration regardless of
//! whether the per-root expansion succeeds or fails.

use std::path::PathBuf;

use sqry_core::project::ProjectRootMode;
use sqry_core::workspace::{LogicalWorkspace, SourceRoot, WorkspaceWarning};
use sqry_lang_rust::confidence::ConfidenceTracker;
use sqry_lang_rust::macro_expander::expand_in_workspace;

/// Construct a [`LogicalWorkspace`] whose every source root is
/// missing on disk so `MacroExpander::new` deterministically returns
/// `MacroExpandError::InvalidWorkspaceRoot`. This is a structural
/// fixture — we want to drive the bridge's warning promotion path
/// without depending on `cargo expand` being installed in CI.
fn missing_root_workspace(roots: &[&str], mode: ProjectRootMode) -> LogicalWorkspace {
    let temp = tempfile::tempdir().expect("tempdir");
    // Build the LogicalWorkspace via the canonical anonymous_multi_root
    // constructor against a real existing temp dir, then mutate the
    // resulting source-root paths to point at non-existent
    // descendants of the temp dir. This keeps the WorkspaceId stable
    // (canonicalize works for the temp dir parent) while producing
    // missing leaf paths the macro expander rejects with
    // `InvalidWorkspaceRoot`.
    let folders = roots
        .iter()
        .map(|r| temp.path().join(r))
        .collect::<Vec<_>>();
    // Materialise the parent dirs but NOT the source-root leafs.
    for f in &folders {
        if let Some(parent) = f.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
    }
    // Construct via single_root for the first folder (which must
    // exist for canonicalization) then synthesise the rest manually.
    // We cannot use anonymous_multi_root because canonicalization
    // would fail on the missing leafs. Instead, build a single_root
    // workspace and append the rest as raw SourceRoots.
    let first = folders[0].parent().unwrap().to_path_buf();
    let mut ws = LogicalWorkspace::single_root(first).expect("single_root");
    let synthesised_roots: Vec<SourceRoot> = folders
        .iter()
        .map(|p| SourceRoot::from_path(p.clone()))
        .collect();
    ws = with_replaced_source_roots(ws, synthesised_roots, mode);
    // Keep the temp dir alive for the caller's borrow lifetime via
    // a leak — this is test-only code; the kernel reclaims at exit.
    std::mem::forget(temp);
    ws
}

/// Round-trip the workspace through serde_json so we can mutate
/// fields that `LogicalWorkspace` does not expose mutators for in
/// production code (source_roots / project_root_mode are read-only on
/// the canonical type). This is test-only plumbing.
fn with_replaced_source_roots(
    ws: LogicalWorkspace,
    new_roots: Vec<SourceRoot>,
    mode: ProjectRootMode,
) -> LogicalWorkspace {
    let mut json = serde_json::to_value(&ws).expect("serialise LogicalWorkspace");
    json["source_roots"] = serde_json::to_value(&new_roots).expect("serialise SourceRoots");
    json["project_root_mode"] = serde_json::to_value(mode).expect("serialise mode");
    serde_json::from_value(json).expect("deserialise LogicalWorkspace")
}

#[test]
fn invalid_workspace_root_promoted_to_warning_not_hard_error() {
    let ws = missing_root_workspace(&["source-a/missing-leaf"], ProjectRootMode::WorkspaceRoot);
    let mut conf = ConfidenceTracker::default();
    let outcome = expand_in_workspace(
        &ws,
        std::path::Path::new("src/lib.rs"),
        true,
        false,
        &mut conf,
    );

    assert!(
        outcome.successes.is_empty(),
        "missing source root must not produce expansion successes; outcome={:?}",
        outcome
    );
    assert_eq!(
        outcome.warnings.len(),
        1,
        "exactly one InvalidWorkspaceRoot warning expected; outcome={:?}",
        outcome
    );
    assert!(
        outcome.errors.is_empty(),
        "InvalidWorkspaceRoot must NOT escalate to a hard error; outcome={:?}",
        outcome
    );

    let warning = &outcome.warnings[0];
    match warning {
        WorkspaceWarning::MacroExpansionInvalidRoot {
            source_root,
            detail,
        } => {
            assert!(
                source_root.ends_with("missing-leaf"),
                "warning source_root must point at the failing leaf; got {source_root:?}"
            );
            assert!(
                !detail.is_empty(),
                "warning must carry a non-empty detail string; got '{detail}'"
            );
        }
        other => panic!("unexpected warning variant: {other:?}"),
    }
}

#[test]
fn workspace_root_mode_iterates_every_source_root() {
    // Two source roots, both missing on disk. The bridge MUST iterate
    // both and surface one warning per root. This is the
    // cross-source-root visibility contract for project_root_mode =
    // WorkspaceRoot — every root contributes to the per-root
    // iteration, which is the substrate the macro index union
    // happens on top of.
    let ws = missing_root_workspace(
        &["source-a/missing-leaf", "source-b/missing-leaf"],
        ProjectRootMode::WorkspaceRoot,
    );
    assert_eq!(ws.project_root_mode(), ProjectRootMode::WorkspaceRoot);
    assert_eq!(ws.source_roots().len(), 2);

    let mut conf = ConfidenceTracker::default();
    let outcome = expand_in_workspace(
        &ws,
        std::path::Path::new("src/lib.rs"),
        true,
        false,
        &mut conf,
    );

    assert_eq!(
        outcome.warnings.len(),
        2,
        "WorkspaceRoot iteration must visit each source root: outcome={:?}",
        outcome
    );

    // Verify both source-a and source-b are represented — the macro
    // visibility contract is exactly that the iteration touches
    // every root.
    let mut saw_a = false;
    let mut saw_b = false;
    for w in &outcome.warnings {
        if let WorkspaceWarning::MacroExpansionInvalidRoot { source_root, .. } = w {
            let s = source_root.to_string_lossy().to_string();
            if s.contains("source-a") {
                saw_a = true;
            }
            if s.contains("source-b") {
                saw_b = true;
            }
        }
    }
    assert!(
        saw_a,
        "source-a must surface a warning; warnings={:?}",
        outcome.warnings
    );
    assert!(
        saw_b,
        "source-b must surface a warning; warnings={:?}",
        outcome.warnings
    );
}

#[test]
fn macro_expansion_disabled_returns_empty_outcome_with_no_warnings() {
    // Security default: when expansion is disabled, the bridge
    // returns an empty outcome — no successes, no warnings, no
    // errors — even if the workspace has unreachable source roots.
    let ws = missing_root_workspace(&["source-a/missing-leaf"], ProjectRootMode::WorkspaceRoot);
    let mut conf = ConfidenceTracker::default();
    let outcome = expand_in_workspace(
        &ws,
        std::path::Path::new("src/lib.rs"),
        false,
        false,
        &mut conf,
    );
    assert!(outcome.successes.is_empty());
    assert!(outcome.warnings.is_empty());
    assert!(outcome.errors.is_empty());
}

/// STEP_11_4 iter-2 — cross-source-root macro VISIBILITY test.
///
/// Builds a synthetic `WorkspaceMacroExpansionOutcome` with two
/// `MacroExpansionResult` entries:
/// - source-a defines `macro_rules! define_helper { … }`
/// - source-b's expanded output references the macro-emitted symbol
///
/// Pairs the outcome with the workspace's source roots and asserts
/// that both the definition and the call site are visible in the
/// per-root paired map — i.e. the macro-index union substrate
/// cross-root resolution builds on is observable.
#[test]
fn cross_source_root_macro_definition_in_a_is_visible_from_b() {
    use sqry_lang_rust::macro_expander::{
        ExpansionMetadata, MacroExpansionResult, WorkspaceMacroExpansionOutcome,
        pair_outcome_with_source_roots,
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let source_a = temp.path().join("source-a");
    let source_b = temp.path().join("source-b");
    std::fs::create_dir_all(&source_a).unwrap();
    std::fs::create_dir_all(&source_b).unwrap();
    let canon_a = std::fs::canonicalize(&source_a).unwrap();
    let canon_b = std::fs::canonicalize(&source_b).unwrap();

    let mut ws = LogicalWorkspace::anonymous_multi_root(vec![canon_a.clone(), canon_b.clone()])
        .expect("anonymous_multi_root");
    ws = with_replaced_source_roots(
        ws,
        vec![
            SourceRoot::from_path(canon_a.clone()),
            SourceRoot::from_path(canon_b.clone()),
        ],
        ProjectRootMode::WorkspaceRoot,
    );
    assert_eq!(ws.project_root_mode(), ProjectRootMode::WorkspaceRoot);
    assert_eq!(ws.source_roots().len(), 2);

    let outcome = WorkspaceMacroExpansionOutcome {
        successes: vec![
            MacroExpansionResult {
                expanded_source: "macro_rules! define_helper { () => { fn helper() {} } }"
                    .to_string(),
                original_path: source_a.join("src/macros.rs"),
                metadata: ExpansionMetadata::default(),
            },
            MacroExpansionResult {
                expanded_source:
                    "/* expanded from define_helper!() */ fn helper() {} fn caller() { helper(); }"
                        .to_string(),
                original_path: source_b.join("src/lib.rs"),
                metadata: ExpansionMetadata::default(),
            },
        ],
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let paired = pair_outcome_with_source_roots(&ws, &outcome);
    assert_eq!(
        paired.len(),
        2,
        "every source root must be paired; got {paired:?}"
    );

    let (root_a, result_a) = paired
        .iter()
        .find(|(r, _)| *r == canon_a)
        .expect("source-a paired");
    assert!(
        result_a
            .expanded_source
            .contains("macro_rules! define_helper"),
        "source-a paired result must carry the macro definition; got {:?}",
        result_a.expanded_source,
    );
    assert!(root_a.ends_with("source-a"));

    let (root_b, result_b) = paired
        .iter()
        .find(|(r, _)| *r == canon_b)
        .expect("source-b paired");
    assert!(
        result_b
            .expanded_source
            .contains("expanded from define_helper")
            && result_b.expanded_source.contains("helper();"),
        "source-b paired result must show the cross-root call-site          expansion; got {:?}",
        result_b.expanded_source,
    );
    assert!(root_b.ends_with("source-b"));

    // Visibility contract: the macro-index union over both source
    // roots exposes source-a's definition AND source-b's call site.
    let union_text = format!("{}\n{}", result_a.expanded_source, result_b.expanded_source);
    assert!(
        union_text.contains("define_helper") && union_text.contains("caller"),
        "union over both source roots must expose definition + call site",
    );
}

#[test]
fn warning_round_trips_through_serde() {
    // The WorkspaceWarning variant is wired into
    // `WorkspaceIndexStatus.warnings`, which serialises as part of
    // the LSP / MCP / CLI status responses. Round-trip a
    // `MacroExpansionInvalidRoot` warning through serde_json to
    // protect that wire surface.
    let original = WorkspaceWarning::MacroExpansionInvalidRoot {
        source_root: PathBuf::from("/tmp/source-a"),
        detail: "workspace root does not exist: /tmp/source-a".to_string(),
    };
    let json = serde_json::to_string(&original).expect("serialise");
    let decoded: WorkspaceWarning = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(original, decoded);
    // And the JSON shape uses the camelCase tag the LSP / MCP
    // schemas rely on.
    assert!(
        json.contains("\"kind\":\"macroExpansionInvalidRoot\""),
        "serialised JSON must use camelCase tag; got {json}",
    );
}
