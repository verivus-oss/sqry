//! `multi_root_workspace.rs` — STEP_11 dedicated coverage of
//! multi-root logical workspaces (two source roots, one member folder,
//! one excluded folder), exercising the path that
//! `LogicalWorkspace::from_sqry_workspace` shares with
//! `LogicalWorkspace::from_code_workspace` /
//! `LogicalWorkspace::anonymous_multi_root`.
//!
//! This test asserts:
//!   * The workspace identity is stable across a save → reload round
//!     trip (the WorkspaceId of the registry-loaded workspace must
//!     equal the WorkspaceId of the workspace immediately after first
//!     load — no drift).
//!   * Every source-root path is is_source_root()-true; every member
//!     and excluded path is is_source_root()-false.
//!   * Classification is `Source` for source-root descendants,
//!     `Member { reason: OperationalFolder }` for member descendants,
//!     `Excluded` for excluded descendants, and `Unknown` for paths
//!     outside the workspace.
//!   * The MCP-side `LogicalWorkspaceView` projection preserves the
//!     same source-root / member / exclusion counts.

use std::path::PathBuf;

use sqry_core::workspace::{Classification, LogicalWorkspace, MemberReason};
use sqry_integration_tests::fixtures::{
    build_logical_workspace_view, build_two_source_one_member_one_excluded,
};

#[test]
fn workspace_id_is_stable_across_save_reload() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    let original_id = *fixture.logical.workspace_id();

    let reloaded = LogicalWorkspace::from_sqry_workspace(&fixture.registry_path)
        .expect("reload .sqry-workspace");
    assert_eq!(
        reloaded.workspace_id(),
        &original_id,
        "WorkspaceId drifted across save/reload — \
         registry persistence is not deterministic"
    );
}

#[test]
fn is_source_root_returns_true_only_for_source_roots() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    assert!(
        fixture.logical.is_source_root(&fixture.source_a),
        "source_a (canonical) must be is_source_root()"
    );
    assert!(
        fixture.logical.is_source_root(&fixture.source_b),
        "source_b (canonical) must be is_source_root()"
    );
    assert!(
        !fixture.logical.is_source_root(&fixture.member),
        "operational member folder must NOT be is_source_root()"
    );
    assert!(
        !fixture.logical.is_source_root(&fixture.excluded),
        "excluded folder must NOT be is_source_root()"
    );
}

#[test]
fn classification_descendants_inherit_owner_kind() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");

    // Source-root descendants → Source.
    let probes_source = [
        fixture.source_a.join("src"),
        fixture.source_a.join("src").join("main.ts"),
        fixture.source_b.join("src").join("main.rs"),
    ];
    for probe in &probes_source {
        assert!(
            matches!(fixture.logical.classify(probe), Classification::Source),
            "{} must classify as Source",
            probe.display()
        );
    }

    // Member descendants → Member { reason: OperationalFolder }.
    let probes_member = [fixture.member.clone(), fixture.member.join("deploy.sh")];
    for probe in &probes_member {
        match fixture.logical.classify(probe) {
            Classification::Member { reason } => assert_eq!(
                reason,
                MemberReason::OperationalFolder,
                "{} must classify as Member with OperationalFolder reason; got {:?}",
                probe.display(),
                reason
            ),
            other => panic!(
                "{} expected Classification::Member, got {:?}",
                probe.display(),
                other
            ),
        }
    }

    // Excluded descendants → Excluded.
    let probes_excluded = [
        fixture.excluded.clone(),
        fixture.excluded.join("pkg"),
        fixture.excluded.join("pkg").join("index.js"),
    ];
    for probe in &probes_excluded {
        assert!(
            matches!(fixture.logical.classify(probe), Classification::Excluded),
            "{} must classify as Excluded",
            probe.display()
        );
    }

    // Outside the workspace entirely → Unknown.
    let outside = PathBuf::from("/var/empty/definitely-not-in-fixture");
    assert!(
        matches!(fixture.logical.classify(&outside), Classification::Unknown),
        "{} must classify as Unknown",
        outside.display()
    );
}

#[test]
fn mcp_view_projection_preserves_counts_for_multi_root() {
    let view_with_fixture = build_logical_workspace_view().expect("fixture + view");
    let view = &view_with_fixture.view;

    assert_eq!(
        view.source_roots.len(),
        2,
        "MCP view must list both source roots"
    );
    assert_eq!(
        view.member_folders.len(),
        1,
        "MCP view must list the operational member folder"
    );
    assert_eq!(
        view.exclusions.len(),
        1,
        "MCP view must list the excluded folder"
    );

    // The view's `is_excluded` rule must match the underlying logical
    // workspace's `Classification::Excluded`.
    let fixture = &view_with_fixture.fixture;
    assert!(view.is_excluded(&fixture.excluded));
    assert!(view.is_excluded(&fixture.excluded.join("pkg").join("index.js")));
    assert!(!view.is_excluded(&fixture.source_a));
    assert!(!view.is_excluded(&fixture.member));
}
