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

/// When `SQRY_UPDATE_GOLDENS=1` is set, overwrite the on-disk golden with the
/// actual (normalized) output. Used to re-baseline expected fixtures after
/// intentional engine behavior changes (e.g. Phase 4c-prime call-site position
/// improvements). The updated golden is then compared against `actual` so the
/// test still asserts the file round-trips cleanly.
fn maybe_update_golden(name: &str, actual: &Value) -> Result<()> {
    if std::env::var("SQRY_UPDATE_GOLDENS").ok().as_deref() == Some("1") {
        let path = fixture_output_path(name);
        let serialized = serde_json::to_string_pretty(actual)?;
        std::fs::write(&path, serialized + "\n")
            .with_context(|| format!("update golden fixture {}", path.display()))?;
    }
    Ok(())
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
    let actual = run_query_fixture("name:hello_rust", "tests/fixtures/polyglot_micro")?;
    maybe_update_golden("expected_query_output.json", &actual)?;
    let expected = load_expected(
        "expected_query_output.json",
        "tests/fixtures/polyglot_micro",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_multi_result() -> Result<()> {
    let actual = run_query_fixture("kind:function", "tests/fixtures/relations/typescript")?;
    maybe_update_golden("expected_query_output_multi.json", &actual)?;
    let expected = load_expected(
        "expected_query_output_multi.json",
        "tests/fixtures/relations/typescript",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_cross_language() -> Result<()> {
    let actual = run_query_fixture("kind:function", "tests/fixtures/polyglot_micro")?;
    maybe_update_golden("expected_query_output_cross_language.json", &actual)?;
    let expected = load_expected(
        "expected_query_output_cross_language.json",
        "tests/fixtures/polyglot_micro",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn query_baseline_empty_result() -> Result<()> {
    let actual = run_query_fixture(
        "kind:function AND name:does_not_exist",
        "tests/fixtures/polyglot_micro",
    )?;
    maybe_update_golden("expected_query_output_empty.json", &actual)?;
    let expected = load_expected(
        "expected_query_output_empty.json",
        "tests/fixtures/polyglot_micro",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

/// Issue #513: `sqry query --help` advertises `file:` as an alias of `path:`,
/// but `file:` used to match nothing while `path:` worked. Run both spellings
/// against the same fixture and assert they return the identical result set,
/// and that the set is non-empty (so we are not just comparing two empties).
#[test]
fn query_file_alias_matches_path() -> Result<()> {
    let via_path = run_query_fixture(
        "kind:function AND path:main.rs",
        "tests/fixtures/polyglot_micro",
    )?;
    let via_file = run_query_fixture(
        "kind:function AND file:main.rs",
        "tests/fixtures/polyglot_micro",
    )?;

    // `path:main.rs` must actually match something in the fixture, otherwise
    // the equality check below would pass vacuously against two empty sets.
    let path_results = via_path
        .get("results")
        .and_then(Value::as_array)
        .expect("results array present for path: query");
    assert!(
        !path_results.is_empty(),
        "path:main.rs should match at least one Rust function in the fixture"
    );

    // The alias must be a true alias: identical result sets and stats. Only
    // the echoed `query.pattern` field legitimately differs (it reflects the
    // raw input spelling), so compare the parts that describe the matches.
    assert_eq!(
        via_file.get("results"),
        via_path.get("results"),
        "file:main.rs and path:main.rs must return the same results"
    );
    assert_eq!(
        via_file.get("stats"),
        via_path.get("stats"),
        "file:main.rs and path:main.rs must report the same match stats"
    );

    Ok(())
}

/// Issue #513 (subquery regression): the alias must also hold inside a relation
/// subquery. `callers:(file:main.rs)` must return the same results as
/// `callers:(path:main.rs)`. Before the fix the nested `file:` field survived
/// unresolved and the relation matched nothing.
#[test]
fn query_file_alias_matches_path_in_subquery() -> Result<()> {
    let via_path = run_query_fixture("callers:(path:main.rs)", "tests/fixtures/polyglot_micro")?;
    let via_file = run_query_fixture("callers:(file:main.rs)", "tests/fixtures/polyglot_micro")?;

    // The `path:` subquery must match at least one caller, otherwise the
    // equality below would pass vacuously.
    let path_results = via_path
        .get("results")
        .and_then(Value::as_array)
        .expect("results array present for callers:(path:...) query");
    assert!(
        !path_results.is_empty(),
        "callers:(path:main.rs) should match at least one caller in the fixture"
    );

    assert_eq!(
        via_file.get("results"),
        via_path.get("results"),
        "callers:(file:main.rs) and callers:(path:main.rs) must return the same results"
    );
    assert_eq!(
        via_file.get("stats"),
        via_path.get("stats"),
        "callers:(file:main.rs) and callers:(path:main.rs) must report the same stats"
    );

    Ok(())
}

#[test]
fn query_baseline_metadata_rich() -> Result<()> {
    let actual = run_query_fixture(
        "kind:function",
        "tests/fixtures/metadata_consistency/async_functions",
    )?;
    maybe_update_golden("expected_query_output_metadata.json", &actual)?;
    let expected = load_expected(
        "expected_query_output_metadata.json",
        "tests/fixtures/metadata_consistency/async_functions",
    )?;
    assert_eq!(actual, expected);
    Ok(())
}
