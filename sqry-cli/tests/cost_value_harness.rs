mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use assert_cmd::Command;
use serde::Serialize;
use sqry_core::graph::unified::persistence::{GraphStorage, Manifest};
use tempfile::TempDir;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Test harness flags are independent booleans
#[allow(clippy::struct_field_names)] // Count fields intentionally all suffixed with _count for clarity
struct HarnessCounts {
    node_count: usize,
    edge_count: usize,
    file_count: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct HarnessSelection {
    active_plugin_ids: Vec<String>,
    high_cost_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HarnessRun {
    wall_millis: u128,
    counts: HarnessCounts,
    selection: HarnessSelection,
}

#[derive(Debug, Clone, Serialize)]
struct HarnessDelta {
    delta_nodes: isize,
    delta_edges: isize,
    delta_file_count: HashMap<String, isize>,
    added_plugins: Vec<String>,
}

fn write_fixture_repo(root: &Path) {
    fs::write(
        root.join("main.rs"),
        "fn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("settings.json"),
        "{\n  \"service\": { \"enabled\": true, \"name\": \"sqry\" }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("record.xml"),
        r#"<record_update table="sys_script_include"><sys_script_include><name>Harness</name><script><![CDATA[function runHarness() { return true; }]]></script></sys_script_include></record_update>"#,
    )
    .unwrap();
}

fn run_index_harness(root: &Path, extra_args: &[&str]) -> HarnessRun {
    let start = Instant::now();
    let mut cmd = Command::new(common::sqry_bin());
    cmd.arg("index").arg(root).arg("--force");
    cmd.args(extra_args);
    cmd.assert().success();
    let wall_millis = start.elapsed().as_millis();

    let storage = GraphStorage::new(root);
    let manifest = Manifest::load(storage.manifest_path()).unwrap();
    let selection = manifest.plugin_selection.unwrap();

    HarnessRun {
        wall_millis,
        counts: HarnessCounts {
            node_count: manifest.node_count,
            edge_count: manifest.edge_count,
            file_count: manifest.file_count,
        },
        selection: HarnessSelection {
            active_plugin_ids: selection.active_plugin_ids,
            high_cost_mode: selection.high_cost_mode,
        },
    }
}

fn compute_delta(baseline: &HarnessRun, candidate: &HarnessRun) -> HarnessDelta {
    let mut delta_file_count = HashMap::new();
    for key in baseline
        .counts
        .file_count
        .keys()
        .chain(candidate.counts.file_count.keys())
    {
        let baseline_count = baseline.counts.file_count.get(key).copied().unwrap_or(0);
        let candidate_count = candidate.counts.file_count.get(key).copied().unwrap_or(0);
        let delta = isize::try_from(candidate_count).expect("count fits in isize")
            - isize::try_from(baseline_count).expect("count fits in isize");
        delta_file_count.insert(key.clone(), delta);
    }

    let added_plugins = candidate
        .selection
        .active_plugin_ids
        .iter()
        .filter(|plugin_id| !baseline.selection.active_plugin_ids.contains(plugin_id))
        .cloned()
        .collect();

    let delta_nodes = isize::try_from(candidate.counts.node_count).expect("count fits in isize")
        - isize::try_from(baseline.counts.node_count).expect("count fits in isize");
    let delta_edges = isize::try_from(candidate.counts.edge_count).expect("count fits in isize")
        - isize::try_from(baseline.counts.edge_count).expect("count fits in isize");
    HarnessDelta {
        delta_nodes,
        delta_edges,
        delta_file_count,
        added_plugins,
    }
}

fn render_text_summary(label: &str, run: &HarnessRun) -> String {
    format!(
        "{label}: wall={}ms nodes={} edges={} plugins={}",
        run.wall_millis,
        run.counts.node_count,
        run.counts.edge_count,
        run.selection.active_plugin_ids.join(",")
    )
}

fn render_json_summary(run: &HarnessRun) -> String {
    serde_json::to_string_pretty(run).unwrap()
}

#[test]
fn deterministic_rerun_preserves_counts_and_manifest_plugin_ids() {
    let temp_dir = TempDir::new().unwrap();
    write_fixture_repo(temp_dir.path());

    let first = run_index_harness(temp_dir.path(), &[]);
    let second = run_index_harness(temp_dir.path(), &[]);

    assert_eq!(first.counts, second.counts);
    assert_eq!(first.selection, second.selection);
    assert!(
        !first
            .selection
            .active_plugin_ids
            .iter()
            .any(|plugin_id| plugin_id == "json"),
        "fast-path default should exclude json"
    );
    assert!(
        !first.counts.file_count.contains_key("json"),
        "json files should not be counted when the plugin is excluded"
    );

    let text_summary = render_text_summary("default", &first);
    let json_summary = render_json_summary(&first);
    assert!(text_summary.contains("plugins="));
    assert!(json_summary.contains("\"active_plugin_ids\""));
}

#[test]
fn include_high_cost_changes_counts_and_persists_plugin_selection() {
    let temp_dir = TempDir::new().unwrap();
    write_fixture_repo(temp_dir.path());

    let baseline = run_index_harness(temp_dir.path(), &[]);
    let candidate = run_index_harness(temp_dir.path(), &["--include-high-cost"]);
    let delta = compute_delta(&baseline, &candidate);

    assert!(
        candidate
            .selection
            .active_plugin_ids
            .iter()
            .any(|plugin_id| plugin_id == "json"),
        "high-cost opt-in should activate json"
    );
    assert_eq!(
        candidate.selection.high_cost_mode.as_deref(),
        Some("include_all")
    );
    assert!(
        candidate.counts.file_count.contains_key("json"),
        "manifest file counts should include json after opt-in"
    );
    assert!(
        delta.delta_nodes > 0 || delta.delta_file_count.get("json").copied().unwrap_or(0) > 0,
        "opt-in should change the indexed surface for the synthetic fixture"
    );
    assert!(
        delta
            .added_plugins
            .iter()
            .any(|plugin_id| plugin_id == "json"),
        "delta should record json as an added plugin"
    );
}

#[test]
#[ignore = "Requires SQRY_COST_HARNESS_REPO for real-repo reproduction"]
fn real_repo_harness_respects_manifest_plugin_ids() {
    let repo_root = PathBuf::from(std::env::var("SQRY_COST_HARNESS_REPO").unwrap());
    let baseline = run_index_harness(&repo_root, &[]);
    let candidate = run_index_harness(&repo_root, &["--include-high-cost"]);

    assert_ne!(
        baseline.selection.active_plugin_ids,
        candidate.selection.active_plugin_ids
    );
    assert!(
        candidate
            .selection
            .active_plugin_ids
            .iter()
            .any(|plugin_id| plugin_id == "json"),
        "real-repo opt-in should persist json when enabled"
    );
}
