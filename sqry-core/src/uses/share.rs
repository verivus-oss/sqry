//! Share feature — generate anonymous usage snapshots for community sharing
//!
//! Provides three public functions:
//! - [`generate_share_snapshot`] — single week, by ISO week string
//! - [`generate_current_share_snapshot`] — single week, current week
//! - [`format_share_preview`] — human-readable preview for `--dry-run`
//!
//! All output types (`ShareSnapshot`) contain only validated string newtypes,
//! C-like enums, and numerics — no free-form `String` or `PathBuf` fields.
//! Privacy is structural: it is impossible to accidentally include PII.

use super::{
    DiagnosticsAggregator, GraphAbandonRate, GraphKind, ShareSnapshot, SqryVersion, TopWorkflow,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;

/// Serialize a C-like enum value to its `snake_case` JSON string.
///
/// All sqry enums use `#[serde(rename_all = "snake_case")]` so serde produces
/// the canonical user-facing name (e.g. `CallChain` → `"call_chain"`).
/// Falls back to the Rust debug name if serialization fails.
fn enum_name<T: Serialize>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// Generate a share snapshot for a specific ISO week.
///
/// Calls the aggregator to load or compute the weekly summary, then converts
/// it to a [`ShareSnapshot`].  Empty weeks (no events) return a valid snapshot
/// with `total_uses: 0` — not an error.
///
/// # Errors
///
/// Returns an error if `week` is not a valid ISO week string (e.g. `"2026-W09"`)
/// or if event data cannot be read from disk.
pub fn generate_share_snapshot(
    aggregator: &DiagnosticsAggregator,
    week: &str,
) -> Result<ShareSnapshot> {
    let summary = aggregator
        .get_or_generate_summary(week)
        .with_context(|| format!("Failed to generate summary for week {week}"))?;
    Ok(ShareSnapshot::from_summary(&summary))
}

/// Generate a share snapshot for the current ISO week.
///
/// Equivalent to calling [`generate_share_snapshot`] with the current week
/// string.  Empty weeks return a valid snapshot with `total_uses: 0`.
///
/// # Errors
///
/// Returns an error if event data cannot be read from disk.
pub fn generate_current_share_snapshot(
    aggregator: &DiagnosticsAggregator,
) -> Result<ShareSnapshot> {
    let summary = aggregator
        .summarize_current_week()
        .context("Failed to generate current week summary")?;
    Ok(ShareSnapshot::from_summary(&summary))
}

/// Merge multiple weekly snapshots into a single aggregated snapshot.
///
/// Counts (`total_uses`, `dropped_events`) are **summed**.
/// Rates (`abandon_rate`, `ai_requery_rate`, timing metrics) are
/// **weighted means** using `total_uses` as the weight for each snapshot.
/// `top_workflows` are merged by `QueryKind` with counts summed, then
/// re-sorted in descending order.
/// `abandonment` rates are merged per `GraphKind` using weighted means;
/// kinds absent from a snapshot contribute 0.0 to the weighted sum.
/// `sqry_version` is set to the latest (highest semver) across all snapshots.
/// `period` retains the **first** snapshot's `IsoWeekPeriod`.
/// `merged_period` is `Some("{first}..{last}")` for multi-week input and
/// `None` for single-snapshot input.
///
/// Empty weeks (`total_uses == 0`) are fully supported — they contribute 0
/// to all weighted sums without causing division-by-zero (the weight is 0).
///
/// # Errors
///
/// Returns an error if `snapshots` is empty.
pub fn merge_snapshots(snapshots: &[ShareSnapshot]) -> Result<ShareSnapshot> {
    anyhow::ensure!(
        !snapshots.is_empty(),
        "merge_snapshots: no snapshots to merge"
    );

    if snapshots.len() == 1 {
        // Single-snapshot input: clone and explicitly reset merged_period to None.
        // The caller may pass a previously-merged snapshot; we must not preserve
        // its merged_period, since a 1-element call is definitionally single-week.
        let mut result = snapshots[0].clone();
        result.merged_period = None;
        return Ok(result);
    }

    let total_uses: usize = snapshots.iter().map(|s| s.total_uses).sum();
    let dropped_events: u64 = snapshots.iter().map(|s| s.dropped_events).sum();

    // Weighted mean helper: weight = snapshot.total_uses.
    // When total_uses == 0 the snapshot contributes nothing to the mean.
    let weight_sum = total_uses as f64;
    let weighted_mean = |field: fn(&ShareSnapshot) -> f64| -> f64 {
        if weight_sum == 0.0 {
            return 0.0;
        }
        snapshots
            .iter()
            .map(|s| field(s) * s.total_uses as f64)
            .sum::<f64>()
            / weight_sum
    };

    let avg_time_to_result_sec = weighted_mean(|s| s.avg_time_to_result_sec);
    let median_time_to_result_sec = weighted_mean(|s| s.median_time_to_result_sec);
    let abandon_rate = weighted_mean(|s| s.abandon_rate);
    let ai_requery_rate = weighted_mean(|s| s.ai_requery_rate);

    // Merge top_workflows by QueryKind → sum counts, re-sort descending
    let mut workflow_counts: HashMap<crate::uses::QueryKind, usize> = HashMap::new();
    for snapshot in snapshots {
        for wf in &snapshot.top_workflows {
            *workflow_counts.entry(wf.kind).or_insert(0) += wf.count;
        }
    }
    let mut top_workflows: Vec<TopWorkflow> = workflow_counts
        .into_iter()
        .map(|(kind, count)| TopWorkflow { kind, count })
        .collect();
    top_workflows.sort_by(|a, b| b.count.cmp(&a.count));

    // Merge abandonment by GraphKind → weighted mean per kind
    let all_graph_kinds: std::collections::HashSet<GraphKind> = snapshots
        .iter()
        .flat_map(|s| s.abandonment.iter().map(|a| a.kind))
        .collect();
    let mut abandonment: Vec<GraphAbandonRate> = all_graph_kinds
        .into_iter()
        .map(|kind| {
            let rate = if weight_sum == 0.0 {
                0.0
            } else {
                snapshots
                    .iter()
                    .map(|s| {
                        let r = s
                            .abandonment
                            .iter()
                            .find(|a| a.kind == kind)
                            .map_or(0.0, |a| a.rate);
                        r * s.total_uses as f64
                    })
                    .sum::<f64>()
                    / weight_sum
            };
            GraphAbandonRate { kind, rate }
        })
        .collect();
    abandonment.sort_by_key(|a| format!("{:?}", a.kind));

    // sqry_version: latest by semver
    let sqry_version = snapshots
        .iter()
        .max_by(|a, b| {
            let va = semver::Version::parse(a.sqry_version.as_str())
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
            let vb = semver::Version::parse(b.sqry_version.as_str())
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
            va.cmp(&vb)
        })
        .map(|s| s.sqry_version.clone())
        .unwrap_or_else(SqryVersion::current);

    // period: first snapshot; merged_period: "first..last"
    let first = snapshots.first().unwrap();
    let last = snapshots.last().unwrap();
    let merged_period = Some(format!(
        "{}..{}",
        first.period.as_str(),
        last.period.as_str()
    ));

    Ok(ShareSnapshot {
        sqry_version,
        period: first.period.clone(),
        top_workflows,
        avg_time_to_result_sec,
        median_time_to_result_sec,
        abandon_rate,
        abandonment,
        ai_requery_rate,
        total_uses,
        dropped_events,
        merged_period,
    })
}

/// Generate a human-readable preview of what would be shared.
///
/// Renders a compact, formatted summary of the snapshot contents.
/// Intended for `--dry-run` output so the user can inspect before writing.
#[must_use]
pub fn format_share_preview(snapshot: &ShareSnapshot) -> String {
    let mut lines = Vec::new();

    let period_display = snapshot
        .merged_period
        .as_deref()
        .unwrap_or(snapshot.period.as_str());
    lines.push(format!("Share Preview for {period_display}"));
    lines.push("=".repeat(32));
    lines.push(String::new());

    lines.push(format!("Total uses: {}", snapshot.total_uses));
    if snapshot.dropped_events > 0 {
        lines.push(format!("Dropped events: {}", snapshot.dropped_events));
    }

    if !snapshot.top_workflows.is_empty() {
        lines.push(String::new());
        lines.push("Top workflows:".to_string());
        for workflow in &snapshot.top_workflows {
            lines.push(format!(
                "  {}: {}",
                enum_name(&workflow.kind),
                workflow.count
            ));
        }
    }

    lines.push(String::new());
    lines.push("Timing:".to_string());
    lines.push(format!(
        "  Average: {:.2}s",
        snapshot.avg_time_to_result_sec
    ));
    lines.push(format!(
        "  Median:  {:.2}s",
        snapshot.median_time_to_result_sec
    ));

    lines.push(String::new());
    lines.push(format!(
        "Abandonment rate: {:.1}%",
        snapshot.abandon_rate * 100.0
    ));
    lines.push(format!(
        "AI requery rate:  {:.1}%",
        snapshot.ai_requery_rate * 100.0
    ));

    if !snapshot.abandonment.is_empty() {
        lines.push(String::new());
        lines.push("Abandonment by graph type:".to_string());
        for entry in &snapshot.abandonment {
            lines.push(format!(
                "  {}: {:.1}%",
                enum_name(&entry.kind),
                entry.rate * 100.0
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uses::{
        DiagnosticsSummary, GraphAbandonRate, GraphKind, IsoWeekPeriod, QueryKind, TopWorkflow,
    };
    use proptest::prelude::*;
    use tempfile::TempDir;

    // =========================================================================
    // Privacy assertions
    // =========================================================================

    /// Asserts that a JSON string contains no PII patterns.
    fn assert_no_pii(json: &str, label: &str) {
        let patterns = [
            "/home/", "/Users/", "C:\\", "/srv/", "/var/", "/tmp/", "~\\", "fn ", "class ", "def ",
            "impl ",
        ];
        for pattern in &patterns {
            assert!(
                !json.contains(pattern),
                "{label}: JSON contains PII pattern {:?}\nJSON: {json}",
                pattern
            );
        }
    }

    // =========================================================================
    // Proptest strategies
    // =========================================================================

    prop_compose! {
        /// Arbitrary valid ISO week string (years 2020-2030, weeks 01-53).
        ///
        /// `IsoWeekPeriod` accepts W01–W53 per its regex.  Not every year has
        /// a W53 in the real calendar, but the type does not enforce calendar
        /// validity beyond the regex; W53 is thus a valid (if rare) test case.
        fn arb_iso_week_str()(
            year in 2020_i32..=2030_i32,
            week in 1_u32..=53_u32,
        ) -> String {
            format!("{year}-W{week:02}")
        }
    }

    prop_compose! {
        fn arb_query_kind()(idx in 0_usize..7) -> QueryKind {
            match idx {
                0 => QueryKind::CallChain,
                1 => QueryKind::ImpactAnalysis,
                2 => QueryKind::SymbolLookup,
                3 => QueryKind::Semantic,
                4 => QueryKind::Unused,
                5 => QueryKind::Duplicates,
                _ => QueryKind::Circular,
            }
        }
    }

    prop_compose! {
        fn arb_graph_kind()(idx in 0_usize..3) -> GraphKind {
            match idx {
                0 => GraphKind::CallGraph,
                1 => GraphKind::DependencyGraph,
                _ => GraphKind::ImportGraph,
            }
        }
    }

    prop_compose! {
        fn arb_top_workflow()(
            kind in arb_query_kind(),
            count in 0_usize..1000_usize,
        ) -> TopWorkflow {
            TopWorkflow { kind, count }
        }
    }

    prop_compose! {
        fn arb_graph_abandon_rate()(
            kind in arb_graph_kind(),
            rate in 0.0_f64..1.0_f64,
        ) -> GraphAbandonRate {
            GraphAbandonRate { kind, rate }
        }
    }

    prop_compose! {
        fn arb_summary()(
            week_str in arb_iso_week_str(),
            total_uses in 0_usize..5000_usize,
            dropped_events in 0_u64..100_u64,
            avg_time in 0.0_f64..30.0_f64,
            median_time in 0.0_f64..30.0_f64,
            abandon_rate in 0.0_f64..1.0_f64,
            ai_requery_rate in 0.0_f64..1.0_f64,
            workflows in proptest::collection::vec(arb_top_workflow(), 0..=5),
            abandonment in proptest::collection::vec(arb_graph_abandon_rate(), 0..=3),
        ) -> DiagnosticsSummary {
            DiagnosticsSummary {
                period: IsoWeekPeriod::try_new(&week_str)
                    .unwrap_or_else(|_| IsoWeekPeriod::try_new("2026-W09").unwrap()),
                top_workflows: workflows,
                avg_time_to_result_sec: avg_time,
                median_time_to_result_sec: median_time,
                abandon_rate,
                abandonment,
                ai_requery_rate,
                total_uses,
                dropped_events,
            }
        }
    }

    fn make_aggregator(tmp: &TempDir) -> DiagnosticsAggregator {
        DiagnosticsAggregator::new(tmp.path())
    }

    fn make_summary(week: &str) -> DiagnosticsSummary {
        DiagnosticsSummary {
            period: IsoWeekPeriod::try_new(week).unwrap(),
            top_workflows: vec![
                TopWorkflow {
                    kind: QueryKind::CallChain,
                    count: 15,
                },
                TopWorkflow {
                    kind: QueryKind::Semantic,
                    count: 8,
                },
            ],
            avg_time_to_result_sec: 1.5,
            median_time_to_result_sec: 1.2,
            abandon_rate: 0.10,
            abandonment: vec![GraphAbandonRate {
                kind: GraphKind::CallGraph,
                rate: 0.07,
            }],
            ai_requery_rate: 0.05,
            total_uses: 23,
            dropped_events: 0,
        }
    }

    #[test]
    fn test_generate_current_share_snapshot_empty_week() {
        let tmp = TempDir::new().unwrap();
        let aggregator = make_aggregator(&tmp);
        // No events recorded → valid snapshot with total_uses == 0
        let snapshot = generate_current_share_snapshot(&aggregator).unwrap();
        assert_eq!(snapshot.total_uses, 0);
    }

    #[test]
    fn test_generate_share_snapshot_specific_week_empty() {
        let tmp = TempDir::new().unwrap();
        let aggregator = make_aggregator(&tmp);
        let snapshot = generate_share_snapshot(&aggregator, "2026-W01").unwrap();
        assert_eq!(snapshot.period.as_str(), "2026-W01");
        assert_eq!(snapshot.total_uses, 0);
    }

    #[test]
    fn test_generate_share_snapshot_invalid_week() {
        let tmp = TempDir::new().unwrap();
        let aggregator = make_aggregator(&tmp);
        let result = generate_share_snapshot(&aggregator, "not-a-week");
        assert!(result.is_err());
    }

    #[test]
    fn test_format_share_preview_contains_key_fields() {
        let summary = make_summary("2026-W09");
        let snapshot = ShareSnapshot::from_summary(&summary);
        let preview = format_share_preview(&snapshot);

        assert!(preview.contains("2026-W09"), "preview must show period");
        assert!(
            preview.contains("Total uses: 23"),
            "preview must show total uses"
        );
        assert!(
            preview.contains("Average: 1.50s"),
            "preview must show avg timing"
        );
        assert!(
            preview.contains("Median:  1.20s"),
            "preview must show median timing"
        );
        assert!(
            preview.contains("Abandonment rate: 10.0%"),
            "preview must show abandonment rate"
        );
        assert!(
            preview.contains("AI requery rate:  5.0%"),
            "preview must show AI requery rate"
        );
        // Enums must be rendered as snake_case (matching JSON schema), not Rust Debug names
        assert!(
            preview.contains("call_chain"),
            "enum names must be snake_case, got: {preview}"
        );
        assert!(
            !preview.contains("CallChain"),
            "preview must not use Rust Debug enum names"
        );
    }

    #[test]
    fn test_merge_snapshots_single_returns_clone() {
        let summary = make_summary("2026-W09");
        let snapshot = ShareSnapshot::from_summary(&summary);
        let merged = merge_snapshots(&[snapshot.clone()]).unwrap();
        // Single-snapshot merge: merged_period must be None
        assert_eq!(merged.merged_period, None);
        assert_eq!(merged.total_uses, snapshot.total_uses);
    }

    #[test]
    fn test_merge_snapshots_single_resets_merged_period_from_prior_merge() {
        // A previously-merged snapshot (merged_period = Some(...)) passed as
        // single element must have merged_period reset to None.
        let mut summary_a = make_summary("2026-W07");
        summary_a.total_uses = 5;
        let mut summary_b = make_summary("2026-W09");
        summary_b.total_uses = 5;
        let already_merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();
        assert_eq!(
            already_merged.merged_period,
            Some("2026-W07..2026-W09".to_string())
        );

        // Pass the merged snapshot as single-element input
        let re_merged = merge_snapshots(&[already_merged]).unwrap();
        assert_eq!(
            re_merged.merged_period, None,
            "single-element merge must reset merged_period even when input had Some"
        );
    }

    #[test]
    fn test_merge_snapshots_empty_returns_error() {
        let result = merge_snapshots(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_snapshots_two_weeks_weighted_averages() {
        // Week A: 10 uses, abandon_rate 0.10, avg_time 1.0s
        let mut summary_a = make_summary("2026-W07");
        summary_a.total_uses = 10;
        summary_a.abandon_rate = 0.10;
        summary_a.avg_time_to_result_sec = 1.0;
        summary_a.median_time_to_result_sec = 0.8;
        summary_a.ai_requery_rate = 0.20;

        // Week B: 30 uses, abandon_rate 0.30, avg_time 3.0s
        let mut summary_b = make_summary("2026-W08");
        summary_b.total_uses = 30;
        summary_b.abandon_rate = 0.30;
        summary_b.avg_time_to_result_sec = 3.0;
        summary_b.median_time_to_result_sec = 2.4;
        summary_b.ai_requery_rate = 0.60;

        let snap_a = ShareSnapshot::from_summary(&summary_a);
        let snap_b = ShareSnapshot::from_summary(&summary_b);
        let merged = merge_snapshots(&[snap_a, snap_b]).unwrap();

        // total_uses: 10 + 30 = 40
        assert_eq!(merged.total_uses, 40);

        // abandon_rate weighted: (10*0.10 + 30*0.30) / 40 = (1.0 + 9.0) / 40 = 0.25
        let expected_abandon = (10.0 * 0.10 + 30.0 * 0.30) / 40.0;
        assert!(
            (merged.abandon_rate - expected_abandon).abs() < 1e-10,
            "abandon_rate mismatch: {} vs {}",
            merged.abandon_rate,
            expected_abandon
        );

        // avg_time weighted: (10*1.0 + 30*3.0) / 40 = 100 / 40 = 2.5
        let expected_avg = (10.0 * 1.0 + 30.0 * 3.0) / 40.0;
        assert!(
            (merged.avg_time_to_result_sec - expected_avg).abs() < 1e-10,
            "avg_time mismatch: {} vs {}",
            merged.avg_time_to_result_sec,
            expected_avg
        );

        // merged_period: "2026-W07..2026-W08"
        assert_eq!(merged.merged_period, Some("2026-W07..2026-W08".to_string()));
        // period: first week
        assert_eq!(merged.period.as_str(), "2026-W07");
    }

    #[test]
    fn test_merge_snapshots_top_workflows_summed() {
        let mut summary_a = make_summary("2026-W07");
        summary_a.top_workflows = vec![
            TopWorkflow {
                kind: QueryKind::CallChain,
                count: 10,
            },
            TopWorkflow {
                kind: QueryKind::Semantic,
                count: 5,
            },
        ];
        let mut summary_b = make_summary("2026-W08");
        summary_b.top_workflows = vec![
            TopWorkflow {
                kind: QueryKind::CallChain,
                count: 20,
            },
            TopWorkflow {
                kind: QueryKind::Unused,
                count: 3,
            },
        ];

        let merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();

        let call_chain = merged
            .top_workflows
            .iter()
            .find(|w| w.kind == QueryKind::CallChain)
            .unwrap();
        let semantic = merged
            .top_workflows
            .iter()
            .find(|w| w.kind == QueryKind::Semantic)
            .unwrap();
        let unused = merged
            .top_workflows
            .iter()
            .find(|w| w.kind == QueryKind::Unused)
            .unwrap();

        assert_eq!(call_chain.count, 30, "CallChain counts should sum");
        assert_eq!(semantic.count, 5, "Semantic only in A");
        assert_eq!(unused.count, 3, "Unused only in B");
        // Sorted descending: CallChain(30) > Semantic(5) > Unused(3)
        assert_eq!(merged.top_workflows[0].kind, QueryKind::CallChain);
    }

    #[test]
    fn test_merge_snapshots_zero_event_weeks() {
        // Both weeks have zero events — should produce valid zero-valued snapshot
        let mut summary_a = make_summary("2026-W07");
        summary_a.total_uses = 0;
        summary_a.abandon_rate = 0.0;
        summary_a.avg_time_to_result_sec = 0.0;
        let mut summary_b = make_summary("2026-W08");
        summary_b.total_uses = 0;
        summary_b.abandon_rate = 0.0;
        summary_b.avg_time_to_result_sec = 0.0;

        let merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();

        assert_eq!(merged.total_uses, 0);
        assert_eq!(merged.abandon_rate, 0.0);
        assert_eq!(merged.avg_time_to_result_sec, 0.0);
    }

    #[test]
    fn test_merge_snapshots_merged_period_none_for_single() {
        let summary = make_summary("2026-W09");
        let snapshot = ShareSnapshot::from_summary(&summary);
        let merged = merge_snapshots(&[snapshot]).unwrap();
        assert_eq!(merged.merged_period, None);
    }

    #[test]
    fn test_format_share_preview_shows_merged_period() {
        let mut summary_a = make_summary("2026-W07");
        summary_a.total_uses = 5;
        let mut summary_b = make_summary("2026-W09");
        summary_b.total_uses = 5;
        let merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();
        let preview = format_share_preview(&merged);
        assert!(
            preview.contains("2026-W07..2026-W09"),
            "preview should show merged period range, got: {preview}"
        );
    }

    #[test]
    fn test_format_share_preview_dropped_events_only_when_nonzero() {
        let mut summary = make_summary("2026-W09");
        summary.dropped_events = 0;
        let preview = format_share_preview(&ShareSnapshot::from_summary(&summary));
        assert!(
            !preview.contains("Dropped"),
            "should not show dropped events when zero"
        );

        let mut summary2 = make_summary("2026-W09");
        summary2.dropped_events = 3;
        let preview2 = format_share_preview(&ShareSnapshot::from_summary(&summary2));
        assert!(
            preview2.contains("Dropped events: 3"),
            "should show dropped events when nonzero"
        );
    }

    // =========================================================================
    // Privacy tests
    // =========================================================================

    #[test]
    fn test_no_pii_in_serialized_share_snapshot() {
        let summary = make_summary("2026-W09");
        let snapshot = ShareSnapshot::from_summary(&summary);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_no_pii(&json, "single-week snapshot");
    }

    #[test]
    fn test_share_snapshot_roundtrip() {
        let summary = make_summary("2026-W09");
        let original = ShareSnapshot::from_summary(&summary);
        let json = serde_json::to_string_pretty(&original).unwrap();
        let restored: ShareSnapshot = serde_json::from_str(&json).unwrap();
        // Verify structural fields are identical
        assert_eq!(original.period, restored.period);
        assert_eq!(original.sqry_version, restored.sqry_version);
        assert_eq!(original.total_uses, restored.total_uses);
        assert_eq!(original.dropped_events, restored.dropped_events);
        assert_eq!(original.top_workflows, restored.top_workflows);
        assert_eq!(original.abandonment, restored.abandonment);
        assert_eq!(original.merged_period, restored.merged_period);
        // JSON must be stable: re-serializing the restored snapshot produces the same JSON
        let json2 = serde_json::to_string_pretty(&restored).unwrap();
        assert_eq!(
            json, json2,
            "JSON serialization must be stable after roundtrip"
        );
    }

    #[test]
    fn test_merge_preserves_privacy() {
        let summary_a = make_summary("2026-W07");
        let summary_b = make_summary("2026-W09");
        let merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();
        let json = serde_json::to_string(&merged).unwrap();
        assert_no_pii(&json, "merged snapshot");
    }

    #[test]
    fn test_merged_period_field_omitted_in_single_week_json() {
        let summary = make_summary("2026-W09");
        let snapshot = ShareSnapshot::from_summary(&summary);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !json.contains("merged_period"),
            "merged_period must be absent for single-week snapshot"
        );
    }

    #[test]
    fn test_merged_period_field_present_in_multi_week_json() {
        let mut summary_a = make_summary("2026-W07");
        summary_a.total_uses = 5;
        let mut summary_b = make_summary("2026-W09");
        summary_b.total_uses = 5;
        let merged = merge_snapshots(&[
            ShareSnapshot::from_summary(&summary_a),
            ShareSnapshot::from_summary(&summary_b),
        ])
        .unwrap();
        let json = serde_json::to_string(&merged).unwrap();
        assert!(
            json.contains("merged_period"),
            "merged_period must be present for merged snapshot"
        );
        assert!(
            json.contains("2026-W07..2026-W09"),
            "merged_period must contain the correct range"
        );
    }

    // =========================================================================
    // Property tests (256 iterations)
    // =========================================================================

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]

        #[test]
        fn prop_no_pii_in_serialized_snapshot(summary in arb_summary()) {
            let snapshot = ShareSnapshot::from_summary(&summary);
            let json = serde_json::to_string(&snapshot).unwrap();
            assert_no_pii(&json, "proptest snapshot");
        }

        #[test]
        fn prop_share_snapshot_roundtrip(summary in arb_summary()) {
            let original = ShareSnapshot::from_summary(&summary);

            // Encode → decode → re-encode
            let json1 = serde_json::to_string(&original).unwrap();
            let r1: ShareSnapshot = serde_json::from_str(&json1).unwrap();

            // Non-float fields must be preserved exactly from original → r1
            prop_assert_eq!(&original.period, &r1.period,
                "period must be preserved through roundtrip");
            prop_assert_eq!(&original.sqry_version, &r1.sqry_version,
                "sqry_version must be preserved");
            prop_assert_eq!(original.total_uses, r1.total_uses,
                "total_uses must be preserved");
            prop_assert_eq!(original.dropped_events, r1.dropped_events,
                "dropped_events must be preserved");
            prop_assert_eq!(&original.top_workflows, &r1.top_workflows,
                "top_workflows must be preserved");
            prop_assert_eq!(&original.merged_period, &r1.merged_period,
                "merged_period must be preserved");

            // JSON stability: re-encode must be stable after the first cycle.
            // (serde_json may adjust the last ULP of f64 values on the first
            //  decode, but must be stable thereafter.)
            let json2 = serde_json::to_string(&r1).unwrap();
            let r2: ShareSnapshot = serde_json::from_str(&json2).unwrap();
            let json3 = serde_json::to_string(&r2).unwrap();
            prop_assert_eq!(json2, json3, "JSON must be stable after first encode/decode cycle");
        }
    }
}
