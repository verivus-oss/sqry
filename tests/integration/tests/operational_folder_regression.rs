//! `operational_folder_regression.rs` — STEP_11 regression coverage
//! for the original "No index found" / "No lock file found" bug class
//! that motivated the workspace-aware-cross-repo workstream.
//!
//! Renamed (per DAG STEP_11 critical_decisions) from the operator
//! fixture name to the generic `operational_folder_regression` so the
//! public test name does not leak the operator's wrapper identity.
//!
//! Acceptance contract (verbatim from the DAG):
//!
//! > operational_folder_regression.rs synthesizes 2 source + 1
//! > operational + 1 excluded fixture and asserts ZERO `No index
//! > found` log lines for member folders
//!
//! The original bug surfaced when the LSP server's auto-index loop
//! enumerated every `vscode.workspace.workspaceFolders` entry and ran
//! a per-folder filesystem probe to see whether `.sqry/graph/snapshot.sqry`
//! existed. Operational/member folders (e.g. `tools/operational`) had
//! no such snapshot and the probe emitted `No index found at <path>`
//! into the LSP outputChannel — once per member folder, on every
//! workspace open.
//!
//! Acceptance is split into three asserts:
//!   1. `LogicalWorkspace::classify` returns `Member { reason:
//!      OperationalFolder }` for the operational folder (so any caller
//!      that gates on classification can short-circuit before probing).
//!   2. The simulated probe loop (a tiny in-test helper that walks the
//!      workspace folders and emits a log line **only when classification
//!      is Source AND no snapshot exists**) emits ZERO `No index found`
//!      lines for the member folder.
//!   3. The simulated probe loop emits exactly TWO `No index found`
//!      aggregate prompts (one per source root with no snapshot file)
//!      — this is the legitimate "user has not run sqry index yet"
//!      surface that the original bug was masking.

use std::path::Path;

use sqry_core::workspace::{Classification, LogicalWorkspace};
use sqry_integration_tests::fixtures::build_two_source_one_member_one_excluded;

/// In-test stand-in for the LSP / extension auto-index probe loop.
/// This mirrors the structure of the bug site (per-folder filesystem
/// probe) WITH the classifier gate that STEP_4 added in front. The
/// returned `Vec<String>` is the captured outputChannel contents; the
/// test asserts against it.
fn simulated_probe_loop(logical: &LogicalWorkspace, all_folders: &[&Path]) -> Vec<String> {
    let mut log = Vec::new();
    for folder in all_folders {
        // STEP_4 contract: classify before probing. Any non-Source
        // verdict short-circuits — there is no per-folder snapshot
        // probe and no `No index found` line.
        match logical.classify(folder) {
            Classification::Source => {
                let snapshot = folder.join(".sqry").join("graph").join("snapshot.sqry");
                if !snapshot.exists() {
                    log.push(format!("No index found at {}", folder.display()));
                }
            }
            Classification::Member { reason } => {
                // Member folders are explicitly part of the workspace
                // but not auto-indexed. The probe MUST short-circuit
                // here. We log a debug-level marker (NOT an error
                // line) so the test can still see the loop visited
                // this folder.
                log.push(format!(
                    "[debug] member-folder skip {} (reason={:?})",
                    folder.display(),
                    reason
                ));
            }
            Classification::Excluded => {
                log.push(format!("[debug] excluded skip {}", folder.display()));
            }
            Classification::Unknown => {
                // Outside the workspace — should not happen in this
                // test; emit a marker if it does so the test can
                // catch the regression.
                log.push(format!("[debug] unknown skip {}", folder.display()));
            }
        }
    }
    log
}

#[test]
fn operational_folder_classifies_as_member_not_source() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    match fixture.logical.classify(&fixture.member) {
        Classification::Member { .. } => {}
        other => panic!(
            "operational folder must classify as Member; got {:?}. \
             This is the original bug class — Member-folder paths must \
             never be misclassified as Source, otherwise the per-folder \
             snapshot probe runs and emits `No index found` once per \
             workspace open.",
            other
        ),
    }
}

#[test]
fn zero_no_index_found_lines_for_member_folder() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    let folders = [
        fixture.source_a.as_path(),
        fixture.source_b.as_path(),
        fixture.member.as_path(),
        fixture.excluded.as_path(),
    ];
    let log = simulated_probe_loop(&fixture.logical, &folders);

    let member_no_index_lines: Vec<&String> = log
        .iter()
        .filter(|line| {
            line.starts_with("No index found at ")
                && line.contains(fixture.member.to_str().unwrap())
        })
        .collect();

    assert!(
        member_no_index_lines.is_empty(),
        "regression: simulated probe loop emitted `No index found` for the operational \
         member folder. Lines:\n{}",
        member_no_index_lines
            .iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Also assert NO "No index found" for the excluded folder.
    let excluded_no_index_lines: Vec<&String> = log
        .iter()
        .filter(|line| {
            line.starts_with("No index found at ")
                && line.contains(fixture.excluded.to_str().unwrap())
        })
        .collect();
    assert!(
        excluded_no_index_lines.is_empty(),
        "regression: simulated probe loop emitted `No index found` for the excluded \
         folder. Lines:\n{}",
        excluded_no_index_lines
            .iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn aggregate_no_index_lines_match_source_root_count_when_unindexed() {
    let fixture = build_two_source_one_member_one_excluded().expect("fixture");
    let folders = [
        fixture.source_a.as_path(),
        fixture.source_b.as_path(),
        fixture.member.as_path(),
        fixture.excluded.as_path(),
    ];
    let log = simulated_probe_loop(&fixture.logical, &folders);

    let source_no_index_lines: Vec<&String> = log
        .iter()
        .filter(|line| line.starts_with("No index found at "))
        .collect();

    // Two source roots, neither has a snapshot file → exactly two
    // legitimate "No index found" prompts. This is the expected
    // user-facing surface (the user has not run `sqry index` yet).
    assert_eq!(
        source_no_index_lines.len(),
        2,
        "expected exactly 2 `No index found` lines (one per source root); got {}.\n\
         Full log:\n{}",
        source_no_index_lines.len(),
        log.iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
