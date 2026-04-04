mod common;

use anyhow::{Context, Result};
use common::sqry_bin;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn fixtures_root() -> PathBuf {
    repo_root().join("tests/fixtures")
}

fn fixture_output_path(name: &str) -> PathBuf {
    fixtures_root().join("query").join(name)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&path, &target)?;
        } else if file_type.is_file() {
            // Normalize CRLF → LF so tree-sitter produces identical byte offsets
            // on Windows (where git may check out files with \r\n).
            let content = std::fs::read(&path)?;
            if content.contains(&b'\r') {
                let normalized: Vec<u8> = content.into_iter().filter(|&b| b != b'\r').collect();
                std::fs::write(&target, normalized)?;
            } else {
                std::fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
}

fn build_graph_snapshot(root: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
    use sqry_core::graph::unified::persistence::{
        BuildProvenance, ConfigProvenance, GraphStorage, MANIFEST_SCHEMA_VERSION, Manifest,
        SNAPSHOT_FORMAT_VERSION, save_to_path_with_provenance,
    };
    use sqry_plugin_registry::create_plugin_manager;
    use std::collections::HashMap;

    let storage = GraphStorage::new(root);
    // Note: Always build fresh (tempdir is always clean)
    // Removed early return check to avoid reusing stale snapshots

    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    let graph = build_unified_graph(root, &plugins, &config)?;
    std::fs::create_dir_all(storage.graph_dir())?;

    // Save graph snapshot with provenance (matches sqry index behavior)
    let provenance = ConfigProvenance::new(
        root.join(".sqry/graph/config/config.json"),
        "test".to_string(),
        1,
    );

    save_to_path_with_provenance(&graph, storage.snapshot_path(), provenance, &plugins)?;

    // Create and save manifest (matches sqry index behavior)
    let snapshot = graph.snapshot();
    let snapshot_sha256 = {
        let content = std::fs::read(storage.snapshot_path())?;
        let hash = Sha256::digest(&content);
        hex::encode(hash)
    };

    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        built_at: chrono::Utc::now().to_rfc3339(),
        root_path: root.to_string_lossy().to_string(),
        node_count: snapshot.nodes().len(),
        edge_count: graph.edge_count(),
        raw_edge_count: None,
        snapshot_sha256,
        build_provenance: BuildProvenance {
            sqry_version: env!("CARGO_PKG_VERSION").to_string(),
            build_timestamp: chrono::Utc::now().to_rfc3339(),
            build_command: "test".to_string(),
            plugin_hashes: HashMap::default(),
        },
        file_count: HashMap::new(),
        languages: Vec::default(),
        config: HashMap::default(),
        confidence: graph.confidence().clone(),
        last_indexed_commit: None,
        plugin_selection: None,
    };

    manifest.save(storage.manifest_path())?;
    Ok(())
}

fn normalize_value(value: &mut Value, path_suffix: &str) {
    match value {
        Value::Object(map) => {
            map.remove("execution_time_ms");
            map.remove("index_age_seconds");
            for entry in map.values_mut() {
                normalize_value(entry, path_suffix);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_value(item, path_suffix);
            }
        }
        Value::String(text) => {
            // Strip carriage returns so symbol names containing newlines match
            // across platforms (Windows tree-sitter may embed \r\n in names).
            if text.contains('\r') {
                *text = text.replace('\r', "");
            }
            // Normalize Windows backslashes so path_suffix (forward slashes) matches.
            let normalized = text.replace('\\', "/");
            if let Some(pos) = normalized.find(path_suffix) {
                let suffix = &normalized[pos..];
                let suffix = suffix.trim_start_matches('/');
                *text = format!("<WORKSPACE_ROOT>/{suffix}");
            }
        }
        _ => {}
    }
}

fn load_expected(name: &str, path_suffix: &str) -> Result<Value> {
    let content = std::fs::read_to_string(fixture_output_path(name))?;
    let mut value: Value = serde_json::from_str(&content)?;
    normalize_value(&mut value, path_suffix);
    Ok(value)
}

fn run_query_fixture(query: &str, relative_fixture: &str) -> Result<Value> {
    let temp_root = tempfile::tempdir()?;
    let temp_fixture_root = temp_root.path().join(relative_fixture);
    let source_fixture_root = repo_root().join(relative_fixture);

    copy_dir_all(&source_fixture_root, &temp_fixture_root).with_context(|| {
        format!(
            "copy fixture from {} to {}",
            source_fixture_root.display(),
            temp_fixture_root.display()
        )
    })?;

    build_graph_snapshot(&temp_fixture_root)?;

    let output = Command::new(sqry_bin())
        .args([
            "--json",
            "--sort",
            "name",
            "query",
            query,
            temp_fixture_root.to_string_lossy().as_ref(),
        ])
        .output()
        .context("execute sqry query")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "sqry query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut value: Value = serde_json::from_slice(&output.stdout)?;
    normalize_value(&mut value, relative_fixture);
    Ok(value)
}

#[test]
fn query_baseline_single_result() -> Result<()> {
    let expected = load_expected(
        "expected_query_output.json",
        "tests/fixtures/polyglot_micro",
    )?;
    let actual = run_query_fixture("name:hello_rust", "tests/fixtures/polyglot_micro")?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_multi_result() -> Result<()> {
    let expected = load_expected(
        "expected_query_output_multi.json",
        "tests/fixtures/relations/typescript",
    )?;
    let actual = run_query_fixture("kind:function", "tests/fixtures/relations/typescript")?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_cross_language() -> Result<()> {
    let expected = load_expected(
        "expected_query_output_cross_language.json",
        "tests/fixtures/polyglot_micro",
    )?;
    let actual = run_query_fixture("kind:function", "tests/fixtures/polyglot_micro")?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_empty_result() -> Result<()> {
    let expected = load_expected(
        "expected_query_output_empty.json",
        "tests/fixtures/polyglot_micro",
    )?;
    let actual = run_query_fixture(
        "kind:function AND name:does_not_exist",
        "tests/fixtures/polyglot_micro",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_metadata_rich() -> Result<()> {
    let expected = load_expected(
        "expected_query_output_metadata.json",
        "tests/fixtures/metadata_consistency/async_functions",
    )?;
    let actual = run_query_fixture(
        "kind:function",
        "tests/fixtures/metadata_consistency/async_functions",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}
