//! Task 7 Phase 7a — `PendingRebuild::coalesce_with` algebra matrix.
//!
//! 11 assertion groups per the iter-2 design review request:
//!
//! 1.  File union with duplicate collapse + deterministic order.
//! 2.  OR of `git_state_changed`.
//! 3.  Full-rebuild dominance in both orderings (`BranchSwitch ⊕ Noise`
//!     and `Noise ⊕ BranchSwitch` both canonicalise to
//!     `Some(TreeDiverged)`).
//! 4.  `TreeDiverged ⊕ LocalCommit == Some(TreeDiverged)`.
//! 5.  Non-full later-wins: `LocalCommit ⊕ Noise == Some(Noise)`.
//! 6.  Non-full reversed: `Noise ⊕ LocalCommit == Some(LocalCommit)`.
//! 7.  Same-class idempotence across all four variants.
//! 8.  `None ⊕ None == None`.
//! 9.  Absorb-None from either side.
//! 10. `enqueued_at = max(a, b)`.
//! 11. Associativity and commutativity-under-`requires_full_rebuild()`.

use std::{path::PathBuf, time::Instant};

use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::PendingRebuild;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn pending(
    files: Vec<PathBuf>,
    git_state_changed: bool,
    git_change_class: Option<GitChangeClass>,
    enqueued_at: Instant,
) -> PendingRebuild {
    PendingRebuild {
        changes: ChangeSet {
            changed_files: files,
            git_state_changed,
            git_change_class,
        },
        enqueued_at,
        git_state_at_enqueue: None,
    }
}

fn bare(git_change_class: Option<GitChangeClass>) -> PendingRebuild {
    pending(
        Vec::new(),
        git_change_class.is_some(),
        git_change_class,
        Instant::now(),
    )
}

// ---------------------------------------------------------------------------
// Group 1 — file union with duplicate collapse + deterministic order
// ---------------------------------------------------------------------------

#[test]
fn file_union_dedups_and_sorts_deterministically() {
    let t = Instant::now();
    let a = pending(
        vec![
            PathBuf::from("/w/zeta.rs"),
            PathBuf::from("/w/alpha.rs"),
            PathBuf::from("/w/alpha.rs"), // intra-side duplicate
        ],
        false,
        None,
        t,
    );
    let b = pending(
        vec![
            PathBuf::from("/w/alpha.rs"), // cross-side duplicate
            PathBuf::from("/w/beta.rs"),
        ],
        false,
        None,
        t,
    );

    let merged = a.coalesce_with(b);

    assert_eq!(
        merged.changes.changed_files,
        vec![
            PathBuf::from("/w/alpha.rs"),
            PathBuf::from("/w/beta.rs"),
            PathBuf::from("/w/zeta.rs"),
        ],
        "merged file list must dedup and be sorted",
    );
}

// ---------------------------------------------------------------------------
// Group 2 — OR of git_state_changed
// ---------------------------------------------------------------------------

#[test]
fn git_state_changed_is_or_of_sides() {
    let t = Instant::now();
    let cases = [
        (false, false, false),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ];
    for (a_git, b_git, expected) in cases {
        let a = pending(Vec::new(), a_git, None, t);
        let b = pending(Vec::new(), b_git, None, t);
        assert_eq!(
            a.coalesce_with(b).changes.git_state_changed,
            expected,
            "git_state_changed merge failed for ({a_git}, {b_git})",
        );
    }
}

// ---------------------------------------------------------------------------
// Group 3 — full-rebuild dominance, both orderings
// ---------------------------------------------------------------------------

#[test]
fn branch_switch_dominates_noise_in_both_orders() {
    let forward =
        bare(Some(GitChangeClass::BranchSwitch)).coalesce_with(bare(Some(GitChangeClass::Noise)));
    assert_eq!(
        forward.changes.git_change_class,
        Some(GitChangeClass::TreeDiverged)
    );

    let reverse =
        bare(Some(GitChangeClass::Noise)).coalesce_with(bare(Some(GitChangeClass::BranchSwitch)));
    assert_eq!(
        reverse.changes.git_change_class,
        Some(GitChangeClass::TreeDiverged)
    );
}

// ---------------------------------------------------------------------------
// Group 4 — TreeDiverged dominates LocalCommit
// ---------------------------------------------------------------------------

#[test]
fn tree_diverged_dominates_local_commit() {
    let merged = bare(Some(GitChangeClass::TreeDiverged))
        .coalesce_with(bare(Some(GitChangeClass::LocalCommit)));
    assert_eq!(
        merged.changes.git_change_class,
        Some(GitChangeClass::TreeDiverged)
    );

    let reverse = bare(Some(GitChangeClass::LocalCommit))
        .coalesce_with(bare(Some(GitChangeClass::TreeDiverged)));
    assert_eq!(
        reverse.changes.git_change_class,
        Some(GitChangeClass::TreeDiverged)
    );
}

// ---------------------------------------------------------------------------
// Group 5 — non-full later-wins
// ---------------------------------------------------------------------------

#[test]
fn non_full_later_wins_local_commit_over_noise() {
    let merged =
        bare(Some(GitChangeClass::LocalCommit)).coalesce_with(bare(Some(GitChangeClass::Noise)));
    assert_eq!(merged.changes.git_change_class, Some(GitChangeClass::Noise));
}

// ---------------------------------------------------------------------------
// Group 6 — reversed non-full ordering
// ---------------------------------------------------------------------------

#[test]
fn non_full_later_wins_reversed_noise_over_local_commit() {
    let merged =
        bare(Some(GitChangeClass::Noise)).coalesce_with(bare(Some(GitChangeClass::LocalCommit)));
    assert_eq!(
        merged.changes.git_change_class,
        Some(GitChangeClass::LocalCommit)
    );
}

// ---------------------------------------------------------------------------
// Group 7 — same-class idempotence across all four variants
// ---------------------------------------------------------------------------

#[test]
fn same_class_idempotent_across_all_variants() {
    for class in [
        GitChangeClass::BranchSwitch,
        GitChangeClass::TreeDiverged,
        GitChangeClass::LocalCommit,
        GitChangeClass::Noise,
    ] {
        let merged = bare(Some(class)).coalesce_with(bare(Some(class)));
        // BranchSwitch + BranchSwitch triggers full-rebuild dominance →
        // canonical TreeDiverged (both sides are full triggers).
        // Same for TreeDiverged + TreeDiverged.
        // LocalCommit / Noise (non-full) self-merge returns the later
        // (same) class, i.e. itself.
        let expected = if class.requires_full_rebuild() {
            GitChangeClass::TreeDiverged
        } else {
            class
        };
        assert_eq!(
            merged.changes.git_change_class,
            Some(expected),
            "same-class idempotence failed for {class:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// Group 8 — None ⊕ None
// ---------------------------------------------------------------------------

#[test]
fn none_merged_with_none_stays_none() {
    let a = bare(None);
    let b = bare(None);
    let merged = a.coalesce_with(b);
    assert_eq!(merged.changes.git_change_class, None);
}

// ---------------------------------------------------------------------------
// Group 9 — absorb-None from either side
// ---------------------------------------------------------------------------

#[test]
fn absorb_none_from_either_side() {
    for class in [
        GitChangeClass::BranchSwitch,
        GitChangeClass::TreeDiverged,
        GitChangeClass::LocalCommit,
        GitChangeClass::Noise,
    ] {
        // None ⊕ Some(x) — for non-full x, merged == Some(x).
        //                 for full x, full-rebuild dominance → Some(TreeDiverged).
        let expected = if class.requires_full_rebuild() {
            GitChangeClass::TreeDiverged
        } else {
            class
        };

        let none_left = bare(None).coalesce_with(bare(Some(class)));
        assert_eq!(
            none_left.changes.git_change_class,
            Some(expected),
            "None ⊕ Some({class:?}) failed",
        );

        let none_right = bare(Some(class)).coalesce_with(bare(None));
        assert_eq!(
            none_right.changes.git_change_class,
            Some(expected),
            "Some({class:?}) ⊕ None failed",
        );
    }
}

// ---------------------------------------------------------------------------
// Group 10 — enqueued_at = max(a, b)
// ---------------------------------------------------------------------------

#[test]
fn enqueued_at_is_max_of_sides() {
    let earlier = Instant::now();
    // Spin briefly so `later` is strictly greater than `earlier` even
    // on systems with coarse clock resolution.
    while Instant::now() == earlier {
        std::hint::spin_loop();
    }
    let later = Instant::now();

    let a = pending(Vec::new(), false, None, earlier);
    let b = pending(Vec::new(), false, None, later);

    // forward direction
    assert_eq!(a.clone().coalesce_with(b.clone()).enqueued_at, later);
    // reversed direction
    assert_eq!(b.coalesce_with(a).enqueued_at, later);
}

// ---------------------------------------------------------------------------
// Group 11 — associativity + commutativity of requires_full_rebuild predicate
// ---------------------------------------------------------------------------

#[test]
fn associativity_holds_for_file_and_bool_axes() {
    let t = Instant::now();
    let a = pending(vec![PathBuf::from("/w/a.rs")], false, None, t);
    let b = pending(vec![PathBuf::from("/w/b.rs")], true, None, t);
    let c = pending(vec![PathBuf::from("/w/c.rs")], false, None, t);

    let ab_c = a.clone().coalesce_with(b.clone()).coalesce_with(c.clone());
    let a_bc = a.coalesce_with(b.coalesce_with(c));

    assert_eq!(
        ab_c.changes.changed_files, a_bc.changes.changed_files,
        "associativity broken on file union",
    );
    assert_eq!(
        ab_c.changes.git_state_changed, a_bc.changes.git_state_changed,
        "associativity broken on git_state_changed OR",
    );
    assert_eq!(
        ab_c.enqueued_at, a_bc.enqueued_at,
        "associativity broken on enqueued_at max",
    );
}

#[test]
fn requires_full_rebuild_predicate_is_symmetric_under_merge() {
    let t = Instant::now();
    // Any pair of classes that includes a full-rebuild trigger must
    // agree on requires_full_rebuild() regardless of merge order.
    let pairs = [
        (
            Some(GitChangeClass::BranchSwitch),
            Some(GitChangeClass::Noise),
        ),
        (
            Some(GitChangeClass::TreeDiverged),
            Some(GitChangeClass::LocalCommit),
        ),
        (Some(GitChangeClass::BranchSwitch), None),
        (None, Some(GitChangeClass::TreeDiverged)),
    ];
    for (left, right) in pairs {
        let a = pending(Vec::new(), left.is_some(), left, t);
        let b = pending(Vec::new(), right.is_some(), right, t);

        let forward = a.clone().coalesce_with(b.clone());
        let reverse = b.coalesce_with(a);

        assert_eq!(
            forward.changes.requires_full_rebuild(),
            reverse.changes.requires_full_rebuild(),
            "commutativity broken on requires_full_rebuild() for ({left:?}, {right:?})",
        );
    }
}

// ---------------------------------------------------------------------------
// Group 12 — git_state_at_enqueue merge rule (Task 7 Phase 7b2)
// ---------------------------------------------------------------------------
//
// The absorb-None, later-wins merge rule mirrors the Option-merge
// semantics already used for `git_change_class` under non-full-rebuild
// cases. These four cases pin the merge algebra:
//
//   (None, None)       -> None
//   (None, Some(b))    -> Some(b)
//   (Some(a), None)    -> Some(a)
//   (Some(a), Some(b)) -> Some(b)   (later wins)

fn state_with_ref(head_ref: &str) -> sqry_core::watch::LastIndexedGitState {
    sqry_core::watch::LastIndexedGitState {
        head_ref: Some(head_ref.to_string()),
        head_commit_oid: Some(format!("commit-{head_ref}")),
        head_tree_oid: Some(format!("tree-{head_ref}")),
    }
}

fn pending_with_git_state(
    git_state_at_enqueue: Option<sqry_core::watch::LastIndexedGitState>,
) -> PendingRebuild {
    PendingRebuild {
        changes: ChangeSet {
            changed_files: Vec::new(),
            git_state_changed: false,
            git_change_class: None,
        },
        enqueued_at: Instant::now(),
        git_state_at_enqueue,
    }
}

#[test]
fn coalesce_git_state_none_both_sides_yields_none() {
    let merged = pending_with_git_state(None).coalesce_with(pending_with_git_state(None));
    assert!(
        merged.git_state_at_enqueue.is_none(),
        "None ⊕ None must remain None"
    );
}

#[test]
fn coalesce_git_state_absorb_none_from_earlier() {
    // self = None, later = Some(b)  →  merged.git_state_at_enqueue == Some(b)
    let later_state = state_with_ref("refs/heads/feat");
    let merged = pending_with_git_state(None)
        .coalesce_with(pending_with_git_state(Some(later_state.clone())));
    assert_eq!(
        merged.git_state_at_enqueue,
        Some(later_state),
        "None ⊕ Some(b) must preserve Some(b)"
    );
}

#[test]
fn coalesce_git_state_absorb_none_from_later() {
    // self = Some(a), later = None  →  merged.git_state_at_enqueue == Some(a)
    let earlier_state = state_with_ref("refs/heads/main");
    let merged = pending_with_git_state(Some(earlier_state.clone()))
        .coalesce_with(pending_with_git_state(None));
    assert_eq!(
        merged.git_state_at_enqueue,
        Some(earlier_state),
        "Some(a) ⊕ None must preserve Some(a)"
    );
}

#[test]
fn coalesce_git_state_later_wins_when_both_some() {
    // self = Some(a), later = Some(b)  →  merged == Some(b)
    let earlier = state_with_ref("refs/heads/main");
    let later = state_with_ref("refs/heads/feat-x");
    let merged = pending_with_git_state(Some(earlier))
        .coalesce_with(pending_with_git_state(Some(later.clone())));
    assert_eq!(
        merged.git_state_at_enqueue,
        Some(later),
        "Some(a) ⊕ Some(b) must yield Some(b) (later wins)"
    );
}
